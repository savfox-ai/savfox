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
    AccountSubscribeFrame, AgentSessionGrantRefreshRequest, AgentSessionRefreshProof,
    AgentSessionRefreshProofContext, Base64UrlString, DeviceId, DidCoreId, DidUrl,
    EventsSubmitOutcome, EventsSubscribeFrame, KeyOperationSignature, KeyPackagesClaimOutcome,
    KeyPackagesClaimRequestBody, KeyPackagesClaimServiceBinding, MlsWelcomeClaimEnvelope,
    NonEmptyString, PeerKeyPackageClaimPurpose, PeerKeyPackageRequesterAuthorization,
    PreparedStandardEvent, RealmId, ServiceDescribe, SessionGrantDpopBindingProof,
    SessionGrantRefreshRequestBody, StrandId, SyncRequestBody, UnsignedAgentSessionGrantRequest,
    UnsignedAgentSessionRefreshProof,
};
use arkret_wire::EventInitialSubmission;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer as _, SigningKey};
use futures_util::Stream;
use garth::session::BoxSessionFuture;
use garth::{
    ArkretClient, AuthenticatedTransportFactory, FileStore, MemoryStore, NativeExecutor,
    NoopSessionGrantStore, SessionEngine, SessionGrantState, SessionGrantStore,
    SessionGrantTransport, SessionRefreshOptions, SessionTransportProvider, TransportProvider,
};
use url::Url;
use uuid::Uuid;

use super::session::ArkretSession;
use super::signer::{ArkretKeyRef, load_ed25519_signing_key};

const SESSION_GRANT_PATH: &str = "/_arkret/gate/account/session-grants";

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct ArkretHttpClient {
    inner: Client,
    initial_submission_provider: Arc<dyn ArkretInitialSubmissionProvider>,
}

/// Host-owned integration that wraps a fully-authored, fully-signed Event for
/// initial publication.
///
/// Online submissions carry no authorization lease. Implementations may also
/// obtain delayed-publication evidence from a local authority component or a
/// remote authz service. They must return the exact Event supplied by Savfox
/// inside an [`EventInitialSubmission`]; Savfox validates that invariant and
/// the wrapper structure before enqueueing or sending it.
#[async_trait::async_trait]
pub trait ArkretInitialSubmissionProvider: Send + Sync + 'static {
    async fn initial_submission(
        &self,
        event: &PreparedStandardEvent,
    ) -> anyhow::Result<EventInitialSubmission>;
}

