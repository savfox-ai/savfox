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
    AccountSubscribeFrame, DeviceId, Did, Ed25519MoveSigner, Event, EventsSubmitOutcome,
    EventsSubscribeFrame, KeyOperationSignature, KeyPackagesClaimOutcome,
    KeyPackagesClaimRequestBody, MlsWelcomeClaimEnvelope, RealmId, ServerDescription,
    SessionGrantDpopBindingProof, StrandId, SyncRequestBody,
};
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
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

use super::session::{ArkretSession, login_with_signer};
use super::signer::{ArkretKeyRef, load_ed25519_signing_key};

const SESSION_GRANT_PATH: &str = "/_arkret/gate/account/session-grants";

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct ArkretHttpClient {
    inner: Client,
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
    signing_key: Arc<SigningKey>,
}

impl SessionGrantTransport for AgentSessionGrantTransport {
    fn issue_session_grant<'a>(
        &'a self,
        request: arkret::SessionGrantRequestBody,
    ) -> BoxSessionFuture<'a, arkret::SessionGrantOutcome> {
        Box::pin(async move { self.bootstrap.auth_issue_session_grant(&request).await })
    }

    fn refresh_session_grant<'a>(
        &'a self,
        request: arkret::SessionGrantRefreshRequestBody,
    ) -> BoxSessionFuture<'a, arkret::SessionGrantRefreshOutcome> {
        Box::pin(async move {
            let client = build_dpop_client(
                self.grant_base_url.clone(),
                Arc::clone(&self.signing_key),
                request.grant_jwt.clone(),
            )?;
            client.auth_refresh_session_grant(&request).await
        })
    }
}

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct AgentAuthenticatedTransportFactory {
    base_url: Url,
    signing_key: Arc<SigningKey>,
}

impl AuthenticatedTransportFactory for AgentAuthenticatedTransportFactory {
    type Transport = Client;

    fn build(&self, state: &SessionGrantState) -> arkret::Result<Self::Transport> {
        build_dpop_client(
            self.base_url.clone(),
            Arc::clone(&self.signing_key),
            state.grant_jwt.clone(),
        )
    }

    fn refresh_options(
        &self,
        state: &SessionGrantState,
        fallback: &SessionRefreshOptions,
    ) -> arkret::Result<SessionRefreshOptions> {
        Ok(SessionRefreshOptions {
            audience: Some(state.audience.clone()),
            device_id: state
                .device_id
                .clone()
                .or_else(|| fallback.device_id.clone()),
            proof: fallback.proof.clone(),
            expected_dpop_jkt: state.dpop_jkt.clone(),
        })
    }
}

pub type ArkretAgentSessionProvider = SessionTransportProvider<
    AgentSessionGrantTransport,
    AgentAuthenticatedTransportFactory,
    NoopSessionGrantStore,
>;

