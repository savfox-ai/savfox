//! Thin wrapper around [`arkret::http_client::Client`] for Arkret agent traffic.
//!
//! All HTTP / retry / canonical-bytes / NDJSON line splitting logic lives in
//! the upstream SDK; this type only exists so the gateway runtime never has
//! to think about constructing the underlying client.
//!
//! Agent mode uses `agent_key_proof` to mint a short-lived
//! `ak.session.grant`, then presents that grant with a fresh DPoP proof on
//! every protected self-surface call.

use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context;
use arkret::http_client::{Auth, Client, ClientBuilder, DpopAuth};
use arkret::sync::{AccountSubscribeFrame, SyncRequestBody};
use arkret::{
    AgentSessionGrantRefreshRequest, AgentSessionRefreshProof, AgentSessionRefreshProofContext,
    AuthoredEvent, AuthoritySubmitOutcome, AuthoritySubmitRequest, Base64UrlString, DeviceId,
    DidCoreId, DidUrl, EventAdmissionSubmission, EventId, KeyPackagesClaimOutcome,
    KeyPackagesClaimRequestBody, KeyPackagesClaimServiceBinding, NonEmptyString,
    PeerKeyPackageClaimPurpose, PeerKeyPackageRequesterAuthorization, RealmId, ServiceDescribe,
    SessionGrantDpopBindingProof, SessionGrantRefreshRequestBody, StrandId,
    UnsignedAgentSessionGrantRequest, UnsignedAgentSessionRefreshProof,
};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer as _, SigningKey};
use futures_util::Stream;
use garth::session::BoxSessionFuture;
use garth::{
    AuthenticatedTransportFactory, NoopSessionGrantStore, SessionEngine, SessionGrantState,
    SessionGrantStore, SessionGrantTransport, SessionRefreshOptions, SessionTransportProvider,
};
use url::Url;

use super::session::ArkretSession;
use super::signer::{ArkretKeyRef, load_ed25519_signing_key};

const SESSION_GRANT_PATH: &str = "/_arkret/gate/account/session-grants";

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct ArkretHttpClient {
    inner: Client,
}

/// Stream of account-level subscribe frames yielded by
/// [`ArkretHttpClient::account_subscribe_stream`].
pub type ArkretAccountFrameStream =
    Pin<Box<dyn Stream<Item = Result<AccountSubscribeFrame, anyhow::Error>> + Send>>;

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
    ) -> BoxSessionFuture<'a, arkret::SessionGrantOutcome> {
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
    principal_id: DidCoreId,
    agent_key_authorization_ref: EventId,
    runtime_signing_key: Arc<SigningKey>,
    dpop_signing_key: Arc<SigningKey>,
    dpop_jkt: String,
    verification_method: DidUrl,
    requested_scope: Vec<String>,
}

