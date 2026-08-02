//! Thin wrapper around [`arkret::http_client::Client`] for Arkret agent traffic.
//!
//! All HTTP / retry / canonical-bytes / NDJSON line splitting logic lives in
//! the upstream SDK; this type only exists so the gateway runtime never has
//! to think about constructing the underlying client.
//!
//! Personal-agent mode uses `agent_key_proof` to mint a short-lived
//! `ak.session.grant`, then presents that grant with a fresh DPoP proof on
//! every protected self-surface call.

use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context;
use arkret::http_client::{Auth, Client, ClientBuilder, DpopAuth};
use arkret::{
    AccountSubscribeFrame, DeviceId, Did, DidUrl, Ed25519PayloadSigner, EventsSubmitOutcome,
    EventsSubscribeFrame, KeyOperationSignature, KeyPackagesClaimOutcome,
    KeyPackagesClaimRequestBody, MlsWelcomeClaimEnvelope, PreparedStandardEvent, RealmId,
    ServiceDescribe, SessionGrantDpopBindingProof, StrandId, SyncRequestBody,
};
use arkret_wire::EventInitialSubmission;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer as _, SigningKey};
use futures_util::Stream;
use garth::session::BoxSessionFuture;
use garth::{
    AgentKeyProofLogin, ArkretClient, AuthenticatedTransportFactory, FileStore, LoginKind,
    MemoryStore, NativeExecutor, NoopSessionGrantStore, SessionEngine, SessionGrantState,
    SessionGrantStore, SessionGrantTransport, SessionRefreshOptions, SessionTransportProvider,
    TransportProvider,
};
use serde::Serialize;
use sha2::Digest as _;
use url::Url;
use uuid::Uuid;

use super::session::{ArkretSession, login_with_signer};
use super::signer::{ArkretKeyRef, load_ed25519_signing_key};

const SESSION_GRANT_PATH: &str = "/_arkret/gate/account/session-grants";

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct ArkretHttpClient {
    inner: Client,
    initial_submission_provider: Arc<dyn ArkretInitialSubmissionProvider>,
}

/// Host-owned integration that obtains issuer-produced publication evidence
/// for a fully-authored, fully-signed Event.
///
/// Implementations may call a local authority component or a remote authz
/// service. They must return the exact Event supplied by Savfox inside an
/// [`EventInitialSubmission`]; Savfox validates that invariant and the wrapper
/// structure before enqueueing or sending it.
#[async_trait::async_trait]
pub trait ArkretInitialSubmissionProvider: Send + Sync + 'static {
    async fn initial_submission(
        &self,
        event: &PreparedStandardEvent,
    ) -> anyhow::Result<EventInitialSubmission>;
}

/// Production publication provider backed by the authenticated Principal
/// Server's standard authorization-lease and proposal-receipt operations.
#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct PrincipalServerInitialSubmissionProvider {
    client: Client,
}

impl PrincipalServerInitialSubmissionProvider {
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl ArkretInitialSubmissionProvider for PrincipalServerInitialSubmissionProvider {
    async fn initial_submission(
        &self,
        event: &PreparedStandardEvent,
    ) -> anyhow::Result<EventInitialSubmission> {
        self.client
            .prepare_initial_standard_submission(event)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }
}

/// Stream of [`EventsSubscribeFrame`] yielded by
/// [`ArkretHttpClient::events_subscribe_stream`].
///
/// Each item is a fully-parsed frame (transient line decode errors come
/// through as `Err` but the stream continues). Use
/// [`futures_util::StreamExt::next`] to pull frames.
pub type ArkretFrameStream =
    Pin<Box<dyn Stream<Item = Result<EventsSubscribeFrame, anyhow::Error>> + Send>>;

/// Stream of account-level subscribe frames yielded by
/// [`ArkretHttpClient::account_subscribe_stream`].
pub type ArkretAccountFrameStream =
    Pin<Box<dyn Stream<Item = Result<AccountSubscribeFrame, anyhow::Error>> + Send>>;

pub type SavfoxArkretClientCore = ArkretClient<NativeExecutor, MemoryStore, MemoryStore>;

pub type SavfoxDurableArkretClientCore = ArkretClient<NativeExecutor, FileStore, FileStore>;

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct AgentSessionGrantTransport {
    grant_base_url: Url,
    bootstrap: Client,
    dpop_signing_key: Arc<SigningKey>,
}

impl SessionGrantTransport for AgentSessionGrantTransport {
    fn issue_session_grant<'a>(
        &'a self,
        request: arkret::SessionGrantRequestBody,
    ) -> BoxSessionFuture<'a, arkret::SessionGrantOutcome> {
        Box::pin(async move {
            self.bootstrap
                .auth_issue_session_grant(&request)
                .await
                .map_err(garth::Error::from)
        })
    }

    fn refresh_session_grant<'a>(
        &'a self,
        request: arkret::SessionGrantRefreshRequestBody,
    ) -> BoxSessionFuture<'a, arkret::SessionGrantRefreshOutcome> {
        Box::pin(async move {
            let client = build_dpop_client(
                self.grant_base_url.clone(),
                Arc::clone(&self.dpop_signing_key),
                request.grant_jwt.clone(),
            )?;
            client
                .auth_refresh_session_grant(&request)
                .await
                .map_err(garth::Error::from)
        })
    }
}

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct AgentAuthenticatedTransportFactory {
    base_url: Url,
    runtime_signing_key: Arc<SigningKey>,
    dpop_signing_key: Arc<SigningKey>,
    dpop_jkt: String,
    verification_method: DidUrl,
}

const AGENT_SESSION_REFRESH_OPERATION: &str = "resume_soft_logged_out_session";