/// Production publication provider backed by the authenticated Arkret client.
/// The default path produces an online submission without an authorization
/// lease; delayed-publication providers remain pluggable through the trait.
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
            let grant_jwt = match &request {
                SessionGrantRefreshRequestBody::Human(request) => &request.grant_jwt,
                SessionGrantRefreshRequestBody::Agent(request) => &request.grant_jwt,
            };
            let client = build_dpop_client(
                self.grant_base_url.clone(),
                Arc::clone(&self.dpop_signing_key),
                grant_jwt.clone(),
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

fn mint_agent_session_refresh_proof(
    state: &SessionGrantState,
    device_id: &DeviceId,
    verification_method: &DidUrl,
    signing_key: &SigningKey,
) -> garth::Result<AgentSessionRefreshProof> {
    let request_canonical_digest = arkret::agent_session_refresh_request_digest(
        &state.grant_jwt,
        &state.principal_id,
        device_id,
        &state.audience,
        verification_method,
    )
    .map_err(|error| garth::Error::Protocol(error.to_string()))?;
    let issued_at = Utc::now();
    let expires_at = issued_at + chrono::Duration::seconds(60);
    let unsigned = UnsignedAgentSessionRefreshProof {
        context: AgentSessionRefreshProofContext::V1,
        request_canonical_digest,
        audience: state.audience.clone(),
        issued_at,
        expires_at,
        verification_method: verification_method.clone(),
    };
    let signing_bytes = unsigned
        .canonical_signing_bytes()
        .map_err(|error| garth::Error::Protocol(error.to_string()))?;
    let signature = Base64UrlString::new(arkret::base64url_encode(
        signing_key.sign(&signing_bytes).to_bytes(),
    ))
    .map_err(|error| garth::Error::Protocol(error.to_string()))?;
    unsigned
        .attach_signature(signature)
        .map_err(|error| garth::Error::Protocol(error.to_string()))
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
        _fallback: &SessionRefreshOptions,
    ) -> garth::Result<SessionRefreshOptions> {
        let device_id = state.device_id.clone().ok_or_else(|| {
            garth::Error::Protocol("agent session refresh device_id is required".to_owned())
        })?;
        let proof = mint_agent_session_refresh_proof(
            state,
            &device_id,
            &self.verification_method,
            &self.runtime_signing_key,
        )?;
        Ok(SessionRefreshOptions {
            request: Some(SessionGrantRefreshRequestBody::Agent(
                AgentSessionGrantRefreshRequest {
                    grant_jwt: state.grant_jwt.clone(),
                    audience: Some(state.audience.clone()),
                    device_id,
                    agent_session_refresh_proof: proof,
                },
            )),
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
        signature_algorithm: Some(
            arkret::NonEmptyString::new("Ed25519").map_err(anyhow::Error::msg)?,
        ),
        sig: arkret::Base64UrlString::new(arkret::base64url_encode(signature.to_bytes()))
            .map_err(anyhow::Error::msg)?,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn build_mls_key_packages_claim_request(
    claim_request_id: String,
    target_principal_id: &str,
    intended_realm_id: &str,
    requester: &str,
    claim_purpose: PeerKeyPackageClaimPurpose,
    required_capabilities: &[String],
    claim_nonce: String,
    expires_at: DateTime<Utc>,
    target_device_ids: &[String],
    strand_id: Option<&str>,
    mls_group_id: &str,
    timeout_ms: Option<u64>,
    source_service_id: &str,
    destination_service_id: &str,
    requester_authorization: PeerKeyPackageRequesterAuthorization,
) -> anyhow::Result<KeyPackagesClaimRequestBody> {
    let claim_request_id = Base64UrlString::new(claim_request_id).map_err(|error| {
        anyhow::anyhow!("invalid Arkret MLS KeyPackage claim request id: {error}")
    })?;
    if claim_nonce.trim().is_empty() {
        anyhow::bail!("Arkret MLS KeyPackage claim nonce must not be empty");
    }
    let claim_nonce = Base64UrlString::new(claim_nonce)
        .map_err(|error| anyhow::anyhow!("invalid Arkret MLS KeyPackage claim nonce: {error}"))?;
    let target_principal_id = DidCoreId::new(target_principal_id.to_owned())
        .with_context(|| format!("invalid Arkret KeyPackage target DID '{target_principal_id}'"))?;
    let intended_realm_id = RealmId::new(intended_realm_id.to_owned()).with_context(|| {
        format!("invalid Arkret KeyPackage claim Realm id '{intended_realm_id}'")
    })?;
    let requester = DidCoreId::new(requester.to_owned())
        .with_context(|| format!("invalid Arkret KeyPackage requester DID '{requester}'"))?;
    let required_capabilities = required_capabilities
        .iter()
        .map(|capability| {
            NonEmptyString::new(capability.clone()).map_err(|error| {
                anyhow::anyhow!("invalid Arkret KeyPackage capability '{capability}': {error}")
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
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
    let mls_group_id = NonEmptyString::new(mls_group_id.trim().to_owned()).map_err(|error| {
        anyhow::anyhow!("invalid Arkret KeyPackage claim MLS group id: {error}")
    })?;
    let service_binding = KeyPackagesClaimServiceBinding {
        source_service_id: DidCoreId::new(source_service_id.to_owned()).with_context(|| {
            format!("invalid Arkret KeyPackage claim source Service DID '{source_service_id}'")
        })?,
        destination_service_id: DidCoreId::new(destination_service_id.to_owned()).with_context(
            || {
                format!(
                    "invalid Arkret KeyPackage claim destination Service DID \
                     '{destination_service_id}'"
                )
            },
        )?,
    };
    let request = KeyPackagesClaimRequestBody {
        claim_request_id,
        target_principal_id,
        requester,
        intended_realm_id,
        mls_group_id,
        claim_purpose,
        required_capabilities,
        claim_nonce,
        expires_at,
        target_device_ids,
        target_keypackage_ref: None,
        target_agent_id: None,
        target_agent_verification_method: None,
        target_agent_key_authorize_event_id: None,
        minimal_metadata_allowed: None,
        timeout_ms,
        strand_id,
        pair_key: None,
        last_resort_allowed: None,
        service_binding,
        requester_authorization,
    };
    request
        .validate_shape()
        .map_err(|error| anyhow::anyhow!("Arkret KeyPackage claim request shape: {error}"))?;
    Ok(request)
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
        principal_did: DidCoreId,
        verification_method: &str,
        agent_key_authorization_ref: &str,
        requested_scope: Vec<String>,
        audience: &str,
        device_id: DeviceId,
        realm_id: Option<&str>,
    ) -> anyhow::Result<(ArkretAgentSessionProvider, ArkretSession)> {
        validate_agent_key_ref(key_ref)?;
        let audience = DidCoreId::new(audience.to_owned())
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
        let unsigned_request = UnsignedAgentSessionGrantRequest::new(
            principal_did.clone(),
            device_id.clone(),
            requested_scope,
            agent_key_authorization_ref.to_owned(),
            agent_scope_request,
            None,
            dpop_binding_proof,
            None,
            arkret::UnsignedAgentSessionGrantProof {
                challenge,
                audience: audience.clone(),
                expires_at,
                verification_method: verification_method.clone(),
                nonce,
            },
        )
        .map_err(|err| anyhow::anyhow!("author agent_key_proof request: {err}"))?;
        let signing_bytes = unsigned_request
            .canonical_signing_bytes()
            .map_err(|err| anyhow::anyhow!("agent_key_proof canonical bytes: {err}"))?;
        let signature = NonEmptyString::new(arkret::base64url_encode(
            runtime_signing_key.sign(&signing_bytes).to_bytes(),
        ))
        .map_err(|err| anyhow::anyhow!("agent_key_proof signature: {err}"))?;
        let login_request = unsigned_request
            .attach_signature(signature)
            .map_err(|err| anyhow::anyhow!("attach agent_key_proof signature: {err}"))?;
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
            request: None,
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
            .login_request(login_request, Utc::now())
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
        principal_did: DidCoreId,
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

    /// Build the initial-publication wrapper for an exact signed Event.
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

    /// Submit one validated initial-publication wrapper.
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
        .auth(Auth::Dpop(DpopAuth::with_dpop_token(
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
            principal_id: DidCoreId::new("ak:did_core:webvh:z6mkfixture").unwrap(),
            device_id: Some(
                DeviceId::new("ak:device:0196419b-0000-7000-8000-000000000001").unwrap(),
            ),
            grant_id: arkret_wire::SessionGrantId::from_issuance_digest([0x11; 32]),
            grant_jwt: "agent.grant.jwt".to_owned(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
            audience: DidCoreId::new("ak:did_core:webvh:z6mkservice").unwrap(),
            granted_scope: vec!["ak.self.events.stream.subscribe".to_owned()],
            session_public_key: Some("session-public-key".to_owned()),
            dpop_jkt: Some("agent-dpop-jkt".to_owned()),
        }
    }

    #[test]
    fn agent_session_refresh_mints_valid_runtime_key_proof() {
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
        assert_eq!(first.context, arkret::AgentSessionRefreshProofContext::V1);
        assert_eq!(first.audience, state.audience);
        assert_eq!(first.verification_method, verification_method);
        assert_eq!(
            first.request_canonical_digest,
            arkret::agent_session_refresh_request_digest(
                &state.grant_jwt,
                &state.principal_id,
                device_id,
                &state.audience,
                &first.verification_method,
            )
            .unwrap()
        );

        let bytes = first.canonical_signing_bytes().unwrap();
        let signature =
            Signature::from_slice(&arkret::base64url_decode(first.signature.as_str()).unwrap())
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
        // Assert against the SDK constant: the DPoP proof algorithm name is
        // generated from the spec registry, so hard-coding it here silently
        // rots when the registry moves (it did: `EdDSA` -> `Ed25519`).
        assert_eq!(protected["alg"], arkret::dpop::DPOP_PROOF_ALG);
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
        let verification_method = "did:webvh:z6mkfixture:alice.example#runtime-key-1";
        let authorization = PeerKeyPackageRequesterAuthorization::NativeAgent {
            verification_method: DidUrl::new(verification_method).unwrap(),
            requester_agent_id: DidCoreId::new("did:webvh:z6mkfixture:alice.example").unwrap(),
            agent_key_authorize_event_id: arkret::EventId::new(
                "ak:event:01904100-0000-7000-8000-000000000011",
            )
            .unwrap(),
            signed_at: Utc::now(),
            signature: KeyOperationSignature {
                kid: arkret::NonEmptyString::new(verification_method).unwrap(),
                signature_algorithm: Some(arkret::NonEmptyString::new("Ed25519").unwrap()),
                sig: arkret::Base64UrlString::new("AQ").unwrap(),
            },
        };
        let request = build_mls_key_packages_claim_request(
            "AQEBAQEBAQEBAQEBAQEBAQ".to_owned(),
            "did:webvh:z6mkfixture:bob.example",
            "ak:realm:01904100-0000-8000-8000-000000000001",
            "did:webvh:z6mkfixture:alice.example",
            PeerKeyPackageClaimPurpose::RealmMembership,
            &["mimi.content.v1".to_owned(), "ak.content.v1".to_owned()],
            "AQEBAQEBAQEBAQEBAQEBAQ".to_owned(),
            Utc::now() + chrono::Duration::minutes(5),
            &["ak:device:01904100-0000-7000-8000-00000000000e".to_owned()],
            Some("ak:strand:01904100-0000-8000-8000-000000000002"),
            "group-1",
            Some(1500),
            "did:webvh:z6mkfixture:service.example",
            "did:webvh:z6mkfixture:peer-service.example",
            authorization,
        )
        .expect("claim request should build");

        assert_eq!(
            request.target_principal_id.as_str(),
            "did:webvh:z6mkfixture:bob.example"
        );
        assert_eq!(request.claim_request_id.as_str(), "AQEBAQEBAQEBAQEBAQEBAQ");
        assert_eq!(
            request.claim_purpose,
            PeerKeyPackageClaimPurpose::RealmMembership
        );
        assert_eq!(request.required_capabilities.len(), 2);
        assert_eq!(request.target_device_ids.len(), 1);
        assert_eq!(
            request.strand_id.as_ref().map(StrandId::as_str),
            Some("ak:strand:01904100-0000-8000-8000-000000000002")
        );
        assert_eq!(request.mls_group_id.as_str(), "group-1");
        assert_eq!(
            request.service_binding.source_service_id.as_str(),
            "did:webvh:z6mkfixture:service.example"
        );
        let PeerKeyPackageRequesterAuthorization::NativeAgent {
            verification_method: authorized_method,
            ..
        } = &request.requester_authorization
        else {
            panic!("requester authorization variant must round-trip");
        };
        assert_eq!(authorized_method.as_str(), verification_method);
    }

    #[test]
    fn mls_welcome_claim_envelope_signing_uses_sdk_transcript() {
        let mut envelope = MlsWelcomeClaimEnvelope {
            keypackage_ref: "ak:mls:keypackage:test".to_owned(),
            keypackage_digest: arkret::Hash::new(format!("sha256:{}", "aa".repeat(32))).unwrap(),
            intended_realm_id: RealmId::new(
                "ak:realm:01904100-0000-8000-8000-000000000001".to_owned(),
            )
            .unwrap(),
            claim_id: arkret::NonEmptyString::new("ak:claim:test").unwrap(),
            requester_actor_id: DidCoreId::new("did:webvh:z6mkfixture:alice.example".to_owned())
                .unwrap(),
            trust_binding: arkret::MlsRequesterTrustBinding::RequesterDevice {
                requester_device_id: DeviceId::new(
                    "ak:device:01904100-0000-7000-8000-000000000001".to_owned(),
                )
                .unwrap(),
                requester_device_authorize_event_id: arkret::EventId::new(
                    "ak:event:01904100-0000-7000-8000-000000000009".to_owned(),
                )
                .unwrap(),
            },
            nonce: arkret::NonEmptyString::new("nonce-1").unwrap(),
            welcome_digest: arkret::Hash::new(format!("sha256:{}", "bb".repeat(32))).unwrap(),
            created_at: Utc::now(),
            signature: KeyOperationSignature {
                kid: arkret::NonEmptyString::new("pending").unwrap(),
                signature_algorithm: None,
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