fn mint_agent_session_refresh_proof(
    state: &SessionGrantState,
    agent_key_authorization_ref: &EventId,
    verification_method: &DidUrl,
    signing_key: &SigningKey,
) -> garth::Result<AgentSessionRefreshProof> {
    let request_canonical_digest = arkret::agent_session_refresh_request_digest(
        &state.grant_jwt,
        &state.account_id.principal_id,
        agent_key_authorization_ref,
        &state.audience_id,
        verification_method,
    )
    .map_err(|error| garth::Error::Protocol(error.to_string()))?;
    let issued_at = Utc::now();
    let expires_at = issued_at + chrono::Duration::seconds(60);
    let unsigned = UnsignedAgentSessionRefreshProof {
        context: AgentSessionRefreshProofContext::V1,
        request_canonical_digest,
        audience_id: state.audience_id.clone(),
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

fn sign_agent_session_request(
    request: UnsignedAgentSessionGrantRequest,
    signing_key: &SigningKey,
) -> anyhow::Result<arkret::SessionGrantRequestBody> {
    let proof = arkret::AgentSessionGrantProof {
        proof_kind: arkret::AgentSessionGrantProofKind::AgentKeyProof,
        challenge: request.proof.challenge.clone(),
        request_canonical_digest: request.canonical_request_digest()?,
        audience_id: request.proof.audience_id.clone(),
        issued_at: request.proof.issued_at,
        expires_at: request.proof.expires_at,
        verification_method: request.proof.verification_method.clone(),
        signature: String::new(),
    };
    let signing_bytes = proof.canonical_signing_bytes()?;
    let signature = NonEmptyString::new(arkret::base64url_encode(
        signing_key.sign(&signing_bytes).to_bytes(),
    ))
    .map_err(anyhow::Error::msg)?;
    Ok(request.attach_signature(signature)?)
}

impl AuthenticatedTransportFactory for AgentAuthenticatedTransportFactory {
    type Transport = Client;

    fn build(&self, state: &SessionGrantState) -> garth::Result<Self::Transport> {
        let observation = state.agent_participation_observation(Utc::now())?;
        if observation.agent_key_authorization_ref != self.agent_key_authorization_ref
            || observation.verification_method != self.verification_method
            || state.dpop_jkt.as_deref() != Some(self.dpop_jkt.as_str())
        {
            return Err(garth::Error::Protocol(
                "Agent observation has another runtime authorization or DPoP key".into(),
            ));
        }
        if state.account_id.principal_id != self.principal_id
            || state.expires_at <= Utc::now()
            || !savfox_gateway_shared::arkret::session_scope_matches_request(
                &self.requested_scope,
                &state.granted_scope,
            )
        {
            return Err(garth::Error::Protocol(
                "Agent session grant expired, widened scope, or omitted the runtime floor"
                    .to_owned(),
            ));
        }
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
        if state.account_id.principal_id != self.principal_id {
            return Err(garth::Error::Protocol(
                "Agent refresh principal differs from its runtime".to_owned(),
            ));
        }
        let proof = mint_agent_session_refresh_proof(
            state,
            &self.agent_key_authorization_ref,
            &self.verification_method,
            &self.runtime_signing_key,
        )?;
        Ok(SessionRefreshOptions {
            request: Some(SessionGrantRefreshRequestBody::Agent(
                AgentSessionGrantRefreshRequest {
                    grant_jwt: state.grant_jwt.clone(),
                    audience_id: Some(state.audience_id.clone()),
                    principal_id: self.principal_id.clone(),
                    agent_key_authorization_ref: self.agent_key_authorization_ref.clone(),
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

#[allow(clippy::too_many_arguments)]
pub fn build_mls_key_packages_claim_request(
    claim_request_id: String,
    target_principal_id: &str,
    intended_realm_id: &str,
    requester: &str,
    claim_purpose: PeerKeyPackageClaimPurpose,
    required_capabilities: &[String],
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
    let mls_group_id =
        arkret::MlsGroupId::new(mls_group_id.trim().to_owned()).map_err(|error| {
            anyhow::anyhow!("invalid Arkret KeyPackage claim MLS group id: {error}")
        })?;
    let service_binding = KeyPackagesClaimServiceBinding {
        source_id: DidCoreId::new(source_service_id.to_owned()).with_context(|| {
            format!("invalid Arkret KeyPackage claim source Service DID '{source_service_id}'")
        })?,
        destination_id: DidCoreId::new(destination_service_id.to_owned()).with_context(|| {
            format!(
                "invalid Arkret KeyPackage claim destination Service DID \
                     '{destination_service_id}'"
            )
        })?,
    };
    let target_account_id =
        arkret::AccountId::new(target_principal_id, service_binding.destination_id.clone());
    let requester_account_id = matches!(
        &requester_authorization,
        PeerKeyPackageRequesterAuthorization::Device { .. }
    )
    .then(|| arkret::AccountId::new(requester, service_binding.source_id.clone()));
    let request = KeyPackagesClaimRequestBody {
        claim_request_id,
        target_account_id: Some(target_account_id),
        requester_account_id,
        intended_realm_id,
        mls_group_id,
        claim_purpose,
        required_capabilities,
        expires_at,
        target_device_ids,
        target_keypackage_ref: None,
        target_agent_id: None,
        target_agent_verification_method: None,
        target_agent_key_authorize_event_id: None,
        target_pairwise_verification_method: None,
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
        Self { inner }
    }

    /// Build an applet HTTP client bound to `base_url`, authenticated via the
    /// Applet bearer token. Agent account runtimes use
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

    /// Construct an Agent HTTP client by exchanging a runtime-key
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
        realm_id: Option<&str>,
    ) -> anyhow::Result<(ArkretAgentSessionProvider, ArkretSession)> {
        savfox_gateway_shared::arkret::validate_agent_runtime_scope(&requested_scope)
            .map_err(anyhow::Error::msg)?;
        let expected_scope = requested_scope.clone();
        let agent_key_authorization_ref = EventId::new(agent_key_authorization_ref.to_owned())
            .context("invalid accepted Agent key authorization Event id")?;
        validate_agent_key_ref(key_ref)?;
        let audience = DidCoreId::new(audience.to_owned())
            .with_context(|| format!("invalid Arkret service audience DID '{audience}'"))?;
        let verification_method = DidUrl::new(verification_method.to_owned()).map_err(|err| {
            anyhow::anyhow!("invalid Arkret verification method '{verification_method}': {err}")
        })?;
        arkret_signatures::agent::validate_agent_verification_method(
            &principal_did,
            &verification_method,
        )
        .map_err(|error| anyhow::anyhow!("Agent session identity binding: {error}"))?;
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
            dpop_signing_key.as_ref(),
        )?;
        let dpop_jkt = binding_proof.jkt.clone();
        let binding_proof = binding_proof.header_value;
        let bootstrap = agent_client_builder(grant_base_url.clone())?
            .auth(Auth::Dpop(DpopAuth::proof_only({
                let expected_htu = grant_htu.clone();
                let holder_key = Arc::clone(&dpop_signing_key);
                move |request| {
                    if request.method != "POST"
                        || request.htu != expected_htu
                        || request.access_token.is_some()
                    {
                        return Err(arkret::http_client::Error::Protocol(
                            "unexpected DPoP kickoff request shape".to_owned(),
                        ));
                    }
                    arkret::dpop::build_dpop_proof(
                        &arkret::dpop::DpopProofRequest::new("POST", expected_htu.clone()),
                        holder_key.as_ref(),
                    )
                    .map(|proof| proof.header_value)
                    .map_err(|error| arkret::http_client::Error::Protocol(error.to_string()))
                }
            })))
            .build()
            .map_err(|err| anyhow::anyhow!("agent session bootstrap HTTP client: {err}"))?;

        let issued_at = Utc::now();
        let expires_at = issued_at + chrono::Duration::minutes(5);
        let challenge = arkret::base64url_encode(rand::random::<[u8; 32]>());
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
            requested_scope,
            agent_key_authorization_ref.clone(),
            agent_scope_request,
            None,
            dpop_binding_proof,
            arkret::UnsignedAgentSessionGrantProof {
                challenge,
                audience_id: audience.clone(),
                issued_at,
                expires_at,
                verification_method: verification_method.clone(),
            },
        )
        .map_err(|err| anyhow::anyhow!("author agent_key_proof request: {err}"))?;
        let login_request = sign_agent_session_request(unsigned_request, &runtime_signing_key)?;
        let session_transport = AgentSessionGrantTransport {
            grant_base_url,
            bootstrap,
            dpop_signing_key: Arc::clone(&dpop_signing_key),
        };
        let factory = AgentAuthenticatedTransportFactory {
            base_url: resource_url,
            principal_id: principal_did.clone(),
            agent_key_authorization_ref,
            runtime_signing_key,
            dpop_signing_key,
            dpop_jkt: dpop_jkt.clone(),
            verification_method: verification_method.clone(),
            requested_scope: expected_scope,
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
            if state.account_id.principal_id == principal_did
                && state.audience_id == audience
                && state.expires_at > Utc::now()
            {
                factory
                    .build(&state)
                    .map_err(|error| anyhow::anyhow!("restored Agent grant scope: {error}"))?;
                let session = arkret_session_from_state(&state)?;
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
        factory
            .build(&state)
            .map_err(|error| anyhow::anyhow!("issued Agent grant scope: {error}"))?;

        let provider = SessionTransportProvider::with_store(
            session_engine,
            factory,
            refresh_options,
            grant_store,
        )
        .await
        .map_err(|error| anyhow::anyhow!("persist agent session grant: {error}"))?;
        Ok((provider, arkret_session_from_state(&state)?))
    }

    /// `GET /_arkret/describe` — used at startup to verify the target
    /// server and pin the service DID.
    pub async fn server_describe(&self) -> anyhow::Result<ServiceDescribe> {
        self.inner
            .describe()
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }

    /// `GET /_arkret/self/account/subscribe` — returns a user-scoped account
    /// stream. Agent runtimes consume this instead of binding the
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
            realm_list: None,
            replace_filter: None,
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

    /// Freeze an exact signed producer Event for current authority admission.
    pub fn prepare_submission(
        &self,
        event: &AuthoredEvent,
    ) -> anyhow::Result<EventAdmissionSubmission> {
        event.verify_identity()?;
        event.verify_producer_proof_self_consistency(event.digest_suite())?;
        let submission = EventAdmissionSubmission::new(event.event().clone());
        submission.validate()?;
        Ok(submission)
    }

    /// Submit over the authenticated self surface and require exact Commit
    /// coverage of this Event and its security-scope stream.
    pub async fn submit_submission(
        &self,
        submission: &EventAdmissionSubmission,
    ) -> anyhow::Result<AuthoritySubmitOutcome> {
        submission.validate()?;
        let outcome = self
            .inner
            .submit_event(submission)
            .await
            .map_err(anyhow::Error::from)?;
        outcome.validate_for_request(&AuthoritySubmitRequest::Event(submission.clone()))?;
        Ok(outcome)
    }

    pub async fn submit_event(
        &self,
        event: &AuthoredEvent,
    ) -> anyhow::Result<AuthoritySubmitOutcome> {
        let submission = self.prepare_submission(event)?;
        self.submit_submission(&submission).await
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
            .extensions
            .get("reason_code")
            .and_then(serde_json::Value::as_str)
            .or_else(|| reason_code_from_message(&error.detail))
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
        "capability_denied" => {
            "the requested runtime permissions are not approved; review the Agent permissions in Inkson before pairing again"
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
/// rejection reason in the Problem `detail` string (`reason_code=...`) while
/// keeping the problem type at the broad `failed_precondition` category.
/// Accept that wire-compatible representation as well as the preferred
/// structured extension so callers never have to parse the rendered `anyhow`
/// chain.
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
        anyhow::bail!("Arkret Agent session keys must use key_ref kind=keyring")
    }
}

fn arkret_session_from_state(state: &SessionGrantState) -> anyhow::Result<ArkretSession> {
    Ok(ArkretSession {
        session_grant: state.grant_jwt.clone(),
        expires_at: state.expires_at,
        principal_did: state.account_id.principal_id.clone(),
        device_id: state.device_id.clone(),
        participation_observation: Some(state.agent_participation_observation(Utc::now())?),
    })
}

fn build_dpop_client(
    base_url: Url,
    signing_key: Arc<SigningKey>,
    access_token: String,
) -> garth::Result<Client> {
    agent_client_builder(base_url)
        .map_err(|error| garth::Error::Http(error.to_string()))?
        .auth(Auth::Dpop(DpopAuth::with_dpop_token(
            access_token,
            move |request| {
                arkret::dpop::build_dpop_proof(&request, signing_key.as_ref())
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
    let discovery = agent_client_builder(resource_url.clone())?
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
    let authority_url = Url::parse(authority.origin.as_str()).with_context(|| {
        format!(
            "invalid Arkret account authority origin: {}",
            authority.origin
        )
    })?;
    let advertised_gate = Url::parse(&authority.gate_account_base_url).with_context(|| {
        format!(
            "invalid Arkret account authority gate_account_base_url: {}",
            authority.gate_account_base_url
        )
    })?;
    let expected_gate = joined_htu(&authority_url, "/_arkret/gate/account")?;
    if advertised_gate.as_str().trim_end_matches('/') != expected_gate.trim_end_matches('/') {
        anyhow::bail!(
            "Arkret account authority metadata mismatch: origin '{}' does not own              gate_account_base_url '{}'",
            authority.origin,
            authority.gate_account_base_url
        );
    }
    Ok(authority_url)
}

fn agent_client_builder(base_url: Url) -> anyhow::Result<ClientBuilder> {
    // Agent conformance stacks run the resource and account authority
    // on loopback. The SDK keeps plaintext HTTP rejected for every non-loopback
    // host, including when this opt-in is enabled.
    let http = savfox_http_client::custom_ca::build_reqwest_client_with_custom_ca(
        reqwest::Client::builder(),
    )
    .context("build Arkret Agent HTTP transport")?;
    Ok(ClientBuilder::new(base_url)
        .http_client(http)
        .allow_insecure_localhost())
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

    fn authorization_event() -> EventId {
        EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [19; 32])
    }

    fn agent_session_state() -> SessionGrantState {
        let public =
            arkret::base64url_encode(SigningKey::from_bytes(&[44; 32]).verifying_key().to_bytes());
        let jwk = arkret_models_identity_fixture_jwk(public);
        let at = arkret::canonical::normalize_timestamp_canonical(Utc::now());
        let mut state = SessionGrantState {
            account_id: arkret::AccountId::new(
                DidCoreId::new("ak:did_core:web:agent.example").unwrap(),
                DidCoreId::new("ak:did_core:web:station.example").unwrap(),
            ),
            device_id: None,
            grant_id: arkret_wire::SessionGrantId::from_issuance_digest([0x11; 32]),
            grant_jwt: String::new(),
            expires_at: at + chrono::Duration::minutes(5),
            audience_id: DidCoreId::new("ak:did_core:web:station.example").unwrap(),
            granted_scope: savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap(),
            session_public_key: Some(jwk.clone().into_string()),
            dpop_jkt: Some(jwk.thumbprint_sha256().unwrap()),
        };
        state.granted_scope.sort();
        let claims = serde_json::json!({"kind":"ak.session.grant","jti":state.grant_id,"issuer_id":state.audience_id,
            "issuance_nonce":"CwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsLCws","account_id":state.account_id,
            "session_public_key":jwk,"audience_id":state.audience_id,"scopes":state.granted_scope,
            "not_before":arkret::canonical::format_timestamp_canonical(at),"expires_at":arkret::canonical::format_timestamp_canonical(state.expires_at),
            "session_id":"runtime-session","credential_class":"standard","holder_binding":{"kind":"agent_runtime","agent_id":state.account_id.principal_id,
             "agent_key_authorization_ref":authorization_event(),"verification_method":"did:web:agent.example#runtime-1"},
            "proof_kind":"agent_key_proof","scope_details":{"controller_principal_id":"ak:did_core:web:controller.example","participation":[]}});
        let input = format!(
            "{}.{}",
            arkret::base64url_encode(br#"{"alg":"EdDSA"}"#),
            arkret::base64url_encode(serde_json::to_vec(&claims).unwrap())
        );
        state.grant_jwt = format!(
            "{input}.{}",
            arkret::base64url_encode(signing_key().sign(input.as_bytes()).to_bytes())
        );
        state
    }

    fn arkret_models_identity_fixture_jwk(public: String) -> arkret::CanonicalSessionPublicJwk {
        arkret::CanonicalSessionPublicJwk::new(
            serde_json::json!({"kty":"OKP","crv":"Ed25519","x":public}).to_string(),
        )
        .unwrap()
    }

    #[test]
    fn agent_scope_transport_factory_checks_every_initial_or_refreshed_grant() {
        let mut requested_scope =
            savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap();
        requested_scope.sort();
        let factory = AgentAuthenticatedTransportFactory {
            base_url: Url::parse("https://arkret.example.org").unwrap(),
            principal_id: agent_session_state().account_id.principal_id.clone(),
            agent_key_authorization_ref: authorization_event(),
            runtime_signing_key: Arc::new(signing_key()),
            dpop_signing_key: Arc::new(SigningKey::from_bytes(&[44; 32])),
            dpop_jkt: agent_session_state().dpop_jkt.clone().unwrap(),
            verification_method: DidUrl::new("did:web:agent.example#runtime-1").unwrap(),
            requested_scope: requested_scope.clone(),
        };
        let mut state = agent_session_state();
        state.granted_scope = requested_scope.clone();
        factory
            .build(&state)
            .expect("exact current grant builds a transport");
        for invalid in [
            {
                let mut scope = requested_scope.clone();
                scope.push(arkret::ServiceOperationId::SELF_REALM_READ_EXPORT_V1.to_owned());
                scope
            },
            {
                let mut scope = requested_scope.clone();
                scope.retain(|action| {
                    action != arkret::ServiceOperationId::SELF_COMMITTED_EVENT_READ_SCAN_V1
                });
                scope
            },
            vec!["ak.self.events.read.scan".to_owned()],
        ] {
            state.granted_scope = invalid;
            assert!(factory.build(&state).is_err());
        }
        state.granted_scope = requested_scope;
        state.expires_at = Utc::now() - chrono::Duration::seconds(1);
        assert!(factory.build(&state).is_err());
    }

    #[test]
    fn agent_session_refresh_mints_valid_runtime_key_proof() {
        let state = agent_session_state();
        let authorization_event = authorization_event();
        let verification_method =
            DidUrl::new("did:webvh:z6mkfixture:agent.example#runtime-key-1").unwrap();
        let first = mint_agent_session_refresh_proof(
            &state,
            &authorization_event,
            &verification_method,
            &signing_key(),
        )
        .expect("first refresh proof");
        assert_eq!(first.context, arkret::AgentSessionRefreshProofContext::V1);
        assert_eq!(first.audience_id, state.audience_id);
        assert_eq!(first.verification_method, verification_method);
        assert_eq!(
            first.request_canonical_digest,
            arkret::agent_session_refresh_request_digest(
                &state.grant_jwt,
                &state.account_id.principal_id,
                &authorization_event,
                &state.audience_id,
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
    fn agent_issuance_signs_closed_proof_and_authorization_request_digest() {
        let state = agent_session_state();
        let issued_at = Utc::now();
        let unsigned = UnsignedAgentSessionGrantRequest::new(
            state.account_id.principal_id.clone(),
            savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap(),
            authorization_event(),
            arkret::SessionGrantAgentScopeRequest {
                realm_ids: Vec::new(),
                strand_ids: Vec::new(),
                track_names: Vec::new(),
            },
            None,
            SessionGrantDpopBindingProof {
                proof_jwt: "unit-only-dpop-binding".to_owned(),
            },
            arkret::UnsignedAgentSessionGrantProof {
                challenge: arkret::base64url_encode([37; 32]),
                audience_id: state.audience_id.clone(),
                issued_at,
                expires_at: issued_at + chrono::Duration::minutes(5),
                verification_method: DidUrl::new("did:webvh:z6mkfixture#runtime-key-1").unwrap(),
            },
        )
        .unwrap();
        let expected_digest = unsigned.canonical_request_digest().unwrap();
        let request = sign_agent_session_request(unsigned, &signing_key()).unwrap();
        let arkret::SessionGrantRequestBody::Agent(request) = request else {
            panic!("Agent request expected")
        };
        assert_eq!(request.proof.request_canonical_digest, expected_digest);
        let bytes = request.proof.canonical_signing_bytes().unwrap();
        let signature =
            Signature::from_slice(&arkret::base64url_decode(&request.proof.signature).unwrap())
                .unwrap();
        signing_key()
            .verifying_key()
            .verify_strict(&bytes, &signature)
            .unwrap();
        let wire = serde_json::to_value(&request).unwrap();
        assert!(wire.get("device_id").is_none());
        assert_eq!(request.agent_key_authorization_ref, authorization_event());
        let mut tampered = request.proof;
        tampered.request_canonical_digest =
            arkret::Hash::new(arkret::canonical::canonical_sha256(&"changed request").unwrap())
                .unwrap();
        assert!(
            signing_key()
                .verifying_key()
                .verify_strict(&tampered.canonical_signing_bytes().unwrap(), &signature)
                .is_err()
        );
    }

    #[test]
    fn agent_refresh_uses_authorization_event_without_a_device_and_rejects_other_principal() {
        let state = agent_session_state();
        let factory = AgentAuthenticatedTransportFactory {
            base_url: Url::parse("https://arkret.example.org").unwrap(),
            principal_id: state.account_id.principal_id.clone(),
            agent_key_authorization_ref: authorization_event(),
            runtime_signing_key: Arc::new(signing_key()),
            dpop_signing_key: Arc::new(SigningKey::from_bytes(&[44; 32])),
            dpop_jkt: agent_session_state().dpop_jkt.clone().unwrap(),
            verification_method: DidUrl::new("did:webvh:z6mkfixture#runtime-key-1").unwrap(),
            requested_scope: savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap(),
        };
        let fallback = SessionRefreshOptions {
            request: None,
            expected_dpop_jkt: None,
        };
        let options = factory.refresh_options(&state, &fallback).unwrap();
        let Some(SessionGrantRefreshRequestBody::Agent(request)) = options.request else {
            panic!("Agent refresh expected")
        };
        request.validate().unwrap();
        assert_eq!(request.principal_id, state.account_id.principal_id);
        assert_eq!(request.agent_key_authorization_ref, authorization_event());
        assert!(
            serde_json::to_value(&request)
                .unwrap()
                .get("device_id")
                .is_none()
        );
        let other_authorization =
            EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [20; 32]);
        assert_ne!(
            request.agent_session_refresh_proof.request_canonical_digest,
            arkret::agent_session_refresh_request_digest(
                &state.grant_jwt,
                &state.account_id.principal_id,
                &other_authorization,
                &state.audience_id,
                &factory.verification_method
            )
            .unwrap()
        );
        let mut other = state;
        other.account_id.principal_id = DidCoreId::new("ak:did_core:web:other.example").unwrap();
        assert!(factory.refresh_options(&other, &fallback).is_err());
        assert!(factory.build(&other).is_err());
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
        let group_id = arkret::ScopeRef::Realm {
            realm_id: RealmId::new("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI")
                .unwrap(),
        }
        .canonical_mls_group_id()
        .unwrap();
        let authorization = PeerKeyPackageRequesterAuthorization::Agent {
            verification_method: DidUrl::new(verification_method).unwrap(),
            requester_agent_id: DidCoreId::new("ak:did_core:webvh:z6mkfixture").unwrap(),
            agent_key_authorize_event_id: arkret::EventId::new(
                "ak:event:AT3ARBdH1FM6GjXK9ulTx-YMvQOXys39dlUzZV6KyID9",
            )
            .unwrap(),
            signed_at: Utc::now(),
            signature: arkret::KeyOperationSignature {
                kid: arkret::NonEmptyString::new(verification_method).unwrap(),
                signature_algorithm: Some(arkret::NonEmptyString::new("Ed25519").unwrap()),
                sig: arkret::Base64UrlString::new("AQ").unwrap(),
            },
        };
        let request = build_mls_key_packages_claim_request(
            "AQEBAQEBAQEBAQEBAQEBAQ".to_owned(),
            "ak:did_core:webvh:z6mkbobfixture",
            "ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI",
            "ak:did_core:webvh:z6mkfixture",
            PeerKeyPackageClaimPurpose::RealmMembership,
            &["ak.content.v1".to_owned(), "mimi.content.v1".to_owned()],
            Utc::now() + chrono::Duration::minutes(5),
            &["ak:device:01904100-0000-7000-8000-00000000000e".to_owned()],
            Some("ak:strand:AT_TSQZlyY7Fu85J33nzo3fSau9RjJOeu21RspghP1gC"),
            group_id.as_str(),
            Some(1500),
            "ak:did_core:webvh:z6mkservicefixture",
            "ak:did_core:webvh:z6mkpeerservicefixture",
            authorization,
        )
        .expect("claim request should build");

        assert_eq!(
            request
                .target_account_id
                .as_ref()
                .expect("human target account")
                .principal_id
                .as_str(),
            "ak:did_core:webvh:z6mkbobfixture"
        );
        assert!(request.requester_account_id.is_none());
        assert_eq!(request.claim_request_id.as_str(), "AQEBAQEBAQEBAQEBAQEBAQ");
        assert_eq!(
            request.claim_purpose,
            PeerKeyPackageClaimPurpose::RealmMembership
        );
        assert_eq!(request.required_capabilities.len(), 2);
        assert_eq!(request.target_device_ids.len(), 1);
        assert_eq!(
            request.strand_id.as_ref().map(StrandId::as_str),
            Some("ak:strand:AT_TSQZlyY7Fu85J33nzo3fSau9RjJOeu21RspghP1gC")
        );
        assert_eq!(request.mls_group_id, group_id);
        assert_eq!(
            request.service_binding.source_id.as_str(),
            "ak:did_core:webvh:z6mkservicefixture"
        );
        let PeerKeyPackageRequesterAuthorization::Agent {
            verification_method: authorized_method,
            requester_agent_id,
            ..
        } = &request.requester_authorization
        else {
            panic!("requester authorization variant must round-trip");
        };
        assert_eq!(authorized_method.as_str(), verification_method);
        assert_eq!(requester_agent_id.as_str(), "ak:did_core:webvh:z6mkfixture");
    }

    #[test]
    fn agent_provider_requires_platform_keyring_reference() {
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
    fn agent_client_allows_only_insecure_loopback() {
        for url in ["http://127.0.0.1:8787", "http://localhost:8787"] {
            agent_client_builder(Url::parse(url).unwrap())
                .expect("Arkret Agent HTTP transport should build")
                .build()
                .expect("loopback HTTP should be available for local conformance stacks");
        }

        assert!(
            agent_client_builder(Url::parse("http://accounts.example:8787").unwrap())
                .expect("Arkret Agent HTTP transport should build")
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
            let envelope = arkret::Problem::from_code("failed_precondition", "rejected")
                .with_extension("reason_code", serde_json::json!(reason));
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
    fn agent_scope_structured_service_reasons_keep_recovery_layers() {
        for (reason, recovery) in [
            (
                "agent_provision_scope_migration_required",
                "provision_new_agent",
            ),
            (
                "agent_key_scope_reauthorization_required",
                "reauthorize_key_within_provision_ceiling",
            ),
            (
                "agent_session_scope_refresh_required",
                "issue_session_within_provision_and_key_ceilings",
            ),
        ] {
            let envelope = arkret::Problem::from_code("failed_precondition", "rejected")
                .with_extension("reason_code", serde_json::json!(reason));
            let error = agent_session_exchange_error(garth::Error::Api {
                status: 412,
                error: Box::new(envelope),
            });
            let actual = agent_session_exchange_reason(&error).unwrap();
            assert_eq!(actual, reason);
            assert_eq!(
                savfox_gateway_shared::arkret::agent_scope_recovery(actual),
                Some(recovery)
            );
        }
    }

    #[test]
    fn agent_session_errors_preserve_message_encoded_reason_codes() {
        let envelope = arkret::Problem::from_code(
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