#[derive(Serialize)]
struct AgentSessionRefreshRequestDigest<'a> {
    operation: &'static str,
    grant_jwt_hash: String,
    principal_id: &'a str,
    device_id: &'a str,
    audience: &'a str,
    grant_binding_key_id: &'a str,
}

#[derive(Serialize)]
struct AgentSessionRefreshProofClaims<'a> {
    principal_id: &'a str,
    device_id: &'a str,
    audience: &'a str,
    challenge: &'a str,
    request_canonical_digest: &'a str,
    #[serde(with = "arkret::canonical::serde_helpers::canonical_timestamp")]
    issued_at: DateTime<Utc>,
    #[serde(with = "arkret::canonical::serde_helpers::canonical_timestamp")]
    expires_at: DateTime<Utc>,
}

fn mint_agent_session_refresh_proof(
    state: &SessionGrantState,
    device_id: &DeviceId,
    verification_method: &DidUrl,
    signing_key: &SigningKey,
) -> garth::Result<arkret::SessionGrantRefreshProof> {
    let grant_jwt_hash = format!(
        "sha256:{}",
        hex::encode(sha2::Sha256::digest(state.grant_jwt.as_bytes()))
    );
    let digest_input = AgentSessionRefreshRequestDigest {
        operation: AGENT_SESSION_REFRESH_OPERATION,
        grant_jwt_hash,
        principal_id: state.principal_id.as_str(),
        device_id: device_id.as_str(),
        audience: state.audience.as_str(),
        grant_binding_key_id: verification_method.as_str(),
    };
    let request_canonical_digest = arkret::Hash::new(
        arkret::canonical::canonical_sha256(&digest_input)
            .map_err(|error| garth::Error::Protocol(error.to_string()))?,
    )
    .map_err(|error| garth::Error::Protocol(error.to_string()))?;
    let challenge = format!("savfox-agent-refresh-{}", Uuid::now_v7());
    let issued_at = Utc::now();
    let expires_at = issued_at + chrono::Duration::seconds(60);
    let claims = AgentSessionRefreshProofClaims {
        principal_id: state.principal_id.as_str(),
        device_id: device_id.as_str(),
        audience: state.audience.as_str(),
        challenge: &challenge,
        request_canonical_digest: request_canonical_digest.as_str(),
        issued_at,
        expires_at,
    };
    let signing_bytes = arkret::canonical::canonical_json_bytes(&claims)
        .map_err(|error| garth::Error::Protocol(error.to_string()))?;
    let signature = arkret::base64url_encode(signing_key.sign(&signing_bytes).to_bytes());

    Ok(arkret::SessionGrantRefreshProof {
        proof_kind: Some(arkret::SessionGrantProofKind::AgentKeyProof),
        challenge: Some(challenge),
        request_canonical_digest: Some(request_canonical_digest),
        audience: Some(state.audience.clone()),
        issued_at: Some(issued_at),
        expires_at: Some(expires_at),
        signature: Some(signature),
        verification_method: Some(verification_method.clone()),
    })
}

fn generate_session_dpop_signing_key() -> SigningKey {
    SigningKey::from_bytes(&rand::random::<[u8; 32]>())
}

impl AuthenticatedTransportFactory for AgentAuthenticatedTransportFactory {
    type Transport = Client;

    fn build(&self, state: &SessionGrantState) -> garth::Result<Self::Transport> {
        build_dpop_client(
            self.base_url.clone(),
            Arc::clone(&self.dpop_signing_key),
            state.grant_jwt.clone(),
        )
    }

    fn refresh_options(
        &self,
        state: &SessionGrantState,
        fallback: &SessionRefreshOptions,
    ) -> garth::Result<SessionRefreshOptions> {
        let device_id = state
            .device_id
            .clone()
            .or_else(|| fallback.device_id.clone())
            .ok_or_else(|| {
                garth::Error::Protocol("agent session refresh device_id is required".to_owned())
            })?;
        let proof = mint_agent_session_refresh_proof(
            state,
            &device_id,
            &self.verification_method,
            &self.runtime_signing_key,
        )?;
        Ok(SessionRefreshOptions {
            audience: Some(state.audience.clone()),
            device_id: Some(device_id),
            proof: Some(proof),
            expected_dpop_jkt: Some(self.dpop_jkt.clone()),
        })
    }
}

pub type ArkretAgentSessionProvider = SessionTransportProvider<
    AgentSessionGrantTransport,
    AgentAuthenticatedTransportFactory,
    NoopSessionGrantStore,
>;

pub fn sign_mls_welcome_claim_envelope(
    key_ref: &ArkretKeyRef,
    verification_method: &str,
    envelope: &mut MlsWelcomeClaimEnvelope,
) -> anyhow::Result<()> {
    let verification_method = verification_method.trim();
    if verification_method.is_empty() {
        anyhow::bail!("Arkret MLS Welcome claim signature missing verification method");
    }
    let signing_key = load_ed25519_signing_key(key_ref)?;
    let signing_input = envelope
        .canonical_signing_bytes()
        .map_err(|err| anyhow::anyhow!("MLS Welcome claim signing input: {err}"))?;
    let signature = signing_key.sign(&signing_input);
    envelope.signature = ed25519_key_operation_signature(verification_method, signature)?;
    Ok(())
}