pub fn sign_key_operation_value(
    key_ref: &ArkretKeyRef,
    verification_method: &str,
    context: &str,
    value: &Value,
) -> anyhow::Result<KeyOperationSignature> {
    let verification_method = verification_method.trim();
    if verification_method.is_empty() {
        anyhow::bail!("Arkret key operation signature missing verification method");
    }
    let context = context.trim();
    if context.is_empty() {
        anyhow::bail!("Arkret key operation signature missing context");
    }
    let signing_key = load_ed25519_signing_key(key_ref)?;
    let canonical = arkret::canonical::canonical_json_bytes(value)
        .map_err(|err| anyhow::anyhow!("Arkret key operation canonical JSON: {err}"))?;
    let mut signing_input = Vec::with_capacity(context.len() + 1 + canonical.len());
    signing_input.extend_from_slice(context.as_bytes());
    signing_input.push(b'\n');
    signing_input.extend_from_slice(&canonical);
    let signature = signing_key.sign(&signing_input);
    Ok(KeyOperationSignature {
        kid: verification_method.to_owned(),
        alg: Some("Ed25519".to_owned()),
        sig: arkret::base64url_encode(signature.to_bytes()),
    })
}

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
    envelope.signature = KeyOperationSignature {
        kid: verification_method.to_owned(),
        alg: Some("Ed25519".to_owned()),
        sig: arkret::base64url_encode(signature.to_bytes()),
    };
    Ok(())
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
        Self { inner }
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
        Ok(Self { inner })
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
        device_id: Option<DeviceId>,
        realm_id: Option<&str>,
    ) -> anyhow::Result<(ArkretAgentSessionProvider, ArkretSession)> {
        validate_agent_key_ref(key_ref)?;
        // Session grants can exceed the Windows Credential Manager 2560-byte
        // secret limit. Keep the short-lived grant in the provider's memory;
        // the long-lived runtime signing key remains keyring-backed.
        let grant_store = NoopSessionGrantStore;
        let resource_url =
            Url::parse(base_url).with_context(|| format!("invalid Arkret base_url: {base_url}"))?;
        let grant_base_url = discover_account_authority_base_url(&resource_url).await?;
        let signing_key = Arc::new(load_ed25519_signing_key(key_ref)?);
        let grant_htu = joined_htu(&grant_base_url, SESSION_GRANT_PATH)?;
        let binding_proof = build_dpop_header(&signing_key, "POST", grant_htu.clone(), None)?;
        let bootstrap = ClientBuilder::new(grant_base_url.clone())
            .auth(Auth::Dpop(DpopAuth::proof_only({
                let expected_htu = grant_htu.clone();
                let proof_jwt = binding_proof.clone();
                move |request| {
                    if request.method != "POST"
                        || request.htu != expected_htu
                        || request.access_token.is_some()
                    {
                        return Err(arkret::Error::Protocol(
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
        let agent_scope_request = if let Some(realm_id) = realm_id {
            json!({ "realm_ids": [realm_id] })
        } else {
            json!({})
        };
        let dpop_binding_proof = SessionGrantDpopBindingProof {
            proof_jwt: binding_proof,
        };
        let signing_input = arkret::agent::agent_key_proof_signing_input_for_session_grant(
            &principal_did,
            &requested_scope,
            agent_key_authorization_ref,
            &agent_scope_request,
            &dpop_binding_proof,
            verification_method.to_owned(),
            challenge.clone(),
            nonce.clone(),
            audience.to_owned(),
            expires_at,
        )
        .map_err(|err| anyhow::anyhow!("agent_key_proof signing input: {err}"))?;
        let signature = signing_key.sign(
            &signing_input
                .canonical_bytes()
                .map_err(|err| anyhow::anyhow!("agent_key_proof canonical bytes: {err}"))?,
        );
        let signature = arkret::base64url_encode(signature.to_bytes());
        let login = AgentKeyProofLogin {
            principal_id: principal_did.clone(),
            requested_scope,
            agent_key_authorization_ref: agent_key_authorization_ref.to_owned(),
            agent_scope_request,
            dpop_binding_proof,
            verification_method: verification_method.to_owned(),
            challenge,
            nonce,
            audience: audience.to_owned(),
            expires_at,
            signature,
        };
        let session_transport = AgentSessionGrantTransport {
            grant_base_url,
            bootstrap,
            signing_key: Arc::clone(&signing_key),
        };
        let factory = AgentAuthenticatedTransportFactory {
            base_url: resource_url,
            signing_key,
        };
        let refresh_options = SessionRefreshOptions {
            audience: Some(audience.to_owned()),
            device_id,
            proof: None,
            expected_dpop_jkt: None,
        };
        let restored = SessionTransportProvider::restore(
            session_transport.clone(),
            factory.clone(),
            refresh_options.clone(),
            grant_store.clone(),
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
            None,
            realm_id,
        )
        .await?;
        let inner = provider
            .provide()
            .await
            .map_err(|error| anyhow::anyhow!("build authenticated Arkret client: {error}"))?;
        Ok((Self { inner }, session))
    }

    /// Construct an applet HTTP client by running DID-proof login.
    ///
    /// Builds an unauthenticated underlying `Client`, runs the applet
    /// DID-proof grant exchange, then rebuilds the authenticated `Client`
    /// carrying the `Authorization: Bearer <grant>` header. This is not the
    /// personal-agent runtime path.
    pub async fn login(
        base_url: &str,
        signer: &Ed25519MoveSigner,
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
        Ok((Self { inner }, session))
    }

    /// `GET /api/v1/server/describe` — used at startup to verify the target
    /// server and pin the service DID.
    pub async fn server_describe(&self) -> anyhow::Result<ServerDescription> {
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
            wait_for: None,
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

    /// `POST /api/v1/events` — submit one signed Event Envelope.
    pub async fn submit_event(&self, event: &Event) -> anyhow::Result<EventsSubmitOutcome> {
        self.inner
            .events_submit(event)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
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

fn agent_session_exchange_error(error: arkret::Error) -> anyhow::Error {
    let reason = match &error {
        arkret::Error::Api { error, .. } => error
            .details()
            .get("reason_code")
            .and_then(serde_json::Value::as_str)
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
    anyhow::anyhow!("agent_key_proof session grant exchange failed ({reason}): {action}: {error}")
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
) -> arkret::Result<Client> {
    ClientBuilder::new(base_url)
        .auth(Auth::Dpop(DpopAuth::with_access_token(
            access_token,
            move |request| {
                arkret::dpop::build_dpop_proof(&request, &signing_key)
                    .map(|proof| proof.header_value)
            },
        )))
        .build()
}

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
    arkret::dpop::build_dpop_proof(&request, signing_key).map(|proof| proof.header_value)
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
    let discovery = ClientBuilder::new(resource_url.clone())
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
            claim_id: "ak:claim:test".to_owned(),
            requester_did: Did::new("did:webvh:z6mkfixture:alice.example".to_owned()).unwrap(),
            ssk_generation: None,
            requester_device_id: Some("ak:device:01904100-0000-7000-8000-000000000001".to_owned()),
            nonce: "nonce-1".to_owned(),
            welcome_digest: arkret::Hash::new(format!("sha256:{}", "bb".repeat(32))).unwrap(),
            created_at: Utc::now(),
            signature: KeyOperationSignature {
                kid: String::new(),
                alg: None,
                sig: String::new(),
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

        let sig =
            Signature::from_slice(&arkret::base64url_decode(&envelope.signature.sig).unwrap())
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
            let rendered = agent_session_exchange_error(arkret::Error::Api {
                status: 412,
                error: Box::new(envelope),
            })
            .to_string();
            assert!(rendered.contains(reason), "{rendered}");
            assert!(rendered.contains(expected), "{rendered}");
        }
    }
}