fn ed25519_key_operation_signature(
    verification_method: &str,
    signature: ed25519_dalek::Signature,
) -> anyhow::Result<KeyOperationSignature> {
    Ok(KeyOperationSignature {
        kid: arkret::NonEmptyString::new(verification_method.to_owned())
            .map_err(anyhow::Error::msg)?,
        alg: Some(arkret::NonEmptyString::new("Ed25519").map_err(anyhow::Error::msg)?),
        sig: arkret::Base64UrlString::new(arkret::base64url_encode(signature.to_bytes()))
            .map_err(anyhow::Error::msg)?,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn build_mls_key_packages_claim_request(
    target_principal_id: &str,
    intended_realm_id: &str,
    requester: &str,
    required_capabilities: &[String],
    claim_nonce: String,
    expires_at: DateTime<Utc>,
    target_device_ids: &[String],
    strand_id: Option<&str>,
    mls_group_id: Option<&str>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<KeyPackagesClaimRequestBody> {
    if claim_nonce.trim().is_empty() {
        anyhow::bail!("Arkret MLS KeyPackage claim nonce must not be empty");
    }
    let target_principal_id = Did::new(target_principal_id.to_owned())
        .with_context(|| format!("invalid Arkret KeyPackage target DID '{target_principal_id}'"))?;
    let intended_realm_id = RealmId::new(intended_realm_id.to_owned()).with_context(|| {
        format!("invalid Arkret KeyPackage claim Realm id '{intended_realm_id}'")
    })?;
    let requester = Did::new(requester.to_owned())
        .with_context(|| format!("invalid Arkret KeyPackage requester DID '{requester}'"))?;
    let target_device_ids = target_device_ids
        .iter()
        .map(|device_id| {
            DeviceId::new(device_id.to_owned()).with_context(|| {
                format!("invalid Arkret KeyPackage target device id '{device_id}'")
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let strand_id = strand_id
        .map(|value| {
            StrandId::new(value.to_owned())
                .with_context(|| format!("invalid Arkret KeyPackage claim Strand id '{value}'"))
        })
        .transpose()?;
    let mls_group_id = mls_group_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Ok(KeyPackagesClaimRequestBody {
        target_principal_id,
        intended_realm_id,
        requester,
        required_capabilities: required_capabilities.to_vec(),
        claim_nonce,
        expires_at,
        target_device_ids,
        minimal_metadata_allowed: None,
        timeout_ms,
        strand_id,
        mls_group_id,
        proofs: Vec::new(),
    })
}

impl ArkretHttpClient {
    #[must_use]
    pub fn inner(&self) -> &Client {
        &self.inner
    }

    #[must_use]
    pub fn from_inner(inner: Client) -> Self {
        let provider: Arc<dyn ArkretInitialSubmissionProvider> =
            Arc::new(PrincipalServerInitialSubmissionProvider::new(inner.clone()));
        Self {
            inner,
            initial_submission_provider: provider,
        }
    }

    /// Override the default authenticated Principal Server publication
    /// provider with another production authority transport.
    #[must_use]
    pub fn with_initial_submission_provider(
        mut self,
        provider: Arc<dyn ArkretInitialSubmissionProvider>,
    ) -> Self {
        self.initial_submission_provider = provider;
        self
    }

    #[must_use]
    pub fn client_core(&self) -> SavfoxArkretClientCore {
        ArkretClient::new(NativeExecutor, MemoryStore::new(), MemoryStore::new())
    }

    #[must_use]
    pub fn client_core_with_account_store(
        &self,
        store: FileStore,
    ) -> SavfoxDurableArkretClientCore {
        ArkretClient::new(NativeExecutor, store.clone(), store)
    }

    /// Build an applet HTTP client bound to `base_url`, authenticated via the
    /// applet bearer token. Personal-agent account runtimes use
    /// [`Self::login_agent`] instead.
    pub fn new(base_url: &str, access_token: &str) -> anyhow::Result<Self> {
        let url =
            Url::parse(base_url).with_context(|| format!("invalid Arkret base_url: {base_url}"))?;
        let inner = ClientBuilder::new(url)
            .auth(Auth::Bearer(access_token.to_owned()))
            .build()
            .map_err(|err| anyhow::anyhow!("failed to build Arkret HTTP client: {err}"))?;
        Ok(Self::from_inner(inner))
    }

    /// Construct a personal-agent HTTP client by exchanging a runtime-key
    /// `agent_key_proof` for a short-lived DPoP-bound session grant.
    #[allow(clippy::too_many_arguments)]
    pub async fn login_agent_provider(
        base_url: &str,
        key_ref: &ArkretKeyRef,
        principal_did: Did,
        verification_method: &str,
        agent_key_authorization_ref: &str,
        requested_scope: Vec<String>,
        audience: &str,
        device_id: DeviceId,
        realm_id: Option<&str>,
    ) -> anyhow::Result<(ArkretAgentSessionProvider, ArkretSession)> {
        validate_agent_key_ref(key_ref)?;
        let audience = Did::new(audience.to_owned())
            .with_context(|| format!("invalid Arkret service audience DID '{audience}'"))?;
        let verification_method = DidUrl::new(verification_method.to_owned()).map_err(|err| {
            anyhow::anyhow!("invalid Arkret verification method '{verification_method}': {err}")
        })?;
        // Session grants can exceed the Windows Credential Manager 2560-byte
        // secret limit. Keep the short-lived grant in the provider's memory;
        // the long-lived runtime signing key remains keyring-backed.
        let grant_store = NoopSessionGrantStore;
        let resource_url =
            Url::parse(base_url).with_context(|| format!("invalid Arkret base_url: {base_url}"))?;
        let grant_base_url = discover_account_authority_base_url(&resource_url).await?;
        let runtime_signing_key = Arc::new(load_ed25519_signing_key(key_ref)?);
        // The grant-binding key is an ephemeral session credential.  It MUST
        // not reuse the long-lived Agent runtime key that signs Agent proofs,
        // Events, KeyPackages, or MLS leaves.
        let dpop_signing_key = Arc::new(generate_session_dpop_signing_key());
        let grant_htu = joined_htu(&grant_base_url, SESSION_GRANT_PATH)?;
        let binding_proof = arkret::dpop::build_dpop_proof(
            &arkret::dpop::DpopProofRequest::new("POST", grant_htu.clone()),
            &dpop_signing_key,
        )?;
        let dpop_jkt = binding_proof.jkt.clone();
        let binding_proof = binding_proof.header_value;
        let bootstrap = personal_agent_client_builder(grant_base_url.clone())
            .auth(Auth::Dpop(DpopAuth::proof_only({
                let expected_htu = grant_htu.clone();
                let proof_jwt = binding_proof.clone();
                move |request| {
                    if request.method != "POST"
                        || request.htu != expected_htu
                        || request.access_token.is_some()
                    {
                        return Err(arkret::http_client::Error::Protocol(
                            "unexpected DPoP kickoff request shape".to_owned(),
                        ));
                    }
                    Ok(proof_jwt.clone())
                }
            })))
            .build()
            .map_err(|err| anyhow::anyhow!("agent session bootstrap HTTP client: {err}"))?;

        let expires_at = Utc::now() + chrono::Duration::minutes(5);
        let challenge = format!("savfox-agent-session-{}", Uuid::now_v7());
        let nonce = Uuid::now_v7().to_string();
        let agent_scope_request = arkret::SessionGrantAgentScopeRequest {
            realm_ids: realm_id
                .map(|realm_id| {
                    RealmId::new(realm_id.to_owned()).with_context(|| {
                        format!("invalid Arkret agent session Realm id '{realm_id}'")
                    })
                })
                .transpose()?
                .into_iter()
                .collect(),
            strand_ids: Vec::new(),
            track_names: Vec::new(),
        };
        let dpop_binding_proof = SessionGrantDpopBindingProof {
            proof_jwt: binding_proof,
        };
        let signing_input = arkret::session_grant::agent_key_proof_signing_input_for_session_grant(
            &principal_did,
            &device_id,
            &requested_scope,
            agent_key_authorization_ref,
            &agent_scope_request,
            &dpop_binding_proof,
            verification_method.clone(),
            challenge.clone(),
            nonce.clone(),
            audience.clone(),
            expires_at,
        )
        .map_err(|err| anyhow::anyhow!("agent_key_proof signing input: {err}"))?;
        let signature = runtime_signing_key.sign(
            &signing_input
                .canonical_bytes()
                .map_err(|err| anyhow::anyhow!("agent_key_proof canonical bytes: {err}"))?,
        );
        let signature = arkret::base64url_encode(signature.to_bytes());
        let login = AgentKeyProofLogin {
            principal_id: principal_did.clone(),
            device_id: device_id.clone(),
            requested_scope,
            agent_key_authorization_ref: agent_key_authorization_ref.to_owned(),
            agent_scope_request,
            dpop_binding_proof,
            verification_method: verification_method.clone(),
            challenge,
            nonce,
            audience: audience.clone(),
            expires_at,
            signature,
        };
        let session_transport = AgentSessionGrantTransport {
            grant_base_url,
            bootstrap,
            dpop_signing_key: Arc::clone(&dpop_signing_key),
        };
        let factory = AgentAuthenticatedTransportFactory {
            base_url: resource_url,
            runtime_signing_key,
            dpop_signing_key,
            dpop_jkt: dpop_jkt.clone(),
            verification_method: verification_method.clone(),
        };
        let refresh_options = SessionRefreshOptions {
            audience: Some(audience.clone()),
            device_id: Some(device_id),
            proof: None,
            expected_dpop_jkt: Some(dpop_jkt),
        };
        let restored = SessionTransportProvider::restore(
            session_transport.clone(),
            factory.clone(),
            refresh_options.clone(),
            grant_store,
        )
        .map_err(|error| anyhow::anyhow!("restore agent session grant: {error}"))?;
        if let Some(state) = restored.session().current_state() {
            if state.principal_id == principal_did
                && state.audience == audience
                && state.expires_at > Utc::now()
            {
                let session = arkret_session_from_state(&state);
                return Ok((restored, session));
            }
            grant_store
                .clear()
                .map_err(|error| anyhow::anyhow!("clear stale agent session grant: {error}"))?;
        }

        let session_engine = SessionEngine::new(session_transport);
        session_engine
            .login(LoginKind::AgentKeyProof(login), Utc::now())
            .await
            .map_err(agent_session_exchange_error)?;
        let state = session_engine
            .current_state()
            .context("agent_key_proof session grant exchange did not yield state")?;

        let provider = SessionTransportProvider::with_store(
            session_engine,
            factory,
            refresh_options,
            grant_store,
        )
        .await
        .map_err(|error| anyhow::anyhow!("persist agent session grant: {error}"))?;
        Ok((provider, arkret_session_from_state(&state)))
    }

    /// Compatibility helper for short-lived callers that only need the
    /// authenticated client returned by the shared session provider.
    #[allow(clippy::too_many_arguments)]
    pub async fn login_agent(
        base_url: &str,
        key_ref: &ArkretKeyRef,
        principal_did: Did,
        verification_method: &str,
        agent_key_authorization_ref: &str,
        requested_scope: Vec<String>,
        audience: &str,
        device_id: DeviceId,
        realm_id: Option<&str>,
    ) -> anyhow::Result<(Self, ArkretSession)> {
        let (provider, session) = Self::login_agent_provider(
            base_url,
            key_ref,
            principal_did,
            verification_method,
            agent_key_authorization_ref,
            requested_scope,
            audience,
            device_id,
            realm_id,
        )
        .await?;
        let inner = provider
            .provide()
            .await
            .map_err(|error| anyhow::anyhow!("build authenticated Arkret client: {error}"))?;
        Ok((Self::from_inner(inner), session))
    }

    /// Construct an applet HTTP client by running DID-proof login.
    ///
    /// Builds an unauthenticated underlying `Client`, runs the applet
    /// DID-proof grant exchange, then rebuilds the authenticated `Client`
    /// carrying the `Authorization: Bearer <grant>` header. This is not the
    /// personal-agent runtime path.
    pub async fn login(
        base_url: &str,
        signer: &Ed25519PayloadSigner,
        principal_did: Did,
        device_id: DeviceId,
        challenge: &str,
        audience: &str,
    ) -> anyhow::Result<(Self, ArkretSession)> {
        let url =
            Url::parse(base_url).with_context(|| format!("invalid Arkret base_url: {base_url}"))?;
        let bootstrap = ClientBuilder::new(url.clone())
            .build()
            .map_err(|err| anyhow::anyhow!("bootstrap HTTP client: {err}"))?;
        let session = login_with_signer(
            &bootstrap,
            signer,
            principal_did,
            device_id,
            challenge,
            audience,
        )
        .await?;
        let inner = ClientBuilder::new(url)
            .auth(Auth::Bearer(session.session_grant.clone()))
            .build()
            .map_err(|err| anyhow::anyhow!("authenticated HTTP client: {err}"))?;
        Ok((Self::from_inner(inner), session))
    }

    /// `GET /_arkret/describe` — used at startup to verify the target
    /// server and pin the service DID.
    pub async fn server_describe(&self) -> anyhow::Result<ServiceDescribe> {
        self.inner
            .describe()
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }

    /// `GET /_arkret/self/events/subscribe` — returns a
    /// [`ArkretFrameStream`] that yields fully-parsed `EventsSubscribeFrame`
    /// items.
    pub async fn events_subscribe_stream(
        &self,
        realm_id: &str,
        after: Option<&str>,
    ) -> anyhow::Result<ArkretFrameStream> {
        let mut options = arkret::http_client::EventsSubscribeOptions::new().realm(realm_id);
        if let Some(after) = after {
            options = options.after(after);
        }
        let stream = self
            .inner
            .events_subscribe_frames(&options)
            .await
            .map_err(|err| anyhow::anyhow!("arkret events_subscribe_stream: {err}"))?;
        Ok(Box::pin(futures_util::stream::unfold(
            stream,
            |mut stream| async move {
                match stream.next_frame().await {
                    Ok(Some(frame)) => Some((Ok(frame), stream)),
                    Ok(None) => None,
                    Err(err) => Some((
                        Err(anyhow::anyhow!("arkret events_subscribe_stream: {err}")),
                        stream,
                    )),
                }
            },
        )))
    }

    /// `GET /_arkret/self/account/subscribe` — returns a user-scoped account
    /// stream. Personal-agent runtimes consume this instead of binding the
    /// listener to a configured Realm.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn account_subscribe_stream(
        &self,
        after: Option<&str>,
    ) -> anyhow::Result<ArkretAccountFrameStream> {
        let request = SyncRequestBody {
            after: after.map(str::to_owned),
            catchup: None,
            filter: None,
            subscriptions: None,
        };
        let stream = self
            .inner
            .account_subscribe_frames(&request)
            .await
            .map_err(|err| anyhow::anyhow!("arkret account_subscribe: {err}"))?;
        Ok(Box::pin(futures_util::stream::unfold(
            stream,
            |mut stream| async move {
                match stream.next_frame().await {
                    Ok(Some(frame)) => Some((Ok(frame), stream)),
                    Ok(None) => None,
                    Err(err) => Some((
                        Err(anyhow::anyhow!("arkret account_subscribe: {err}")),
                        stream,
                    )),
                }
            },
        )))
    }

    /// Obtain issuer-produced publication evidence for an exact signed Event.
    pub async fn prepare_initial_submission(
        &self,
        event: &PreparedStandardEvent,
    ) -> anyhow::Result<EventInitialSubmission> {
        let submission = self
            .initial_submission_provider
            .initial_submission(event)
            .await?;
        if &submission.event != event.event() {
            anyhow::bail!(
                "Arkret EventInitialSubmissionProvider replaced or mutated the signed Event"
            );
        }
        submission
            .validate_structural()
            .map_err(|error| anyhow::anyhow!("invalid Arkret initial submission: {error}"))?;
        Ok(submission)
    }

    /// Submit one issuer-produced initial publication wrapper.
    pub async fn submit_initial(
        &self,
        submission: &EventInitialSubmission,
    ) -> anyhow::Result<EventsSubmitOutcome> {
        submission
            .validate_structural()
            .map_err(|error| anyhow::anyhow!("invalid Arkret initial submission: {error}"))?;
        self.inner
            .events_submit(submission)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }

    /// Prepare and submit one signed Event through the installed publication
    /// evidence provider.
    pub async fn submit_event(
        &self,
        event: &PreparedStandardEvent,
    ) -> anyhow::Result<EventsSubmitOutcome> {
        let submission = self.prepare_initial_submission(event).await?;
        self.submit_initial(&submission).await
    }

    pub async fn keypackages_claim(
        &self,
        request: &KeyPackagesClaimRequestBody,
    ) -> anyhow::Result<KeyPackagesClaimOutcome> {
        self.inner
            .keypackages_claim(request)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }
}

#[derive(Debug)]
struct AgentSessionExchangeError {
    reason: String,
    action: &'static str,
    source: garth::Error,
}

impl std::fmt::Display for AgentSessionExchangeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "agent_key_proof session grant exchange failed ({}): {}: {}",
            self.reason, self.action, self.source
        )
    }
}

impl std::error::Error for AgentSessionExchangeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Preserve the machine-readable Account Authority reason through `anyhow`
/// context so lifecycle-aware callers (notably explicit unbind) can distinguish
/// an irreversibly dead authorization from a transient authentication failure.
pub fn agent_session_exchange_reason(error: &anyhow::Error) -> Option<&str> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<AgentSessionExchangeError>())
        .map(|error| error.reason.as_str())
}

/// Whether the authorization can never become valid again. KeyPackages bind
/// to the exact authorization Event, so these outcomes also make its pool
/// permanently unclaimable. A paused Agent is deliberately excluded because
/// resuming can make the same authorization usable again.
pub fn agent_session_reason_is_irreversibly_terminal(reason: &str) -> bool {
    matches!(
        reason,
        "agent_deactivated"
            | "agent_key_authorization_expired"
            | "agent_key_authorization_revoked"
            | "superseded_by_repairing"
    )
}

fn agent_session_exchange_error(error: garth::Error) -> anyhow::Error {
    let reason = match &error {
        garth::Error::Api { error, .. } => error
            .details()
            .get("reason_code")
            .and_then(serde_json::Value::as_str)
            .or_else(|| reason_code_from_message(error.message()))
            .unwrap_or_else(|| error.code()),
        _ => "unknown",
    };
    let action = match reason {
        "agent_key_authorization_expired" => "the controller must re-authorize this runtime key",
        "agent_paused" => "the controller must resume this agent",
        "agent_deactivated" => "the agent is deactivated and must be provisioned again",
        "superseded_by_repairing" => {
            "this runtime key was replaced; import the new pairing bootstrap"
        }
        _ => "verify the pairing, authorization reference, scope, and service audience",
    };
    AgentSessionExchangeError {
        reason: reason.to_owned(),
        action,
        source: error,
    }
    .into()
}

/// Coauth's Arkret session-grant boundary currently carries the specific
/// rejection reason in the protocol message (`reason_code=...`) while keeping
/// the envelope code at the broad `failed_precondition` category. Accept that
/// wire-compatible representation as well as the preferred structured detail
/// so callers never have to parse the rendered `anyhow` chain.
fn reason_code_from_message(message: &str) -> Option<&str> {
    const MARKER: &str = "reason_code=";
    let value = message.split_once(MARKER)?.1;
    let end = value
        .find(|character: char| {
            !character.is_ascii_alphanumeric() && character != '_' && character != '-'
        })
        .unwrap_or(value.len());
    let reason = &value[..end];
    (!reason.is_empty()).then_some(reason)
}

fn validate_agent_key_ref(key_ref: &ArkretKeyRef) -> anyhow::Result<()> {
    if matches!(key_ref, ArkretKeyRef::Keyring { .. }) {
        Ok(())
    } else {
        anyhow::bail!("Arkret personal-agent session keys must use key_ref kind=keyring")
    }
}

fn arkret_session_from_state(state: &SessionGrantState) -> ArkretSession {
    ArkretSession {
        session_grant: state.grant_jwt.clone(),
        expires_at: state.expires_at,
        principal_did: state.principal_id.clone(),
        device_id: state.device_id.clone(),
    }
}

fn build_dpop_client(
    base_url: Url,
    signing_key: Arc<SigningKey>,
    access_token: String,
) -> garth::Result<Client> {
    personal_agent_client_builder(base_url)
        .auth(Auth::Dpop(DpopAuth::with_access_token(
            access_token,
            move |request| {
                arkret::dpop::build_dpop_proof(&request, &signing_key)
                    .map(|proof| proof.header_value)
                    .map_err(arkret::http_client::Error::from)
            },
        )))
        .build()
        .map_err(garth::Error::from)
}

#[cfg(test)]
fn build_dpop_header(
    signing_key: &SigningKey,
    method: impl Into<String>,
    htu: impl Into<String>,
    access_token: Option<&str>,
) -> arkret::Result<String> {
    let mut request = arkret::dpop::DpopProofRequest::new(method, htu);
    if let Some(access_token) = access_token {
        request = request.access_token(access_token.to_owned());
    }
    Ok(arkret::dpop::build_dpop_proof(&request, signing_key).map(|proof| proof.header_value)?)
}

fn joined_htu(base_url: &Url, path: &str) -> anyhow::Result<String> {
    let mut url = base_url
        .join(path.trim_start_matches('/'))
        .with_context(|| format!("invalid Arkret endpoint path: {path}"))?;
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

async fn discover_account_authority_base_url(resource_url: &Url) -> anyhow::Result<Url> {
    let discovery = personal_agent_client_builder(resource_url.clone())
        .build()
        .map_err(|error| anyhow::anyhow!("Arkret service discovery client: {error}"))?;
    let description = discovery
        .describe()
        .await
        .map_err(|error| anyhow::anyhow!("Arkret service discovery: {error}"))?;
    let authority = description
        .auth_metadata
        .account_authority
        .context("Arkret service description omitted auth_metadata.account_authority")?;
    let authority_url = Url::parse(&authority.origin).with_context(|| {
        format!(
            "invalid Arkret account authority origin: {}",
            authority.origin
        )
    })?;
    let advertised_gate = Url::parse(&authority.gate_account_base).with_context(|| {
        format!(
            "invalid Arkret account authority gate_account_base: {}",
            authority.gate_account_base
        )
    })?;
    let expected_gate = joined_htu(&authority_url, "/_arkret/gate/account")?;
    if advertised_gate.as_str().trim_end_matches('/') != expected_gate.trim_end_matches('/') {
        anyhow::bail!(
            "Arkret account authority metadata mismatch: origin '{}' does not own gate_account_base '{}'",
            authority.origin,
            authority.gate_account_base
        );
    }
    Ok(authority_url)
}

fn personal_agent_client_builder(base_url: Url) -> ClientBuilder {
    // Personal-agent conformance stacks run the resource and account authority
    // on loopback. The SDK keeps plaintext HTTP rejected for every non-loopback
    // host, including when this opt-in is enabled.
    ClientBuilder::new(base_url).allow_insecure_localhost()
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use ed25519_dalek::Signature;
    use serde_json::Value;

    use super::*;

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7_u8; 32])
    }

    fn key_ref() -> ArkretKeyRef {
        ArkretKeyRef::InlineSeedBase64 {
            value: STANDARD_NO_PAD.encode([7_u8; 32]),
        }
    }

    fn agent_session_state() -> SessionGrantState {
        SessionGrantState {
            principal_id: Did::new("did:webvh:z6mkfixture:agent.example").unwrap(),
            device_id: Some(
                DeviceId::new("ak:device:0196419b-0000-7000-8000-000000000001").unwrap(),
            ),
            grant_id: arkret::GrantId::new("ak:grant:0196419b-0000-7000-8000-000000000001")
                .unwrap(),
            grant_jwt: "agent.grant.jwt".to_owned(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
            audience: Did::new("did:webvh:z6mkfixture:service.example").unwrap(),
            granted_scope: vec!["ak.self.events.stream.subscribe".to_owned()],
            session_public_key: Some("session-public-key".to_owned()),
            dpop_jkt: Some("agent-dpop-jkt".to_owned()),
        }
    }

    #[test]
    fn agent_session_refresh_mints_fresh_runtime_key_proof() {
        let state = agent_session_state();
        let device_id = state.device_id.as_ref().unwrap();
        let verification_method =
            DidUrl::new("did:webvh:z6mkfixture:agent.example#runtime-key-1").unwrap();
        let first = mint_agent_session_refresh_proof(
            &state,
            device_id,
            &verification_method,
            &signing_key(),
        )
        .expect("first refresh proof");
        let second = mint_agent_session_refresh_proof(
            &state,
            device_id,
            &verification_method,
            &signing_key(),
        )
        .expect("second refresh proof");

        assert_eq!(
            first.proof_kind,
            Some(arkret::SessionGrantProofKind::AgentKeyProof)
        );
        assert_ne!(first.challenge, second.challenge);
        assert_eq!(first.audience.as_ref(), Some(&state.audience));
        assert_eq!(
            first.verification_method.as_ref(),
            Some(&verification_method)
        );

        let claims = AgentSessionRefreshProofClaims {
            principal_id: state.principal_id.as_str(),
            device_id: device_id.as_str(),
            audience: state.audience.as_str(),
            challenge: first.challenge.as_deref().unwrap(),
            request_canonical_digest: first.request_canonical_digest.as_ref().unwrap().as_str(),
            issued_at: first.issued_at.unwrap(),
            expires_at: first.expires_at.unwrap(),
        };
        let bytes = arkret::canonical::canonical_json_bytes(&claims).unwrap();
        let signature = Signature::from_slice(
            &arkret::base64url_decode(first.signature.as_deref().unwrap()).unwrap(),
        )
        .unwrap();
        signing_key()
            .verifying_key()
            .verify_strict(&bytes, &signature)
            .expect("refresh proof must be signed by the authorized runtime key");
    }

    #[test]
    fn session_dpop_key_is_distinct_from_agent_runtime_key() {
        let runtime_key = signing_key();
        let dpop_key = generate_session_dpop_signing_key();

        assert_ne!(runtime_key.to_bytes(), dpop_key.to_bytes());
        let dpop_proof = arkret::dpop::build_dpop_proof(
            &arkret::dpop::DpopProofRequest::new(
                "POST",
                "https://arkret.example/_arkret/gate/account/session-grants",
            ),
            &dpop_key,
        )
        .expect("DPoP proof");
        let runtime_proof = arkret::dpop::build_dpop_proof(
            &arkret::dpop::DpopProofRequest::new(
                "POST",
                "https://arkret.example/_arkret/gate/account/session-grants",
            ),
            &runtime_key,
        )
        .expect("runtime-key proof");
        assert_ne!(dpop_proof.jkt, runtime_proof.jkt);
    }

    #[test]
    fn dpop_header_binds_access_token_ath() {
        let header = build_dpop_header(
            &signing_key(),
            "GET",
            "https://arkret.example/_arkret/self/events",
            Some("session-grant-token"),
        )
        .expect("dpop header");
        let parts: Vec<&str> = header.split('.').collect();
        assert_eq!(parts.len(), 3);

        let protected: Value =
            serde_json::from_slice(&arkret::base64url_decode(parts[0]).unwrap()).unwrap();
        let payload: Value =
            serde_json::from_slice(&arkret::base64url_decode(parts[1]).unwrap()).unwrap();

        assert_eq!(protected["typ"], "dpop+jwt");
        assert_eq!(protected["alg"], "EdDSA");
        assert_eq!(payload["htm"], "GET");
        assert_eq!(payload["htu"], "https://arkret.example/_arkret/self/events");
        assert_eq!(
            payload["ath"],
            arkret::dpop::dpop_access_token_hash("session-grant-token")
        );
        assert_ne!(
            payload["ath"],
            arkret::dpop::dpop_access_token_hash("other-token")
        );
    }

    #[test]
    fn claim_request_builder_validates_typed_claim_fields() {
        let request = build_mls_key_packages_claim_request(
            "did:webvh:z6mkfixture:bob.example",
            "ak:realm:01904100-0000-7000-8000-000000000001",
            "did:webvh:z6mkfixture:alice.example",
            &["mimi.content.v1".to_owned(), "ak.content.v1".to_owned()],
            "claim-nonce-1".to_owned(),
            Utc::now() + chrono::Duration::minutes(5),
            &["ak:device:01904100-0000-7000-8000-00000000000e".to_owned()],
            Some("ak:strand:01904100-0000-7000-8000-000000000002"),
            Some("group-1"),
            Some(1500),
        )
        .expect("claim request should build");

        assert_eq!(
            request.target_principal_id.as_str(),
            "did:webvh:z6mkfixture:bob.example"
        );
        assert_eq!(request.required_capabilities.len(), 2);
        assert_eq!(request.target_device_ids.len(), 1);
        assert_eq!(
            request.strand_id.as_ref().map(StrandId::as_str),
            Some("ak:strand:01904100-0000-7000-8000-000000000002")
        );
        assert_eq!(request.mls_group_id.as_deref(), Some("group-1"));
        assert!(request.proofs.is_empty());
    }

    #[test]
    fn mls_welcome_claim_envelope_signing_uses_sdk_transcript() {
        let mut envelope = MlsWelcomeClaimEnvelope {
            keypackage_ref: "ak:mls:keypackage:test".to_owned(),
            keypackage_digest: arkret::Hash::new(format!("sha256:{}", "aa".repeat(32))).unwrap(),
            intended_realm_id: RealmId::new(
                "ak:realm:01904100-0000-7000-8000-000000000001".to_owned(),
            )
            .unwrap(),
            claim_id: arkret::NonEmptyString::new("ak:claim:test").unwrap(),
            requester_did: Did::new("did:webvh:z6mkfixture:alice.example".to_owned()).unwrap(),
            trust_binding: arkret::MlsRequesterTrustBinding::RequesterDeviceId(
                DeviceId::new("ak:device:01904100-0000-7000-8000-000000000001".to_owned()).unwrap(),
            ),
            nonce: arkret::NonEmptyString::new("nonce-1").unwrap(),
            welcome_digest: arkret::Hash::new(format!("sha256:{}", "bb".repeat(32))).unwrap(),
            created_at: Utc::now(),
            signature: KeyOperationSignature {
                kid: arkret::NonEmptyString::new("pending").unwrap(),
                alg: None,
                sig: arkret::Base64UrlString::new("cGVuZGluZw").unwrap(),
            },
        };
        let before = envelope
            .canonical_signing_bytes()
            .expect("SDK transcript should serialize");

        sign_mls_welcome_claim_envelope(
            &key_ref(),
            "did:webvh:z6mkfixture:alice.example#runtime-1",
            &mut envelope,
        )
        .expect("signing should succeed");
        let after = envelope
            .canonical_signing_bytes()
            .expect("SDK transcript should remain stable");
        assert_eq!(before, after);
        envelope
            .validate_signature_shape()
            .expect("signature shape should be valid");

        let sig = Signature::from_slice(
            &arkret::base64url_decode(envelope.signature.sig.as_str()).unwrap(),
        )
        .expect("signature bytes");
        signing_key()
            .verifying_key()
            .verify_strict(&after, &sig)
            .expect("signature should verify over SDK transcript");
    }

    #[test]
    fn personal_agent_provider_requires_platform_keyring_reference() {
        let inline = key_ref();
        assert!(validate_agent_key_ref(&inline).is_err());
        assert!(
            validate_agent_key_ref(&ArkretKeyRef::Keyring {
                service: "savfox-arkret".to_owned(),
                account: "agent-1".to_owned(),
            })
            .is_ok()
        );
    }

    #[test]
    fn personal_agent_client_allows_only_insecure_loopback() {
        for url in ["http://127.0.0.1:8787", "http://localhost:8787"] {
            personal_agent_client_builder(Url::parse(url).unwrap())
                .build()
                .expect("loopback HTTP should be available for local conformance stacks");
        }

        assert!(
            personal_agent_client_builder(Url::parse("http://accounts.example:8787").unwrap())
                .build()
                .is_err(),
            "non-loopback HTTP must remain rejected"
        );
    }

    #[test]
    fn agent_session_errors_preserve_actionable_reason_codes() {
        for (reason, expected) in [
            (
                "agent_key_authorization_expired",
                "controller must re-authorize",
            ),
            ("agent_paused", "controller must resume"),
            ("agent_deactivated", "must be provisioned again"),
            ("superseded_by_repairing", "new pairing bootstrap"),
        ] {
            let envelope = arkret::ErrorEnvelope::new("failed_precondition", "rejected")
                .with_detail("reason_code", serde_json::json!(reason));
            let error = agent_session_exchange_error(garth::Error::Api {
                status: 412,
                error: Box::new(envelope),
            });
            assert_eq!(agent_session_exchange_reason(&error), Some(reason));
            let rendered = error.to_string();
            assert!(rendered.contains(reason), "{rendered}");
            assert!(rendered.contains(expected), "{rendered}");
        }
    }

    #[test]
    fn agent_session_errors_preserve_message_encoded_reason_codes() {
        let envelope = arkret::ErrorEnvelope::new(
            "failed_precondition",
            "reason_code=agent_deactivated; failed_precondition",
        );
        let error = agent_session_exchange_error(garth::Error::Api {
            status: 403,
            error: Box::new(envelope),
        });

        assert_eq!(
            agent_session_exchange_reason(&error),
            Some("agent_deactivated")
        );
        assert!(agent_session_reason_is_irreversibly_terminal(
            agent_session_exchange_reason(&error).unwrap()
        ));
    }

    #[test]
    fn only_irreversible_agent_session_failures_allow_terminal_cleanup() {
        for reason in [
            "agent_deactivated",
            "agent_key_authorization_expired",
            "agent_key_authorization_revoked",
            "superseded_by_repairing",
        ] {
            assert!(agent_session_reason_is_irreversibly_terminal(reason));
        }
        for reason in ["agent_paused", "proof_invalid", "unknown"] {
            assert!(!agent_session_reason_is_irreversibly_terminal(reason));
        }
    }
}
