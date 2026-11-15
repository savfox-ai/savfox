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
use std::sync::{Arc, Mutex};

use anyhow::Context;
use arkret::http_client::{Auth, Client, ClientBuilder, DpopAuth};
use arkret::sync::{AccountSubscribeFrame, AccountSubscribeParameters};
use arkret::{
    AgentSessionGrantRefreshRequestBody, AgentSessionGrantSigningInput, AgentSessionRefreshProof,
    AgentSessionRefreshProofContext, AuthoredEvent, AuthoritySubmitRequestBody,
    AuthoritySubmitResponseBody, Base64UrlString, DeviceId, DidCoreId, DidUrl,
    EventAdmissionSubmission, EventId, KeyPackagesClaimRequestBody, KeyPackagesClaimResponseBody,
    KeyPackagesClaimServiceBinding, NonEmptyString, PeerKeyPackageClaimPurpose,
    PeerKeyPackageRequesterAuthorization, RealmId, ServiceDescribe, SessionGrantDpopBindingProof,
    SessionGrantRefreshRequestBody, StrandId, UnsignedAgentSessionRefreshProof,
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
    pub(super) inner: Client,
    own_station: Option<arkret::http_client::own_station_results::OwnStationStateClient>,
    session_provider: Option<ArkretAgentSessionProvider>,
    station_binding: Option<arkret::StationConnectionBinding>,
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
    issuer_id: DidCoreId,
    issued: Arc<Mutex<std::collections::BTreeMap<arkret::SessionGrantRef, AcceptedAgentIssue>>>,
}

/// Local provenance of a real public issuance response, never issuer-private metadata.
#[derive(Clone)]
struct AcceptedAgentIssue {
    request: arkret::AgentSessionGrantRequestBody,
    outcome: arkret::SessionGrantResponseBody,
}

impl AgentSessionGrantTransport {
    fn holder_public_key(&self) -> garth::Result<arkret::CanonicalSessionPublicJwk> {
        Ok(arkret::CanonicalSessionPublicJwk::new(
            serde_json::to_string(&arkret::signatures::JsonWebKey::from_ed25519_verifying_key(
                &self.dpop_signing_key.verifying_key(),
            ))?,
        )?)
    }

    fn accept_issue(
        &self,
        request: arkret::AgentSessionGrantRequestBody,
        outcome: arkret::SessionGrantResponseBody,
    ) -> garth::Result<()> {
        request.validate()?;
        outcome.validate_binding(
            &self.issuer_id,
            &arkret::AccountId::new(request.principal_id.clone(), self.issuer_id.clone()),
            &request.proof.audience_id,
            &self.holder_public_key()?,
            Utc::now(),
        )?;
        if outcome.device_id.is_some()
            || outcome.previous_session_grant_id.is_some()
            || !savfox_gateway_shared::arkret::session_scope_matches_request(
                &request.requested_scope,
                &outcome.granted_scope,
            )
        {
            return Err(garth::Error::Protocol(
                "Agent issuance changed its signed scope or holder kind".into(),
            ));
        }
        let key = outcome.grant_ref();
        let mut issued = self
            .issued
            .lock()
            .map_err(|_| garth::Error::Protocol("Agent issuance source lock poisoned".into()))?;
        if let Some(old) = issued.get(&key)
            && (old.outcome != outcome
                || arkret::canonical::canonical_json_bytes(&old.request)?
                    != arkret::canonical::canonical_json_bytes(&request)?)
        {
            return Err(garth::Error::Protocol(
                "Agent issuance ref has conflicting authenticated outcomes".into(),
            ));
        }
        issued.insert(key, AcceptedAgentIssue { request, outcome });
        Ok(())
    }

    fn accept_refresh_outcome(
        &self,
        prior: &AcceptedAgentIssue,
        outcome: arkret::SessionGrantResponseBody,
    ) -> garth::Result<()> {
        outcome.validate_refresh_binding(&prior.outcome, Utc::now())?;
        let key = outcome.grant_ref();
        let predecessor = prior.outcome.grant_ref();
        let request_bytes = arkret::canonical::canonical_json_bytes(&prior.request)?;
        let mut issued = self
            .issued
            .lock()
            .map_err(|_| garth::Error::Protocol("Agent issuance source lock poisoned".into()))?;
        let Some(held_prior) = issued.get(&predecessor) else {
            return Err(garth::Error::Protocol(
                "Late Agent refresh no longer has its retained predecessor source".into(),
            ));
        };
        if held_prior.outcome != prior.outcome
            || arkret::canonical::canonical_json_bytes(&held_prior.request)? != request_bytes
        {
            return Err(garth::Error::Protocol(
                "Agent refresh predecessor source changed during issuer IO".into(),
            ));
        }
        if issued.iter().any(|(reference, source)| {
            source.outcome.issuer_id == prior.outcome.issuer_id
                && source.outcome.previous_session_grant_id.as_ref()
                    == Some(&prior.outcome.session_grant_id)
                && (reference != &key || source.outcome != outcome)
        }) {
            return Err(garth::Error::Protocol(
                "Agent refresh predecessor has conflicting authenticated successors".into(),
            ));
        }
        if let Some(old) = issued.get(&key)
            && old.outcome != outcome
        {
            return Err(garth::Error::Protocol(
                "Agent refresh ref has conflicting authenticated outcomes".into(),
            ));
        }
        // Keep the exact predecessor for replay, plus its current successor.
        // A late result must pass the retained-predecessor guard before pruning.
        let mut remove = Vec::new();
        for (reference, source) in issued.iter() {
            if reference != &predecessor
                && reference != &key
                && arkret::canonical::canonical_json_bytes(&source.request)? == request_bytes
            {
                remove.push(reference.clone());
            }
        }
        for reference in remove {
            issued.remove(&reference);
        }
        issued.insert(
            key,
            AcceptedAgentIssue {
                request: prior.request.clone(),
                outcome,
            },
        );
        Ok(())
    }

    fn accepted_state(&self, state: &SessionGrantState) -> garth::Result<AcceptedAgentIssue> {
        let source = self
            .issued
            .lock()
            .map_err(|_| garth::Error::Protocol("Agent issuance source lock poisoned".into()))?
            .get(&state.grant_ref)
            .cloned()
            .ok_or_else(|| {
                garth::Error::Protocol(
                    "Agent grant has no authenticated public issuance source".into(),
                )
            })?;
        let outcome = &source.outcome;
        if state.account_id != outcome.account_id
            || state.device_id != outcome.device_id
            || state.session_grant != outcome.session_grant
            || state.expires_at != outcome.expires_at
            || state.audience_id != outcome.audience_id
            || state.granted_scope != outcome.granted_scope
            || state.session_public_key.as_deref() != Some(outcome.session_public_key.as_str())
            || state.dpop_jkt.as_deref()
                != Some(outcome.session_public_key.thumbprint_sha256()?.as_str())
        {
            return Err(garth::Error::Protocol(
                "Agent state differs from its authenticated public issuance".into(),
            ));
        }
        Ok(source)
    }
}

impl SessionGrantTransport for AgentSessionGrantTransport {
    fn issue_session_grant<'a>(
        &'a self,
        request: arkret::SessionGrantRequestBody,
    ) -> BoxSessionFuture<'a, arkret::SessionGrantResponseBody> {
        Box::pin(async move {
            let arkret::SessionGrantRequestBody::Agent(intent) = &request else {
                return Err(garth::Error::Protocol(
                    "Agent transport requires a signed Agent issue intent".into(),
                ));
            };
            let outcome = self.bootstrap.auth_issue_session_grant(&request).await?;
            self.accept_issue(intent.clone(), outcome.clone())?;
            Ok(outcome)
        })
    }

    fn refresh_session_grant<'a>(
        &'a self,
        request: arkret::SessionGrantRefreshRequestBody,
    ) -> BoxSessionFuture<'a, arkret::SessionGrantResponseBody> {
        Box::pin(async move {
            request.validate()?;
            let SessionGrantRefreshRequestBody::Agent(intent) = &request else {
                return Err(garth::Error::Protocol(
                    "Agent transport requires an Agent refresh proof".into(),
                ));
            };
            let session_grant = &intent.session_grant;
            let client = build_dpop_client(
                self.grant_base_url.clone(),
                Arc::clone(&self.dpop_signing_key),
                session_grant.as_str().to_owned(),
            )?;
            let prior = self
                .issued
                .lock()
                .map_err(|_| garth::Error::Protocol("Agent issuance source lock poisoned".into()))?
                .values()
                .find(|source| &source.outcome.session_grant == session_grant)
                .cloned()
                .ok_or_else(|| {
                    garth::Error::Protocol(
                        "Agent refresh has no authenticated predecessor issuance".into(),
                    )
                })?;
            if intent.principal_id != prior.request.principal_id
                || intent.agent_key_authorization_ref != prior.request.agent_key_authorization_ref
                || intent.agent_session_refresh_proof.verification_method
                    != prior.request.proof.verification_method
                || intent
                    .audience_id
                    .as_ref()
                    .is_some_and(|audience| audience != &prior.outcome.audience_id)
            {
                return Err(garth::Error::Protocol(
                    "Agent refresh changed its accepted runtime intent".into(),
                ));
            }
            let outcome = client.auth_refresh_session_grant(&request).await?;
            self.accept_refresh_outcome(&prior, outcome.clone())?;
            Ok(outcome)
        })
    }
}

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct AgentAuthenticatedTransportFactory {
    base_url: Url,
    account_id: arkret::AccountId,
    runtime_fence: Arc<dyn Fn() -> bool + Send + Sync>,
    agent_key_authorization_ref: EventId,
    runtime_signing_key: Arc<SigningKey>,
    dpop_signing_key: Arc<SigningKey>,
    dpop_jkt: String,
    verification_method: DidUrl,
    requested_scope: Vec<String>,
    session_transport: AgentSessionGrantTransport,
    content_realm: Option<RealmId>,
    content_sessions: Option<Arc<AgentContentSessions>>,
}

#[derive(Default)]
struct PendingContentSession {
    request: Option<arkret::SessionGrantRequestBody>,
    engine: Option<SessionEngine<AgentSessionGrantTransport>>,
}

#[derive(Default)]
struct AgentContentSessions {
    realms:
        Mutex<std::collections::BTreeMap<RealmId, Arc<tokio::sync::Mutex<PendingContentSession>>>>,
}

fn mint_agent_session_refresh_proof(
    state: &SessionGrantState,
    agent_key_authorization_ref: &EventId,
    verification_method: &DidUrl,
    signing_key: &SigningKey,
) -> garth::Result<AgentSessionRefreshProof> {
    let request_canonical_digest = arkret::agent_session_refresh_request_digest(
        &state.session_grant,
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
    .map_err(|error| garth::Error::Protocol(error.to_owned()))?;
    unsigned
        .attach_signature(signature)
        .map_err(|error| garth::Error::Protocol(error.to_string()))
}

fn generate_session_dpop_signing_key() -> SigningKey {
    SigningKey::from_bytes(&rand::random::<[u8; 32]>())
}

fn sign_agent_session_request(
    request: AgentSessionGrantSigningInput,
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

impl AgentAuthenticatedTransportFactory {
    /// Validate the retained public grant and the already accepted runtime identity.
    pub fn require_runtime_binding(
        &self,
        state: &SessionGrantState,
        account: &arkret::AccountId,
        verification_method: &str,
        authorization_ref: &str,
    ) -> garth::Result<()> {
        self.build(state)?;
        if &self.account_id != account
            || self.verification_method.as_str() != verification_method
            || self.agent_key_authorization_ref.as_str() != authorization_ref
        {
            return Err(garth::Error::Protocol(
                "Agent session differs from the accepted runtime binding".into(),
            ));
        }
        Ok(())
    }

    fn fresh_content_request(
        &self,
        realm: &RealmId,
    ) -> anyhow::Result<arkret::SessionGrantRequestBody> {
        anyhow::ensure!((self.runtime_fence)(), "Agent runtime was replaced");
        let proof = arkret::dpop::build_dpop_proof(
            &arkret::dpop::DpopProofRequest::new(
                "POST",
                joined_htu(&self.session_transport.grant_base_url, SESSION_GRANT_PATH)?,
            ),
            &self.dpop_signing_key,
        )?;
        anyhow::ensure!(proof.jkt == self.dpop_jkt, "Agent content holder changed");
        let issued_at = Utc::now();
        let unsigned = AgentSessionGrantSigningInput::new(
            self.account_id.principal_id.clone(),
            self.requested_scope.clone(),
            self.agent_key_authorization_ref.clone(),
            arkret::RequestedAgentSessionGrantScope {
                realm_ids: vec![realm.clone()],
                strand_ids: Vec::new(),
                track_names: Vec::new(),
            },
            None,
            SessionGrantDpopBindingProof {
                proof_jwt: proof.header_value,
            },
            arkret::AgentSessionGrantProofInput {
                challenge: arkret::base64url_encode(rand::random::<[u8; 32]>()),
                audience_id: self.account_id.station_id.clone(),
                issued_at,
                expires_at: issued_at + chrono::Duration::minutes(5),
                verification_method: self.verification_method.clone(),
            },
        )?;
        sign_agent_session_request(unsigned, &self.runtime_signing_key)
    }

    async fn content_provider(
        &self,
        realm: &RealmId,
    ) -> anyhow::Result<ArkretAgentSessionProvider> {
        anyhow::ensure!((self.runtime_fence)(), "Agent runtime was replaced");
        let sessions = self
            .content_sessions
            .as_ref()
            .context("Agent content issuer is unavailable")?;
        let entry = {
            let mut realms = sessions
                .realms
                .lock()
                .map_err(|_| anyhow::anyhow!("Agent content session lock poisoned"))?;
            Arc::clone(realms.entry(realm.clone()).or_default())
        };
        // Independent resources never hold a common lock during issuer IO.
        let mut entry = entry.lock().await;
        let factory = Self {
            content_realm: Some(realm.clone()),
            content_sessions: None,
            ..self.clone()
        };
        let refresh = SessionRefreshOptions {
            request: None,
            expected_dpop_jkt: Some(self.dpop_jkt.clone()),
        };
        if let Some(engine) = &entry.engine {
            if engine.current_state().is_some_and(|state| {
                (state.expires_at > Utc::now() || state.pending_refresh.is_some())
                    && state
                        .granted_scope
                        .iter()
                        .any(|action| action == "ak.event.read")
            }) {
                return Ok(SessionTransportProvider::new(
                    engine.clone(),
                    factory,
                    refresh,
                ));
            }
            // A narrowed ceiling or expired grant cannot be refreshed into
            // content access. Leave it immutable and issue under current authority.
            entry.engine = None;
            entry.request = None;
        }
        if entry.request.is_none() {
            entry.request = Some(self.fresh_content_request(realm)?);
        }
        let request = entry
            .request
            .as_ref()
            .expect("content intent installed")
            .clone();
        let engine = SessionEngine::new(self.session_transport.clone());
        if let Err(error) = engine.login_request(request, garth::SystemClock).await {
            // Explicit non-acceptance permits a new issue intent after authority
            // changes. Unknown transport/503 outcomes retain the original proof.
            if matches!(&error, garth::Error::Api { status: 403, .. }) {
                entry.request = None;
            }
            return Err(agent_session_exchange_error(error));
        }
        // A completed, narrowed grant stays immutable. A later authority
        // change requires a new issue intent, never refresh of this response.
        entry.request = None;
        let state = engine
            .current_state()
            .context("Agent content issue returned no grant")?;
        factory.build(&state)?;
        let provider = SessionTransportProvider::new(engine.clone(), factory, refresh);
        entry.engine = Some(engine);
        Ok(provider)
    }
}

impl AuthenticatedTransportFactory for AgentAuthenticatedTransportFactory {
    type Transport = Client;

    fn build(&self, state: &SessionGrantState) -> garth::Result<Self::Transport> {
        let source = self.session_transport.accepted_state(state)?;
        if source.request.agent_key_authorization_ref != self.agent_key_authorization_ref
            || source.request.proof.verification_method != self.verification_method
            || state.dpop_jkt.as_deref() != Some(self.dpop_jkt.as_str())
        {
            return Err(garth::Error::Protocol(
                "Agent observation has another runtime authorization or DPoP key".into(),
            ));
        }
        if !(self.runtime_fence)()
            || state.account_id != self.account_id
            || state.audience_id != self.account_id.station_id
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
        if let Some(realm) = &self.content_realm
            && (source.request.agent_scope_request.realm_ids != [realm.clone()]
                || !source.request.agent_scope_request.strand_ids.is_empty()
                || !source.request.agent_scope_request.track_names.is_empty()
                || !state
                    .granted_scope
                    .iter()
                    .any(|action| action == "ak.event.read"))
        {
            return Err(garth::Error::Protocol(
                "Agent content grant differs from its original exact resource intent".into(),
            ));
        }
        build_dpop_client(
            self.base_url.clone(),
            Arc::clone(&self.dpop_signing_key),
            state.session_grant.as_str().to_owned(),
        )
    }

    fn refresh_options(
        &self,
        state: &SessionGrantState,
        _fallback: &SessionRefreshOptions,
    ) -> garth::Result<SessionRefreshOptions> {
        let source = self.session_transport.accepted_state(state)?;
        if source.request.agent_key_authorization_ref != self.agent_key_authorization_ref
            || source.request.proof.verification_method != self.verification_method
            || state.dpop_jkt.as_deref() != Some(self.dpop_jkt.as_str())
        {
            return Err(garth::Error::Protocol(
                "Agent refresh differs from its original runtime binding".into(),
            ));
        }
        if !(self.runtime_fence)()
            || state.account_id != self.account_id
            || state.audience_id != self.account_id.station_id
        {
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
                AgentSessionGrantRefreshRequestBody {
                    session_grant: state.session_grant.clone(),
                    audience_id: Some(state.audience_id.clone()),
                    principal_id: self.account_id.principal_id.clone(),
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
    effective_scope: arkret::ScopeRef,
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
        target_actor_id: None,
        requester_account_id,
        requester_actor_id: None,
        intended_realm_id,
        effective_scope,
        mls_group_id,
        claim_purpose,
        required_capabilities,
        expires_at,
        target_device_ids,
        target_keypackage_ref: None,
        target_agent_id: None,
        target_agent_verification_method: None,
        target_agent_key_authorize_event_id: None,
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

/// A transient locator read, not a governance projection or authority cache.
#[derive(Default)]
struct JoinedRealmLocatorPages {
    cut: Option<(String, u64)>,
    next: Option<String>,
    finished: bool,
    pages: std::collections::BTreeMap<String, String>,
    seen_realms: std::collections::BTreeSet<RealmId>,
    joined: std::collections::BTreeSet<RealmId>,
}
impl JoinedRealmLocatorPages {
    fn push(
        &mut self,
        after: Option<&str>,
        page: &arkret_models_collaboration::sync_frames::demand_sync::RealmListPage,
    ) -> anyhow::Result<Option<String>> {
        page.validate()?;
        let key = after.unwrap_or_default().to_owned();
        let digest = arkret::canonical::canonical_sha256(page)?;
        if let Some(old) = self.pages.get(&key) {
            anyhow::ensure!(
                old == &digest,
                "the same Realm-list cursor returned different bytes"
            );
            return Ok(self.next.clone());
        }
        anyhow::ensure!(
            !self.finished,
            "a completed Realm-list cut received another page"
        );
        anyhow::ensure!(
            after == self.next.as_deref(),
            "Realm-list continuation is out of order"
        );
        let cut = (page.snapshot_cursor.clone(), page.snapshot_revision);
        if let Some(expected) = &self.cut {
            anyhow::ensure!(
                expected == &cut,
                "Realm-list page belongs to another frozen cut"
            );
        }
        if let Some(next) = &page.next_cursor {
            anyhow::ensure!(
                next != &key && !self.pages.contains_key(next),
                "Realm-list continuation loops to an old cursor"
            );
        }
        anyhow::ensure!(
            page.items
                .iter()
                .all(|item| !self.seen_realms.contains(&item.realm_id)),
            "frozen Realm-list pages repeat a Realm"
        );
        if self.cut.is_none() {
            self.cut = Some(cut);
        }
        for item in &page.items {
            self.seen_realms.insert(item.realm_id.clone());
            if item.membership
                == arkret_models_collaboration::sync_frames::demand_sync::RealmListMembership::Join
            {
                self.joined.insert(item.realm_id.clone());
            }
        }
        self.next = page.next_cursor.clone();
        self.finished = self.next.is_none();
        self.pages.insert(key, digest);
        Ok(self.next.clone())
    }
}

impl ArkretHttpClient {
    #[must_use]
    pub fn inner(&self) -> &Client {
        &self.inner
    }

    #[must_use]
    pub fn from_inner(inner: Client) -> Self {
        Self {
            inner,
            own_station: None,
            session_provider: None,
            station_binding: None,
        }
    }

    pub fn own_station(
        &self,
    ) -> anyhow::Result<&arkret::http_client::own_station_results::OwnStationStateClient> {
        self.own_station
            .as_ref()
            .context("Agent client has no accepted own-Station session")
    }

    /// The public opaque issuance API does not provide the issuer-owned selection overlay.
    /// Retain the explicit missing state; it must never be replaced by Public mode or claims
    /// parsing.
    pub fn participation_observation(
        &self,
    ) -> anyhow::Result<
        arkret_models_collaboration::governance::agent_participation::AgentParticipationObservation,
    > {
        self.own_station()?.check_session()?;
        let provider = self
            .session_provider
            .as_ref()
            .context("Agent provider is unavailable")?;
        let state = provider
            .session()
            .current_state()
            .context("Agent scoped session is unavailable")?;
        provider.factory().build(&state)?;
        anyhow::bail!(
            "Agent participation selection overlay is unavailable from public opaque issuance"
        )
    }

    pub async fn from_provider(
        provider: &ArkretAgentSessionProvider,
        store: &super::crypto_state::FileArkretCryptoStore,
    ) -> garth::Result<Self> {
        let binding = store
            .accepted_station_binding()
            .map_err(|error| garth::Error::Protocol(error.to_string()))?;
        let inner = garth::TransportProvider::provide(provider).await?;
        let own_station = provider.own_station_result_client(inner.clone(), binding.clone())?;
        Ok(Self {
            inner,
            own_station: Some(own_station),
            session_provider: Some(provider.clone()),
            station_binding: Some(binding),
        })
    }

    /// Issue an independent single-resource grant; never expand the service chain.
    pub async fn for_realm(&self, realm: &RealmId) -> anyhow::Result<Self> {
        let provider = self
            .session_provider
            .as_ref()
            .context("Agent content provider is unavailable")?;
        let factory = provider.factory();
        let provider = if factory.content_realm.as_ref() == Some(realm) {
            provider.clone()
        } else {
            factory.content_provider(realm).await?
        };
        let inner = garth::TransportProvider::provide(&provider).await?;
        let binding = self
            .station_binding
            .clone()
            .context("Agent Station binding is unavailable")?;
        let own_station = provider.own_station_result_client(inner.clone(), binding.clone())?;
        Ok(Self {
            inner,
            own_station: Some(own_station),
            session_provider: Some(provider),
            station_binding: Some(binding),
        })
    }

    pub async fn current(&self) -> anyhow::Result<Self> {
        let Some(provider) = &self.session_provider else {
            return Ok(self.clone());
        };
        let inner = garth::TransportProvider::provide(provider).await?;
        let binding = self
            .station_binding
            .clone()
            .context("Agent Station binding is unavailable")?;
        let own_station = provider.own_station_result_client(inner.clone(), binding.clone())?;
        Ok(Self {
            inner,
            own_station: Some(own_station),
            session_provider: Some(provider.clone()),
            station_binding: Some(binding),
        })
    }

    /// Previously requested targets are only locators, never an authority cache.
    pub fn cached_content_realms(&self) -> anyhow::Result<Vec<RealmId>> {
        let provider = self
            .session_provider
            .as_ref()
            .context("Agent provider is unavailable")?;
        let Some(sessions) = &provider.factory().content_sessions else {
            return Ok(Vec::new());
        };
        let realms = sessions
            .realms
            .lock()
            .map_err(|_| anyhow::anyhow!("Agent content session lock poisoned"))?;
        Ok(realms.keys().cloned().collect())
    }

    /// Read only already-disclosed membership coordinates. Every candidate
    /// still needs its own fresh issue and the service's current content gates.
    async fn joined_realm_locators(&self) -> anyhow::Result<Vec<RealmId>> {
        let own = self.own_station()?;
        let session = own.session()?.clone();
        let mut pages = JoinedRealmLocatorPages::default();
        let mut account_after = None;
        loop {
            own.check_session()?;
            anyhow::ensure!(
                own.session()? == &session,
                "Agent session changed during Realm-list paging"
            );
            let request=AccountSubscribeParameters {
                after:account_after.clone(),catchup:Some(false),
                filter:Some(arkret_models_collaboration::sync_frames::account_subscribe::AccountFilter {realm_ids:Some(Vec::new()),..Default::default()}),
                realm_list_parameters:Some(arkret_models_collaboration::sync_frames::account_subscribe::RealmListParameters {
                    after:pages.next.as_deref().map(arkret_wire::Cursor::new).transpose()?,limit:Some(100),
                }),replace_filter:None,
            };
            let batch = self.inner.account_subscribe_batch(&request).await?;
            own.check_session()?;
            anyhow::ensure!(
                own.session()? == &session,
                "Agent session changed during Realm-list delivery"
            );
            let mut found = false;
            let requested_after = pages.next.clone();
            for frame in &batch.frames {
                if let Some(page) = &frame.realm_list {
                    let _ = pages.push(requested_after.as_deref(), page)?;
                    found = true;
                }
            }
            anyhow::ensure!(found, "Realm-list operation omitted its requested page");
            account_after = Some(batch.cursor);
            if pages.finished {
                return Ok(pages.joined.into_iter().collect());
            }
        }
    }

    /// Discover joined membership locators and the exact owner Direct pair.
    /// Each target still requires its own bound resource session; these locators
    /// do not supply a participation selection overlay or grant authority.
    pub async fn content_realm_candidates(
        &self,
        controller_account: &arkret::AccountId,
    ) -> anyhow::Result<Vec<RealmId>> {
        let provider = self
            .session_provider
            .as_ref()
            .context("Agent provider is unavailable")?;
        provider.refresh_for_sensitive_action().await?;
        let current = self.current().await?;
        let state = provider
            .session()
            .current_state()
            .context("Agent discovery session is unavailable")?;
        provider.factory().build(&state)?;
        anyhow::ensure!(
            controller_account.station_id == state.account_id.station_id,
            "Direct owner discovery names another Account Station"
        );
        let request = arkret::DirectConversationResolveRequestBody {
            peer: arkret::ContactPeer::Human {
                account_id: controller_account.clone(),
            },
        };
        let mut realms = current
            .joined_realm_locators()
            .await?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if let Ok(outcome) = current.inner.direct_conversation_resolve(&request).await
            && let Some(coordinates) = outcome.coordinates()
        {
            realms.insert(coordinates.realm_id.clone());
        }
        current.own_station()?.check_session()?;
        Ok(realms.into_iter().collect())
    }

    /// Construct an Agent HTTP client by exchanging a runtime-key
    /// `agent_key_proof` for a short-lived DPoP-bound session grant.
    #[allow(clippy::too_many_arguments)]
    pub async fn login_agent_provider(
        base_url: &str,
        key_ref: &ArkretKeyRef,
        account_id: arkret::AccountId,
        accepted_binding: &arkret::StationConnectionBinding,
        runtime_fence: Arc<dyn Fn() -> bool + Send + Sync>,
        verification_method: &str,
        agent_key_authorization_ref: &str,
        requested_scope: Vec<String>,
        audience: &str,
        realm_id: Option<&str>,
    ) -> anyhow::Result<(ArkretAgentSessionProvider, ArkretSession)> {
        let (provider, session, _) = Self::login_agent_provider_with_native_credentials(
            base_url,
            key_ref,
            account_id,
            accepted_binding,
            runtime_fence,
            verification_method,
            agent_key_authorization_ref,
            requested_scope,
            audience,
            realm_id,
        )
        .await?;
        Ok((provider, session))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn login_agent_provider_with_native_credentials(
        base_url: &str,
        key_ref: &ArkretKeyRef,
        account_id: arkret::AccountId,
        accepted_binding: &arkret::StationConnectionBinding,
        runtime_fence: Arc<dyn Fn() -> bool + Send + Sync>,
        verification_method: &str,
        agent_key_authorization_ref: &str,
        requested_scope: Vec<String>,
        audience: &str,
        realm_id: Option<&str>,
    ) -> anyhow::Result<(
        ArkretAgentSessionProvider,
        ArkretSession,
        ArkretNativeCredentialSource,
    )> {
        savfox_gateway_shared::arkret::validate_agent_runtime_scope(&requested_scope)
            .map_err(anyhow::Error::msg)?;
        let principal_did = account_id.principal_id.clone();
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
        // Keep the short-lived opaque grant and its original public issuance
        // context in provider memory; the long-lived runtime key stays in
        // platform-protected key custody.
        let grant_store = NoopSessionGrantStore;
        let resource_url =
            Url::parse(base_url).with_context(|| format!("invalid Arkret base_url: {base_url}"))?;
        anyhow::ensure!(
            resource_url.as_str() == accepted_binding.base_url
                && account_id.station_id == accepted_binding.service_id
                && audience == accepted_binding.service_id,
            "Agent configured Account, audience and accepted Station differ"
        );
        let authority = accepted_binding
            .authentication_metadata
            .account_authority
            .as_ref()
            .context("accepted Station omitted Account Authority")?;
        let grant_base_url = Url::parse(authority.origin.as_str())?;
        let discovered_grant_base_url =
            discover_account_authority_base_url(&resource_url, &audience).await?;
        anyhow::ensure!(
            discovered_grant_base_url == grant_base_url,
            "current Account Authority differs from accepted Station binding"
        );
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
        let agent_scope_request = arkret::RequestedAgentSessionGrantScope {
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
        let unsigned_request = AgentSessionGrantSigningInput::new(
            principal_did.clone(),
            requested_scope,
            agent_key_authorization_ref.clone(),
            agent_scope_request,
            None,
            dpop_binding_proof,
            arkret::AgentSessionGrantProofInput {
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
            issuer_id: accepted_binding.service_id.clone(),
            issued: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        };
        let factory = AgentAuthenticatedTransportFactory {
            base_url: resource_url,
            account_id: account_id.clone(),
            runtime_fence,
            agent_key_authorization_ref,
            runtime_signing_key,
            dpop_signing_key,
            dpop_jkt: dpop_jkt.clone(),
            verification_method: verification_method.clone(),
            requested_scope: expected_scope,
            session_transport: session_transport.clone(),
            content_realm: realm_id
                .map(|value| RealmId::new(value.to_owned()))
                .transpose()?,
            content_sessions: Some(Arc::new(AgentContentSessions::default())),
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
            if state.account_id == account_id
                && state.audience_id == audience
                && state.expires_at > Utc::now()
            {
                factory
                    .build(&state)
                    .map_err(|error| anyhow::anyhow!("restored Agent grant scope: {error}"))?;
                let session = arkret_session_from_state(&state);
                return Ok((
                    restored.clone(),
                    session,
                    ArkretNativeCredentialSource {
                        provider: restored,
                        factory,
                    },
                ));
            }
            grant_store
                .clear()
                .map_err(|error| anyhow::anyhow!("clear stale agent session grant: {error}"))?;
        }

        let session_engine = SessionEngine::new(session_transport);
        session_engine
            .login_request(login_request, garth::SystemClock)
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
            factory.clone(),
            refresh_options,
            grant_store,
        )
        .await
        .map_err(|error| anyhow::anyhow!("persist agent session grant: {error}"))?;
        Ok((
            provider.clone(),
            arkret_session_from_state(&state),
            ArkretNativeCredentialSource { provider, factory },
        ))
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
        let request = AccountSubscribeParameters {
            after: after.map(str::to_owned),
            catchup: None,
            filter: None,
            realm_list_parameters: None,
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
        context: &arkret::ProducerVerificationContext,
    ) -> anyhow::Result<arkret::VerifiedSubmission<EventAdmissionSubmission>> {
        let verified = arkret::VerifiedProducerEvent::verify(event.event().clone(), context)?;
        let submission = EventAdmissionSubmission::new(event.event().clone());
        Ok(arkret::VerifiedSubmission::from_verified_events(
            submission,
            &[verified],
        )?)
    }

    /// Submit over the authenticated self surface and require exact Commit
    /// coverage of this Event and its security-scope stream.
    pub async fn submit_submission(
        &self,
        submission: &arkret::VerifiedSubmission<EventAdmissionSubmission>,
    ) -> anyhow::Result<AuthoritySubmitResponseBody> {
        submission.validate()?;
        let content = self.for_realm(&submission.event.realm_id).await?;
        let outcome = content
            .inner
            .submit_event(submission)
            .await
            .map_err(anyhow::Error::from)?;
        outcome.validate_for_request(&AuthoritySubmitRequestBody::Event(
            submission.wire().clone(),
        ))?;
        Ok(outcome)
    }

    pub async fn submit_event(
        &self,
        event: &AuthoredEvent,
        context: &arkret::ProducerVerificationContext,
    ) -> anyhow::Result<AuthoritySubmitResponseBody> {
        let submission = self.prepare_submission(event, context)?;
        self.submit_submission(&submission).await
    }

    pub async fn keypackages_claim(
        &self,
        request: &KeyPackagesClaimRequestBody,
    ) -> anyhow::Result<KeyPackagesClaimResponseBody> {
        self.inner
            .keypackages_claim(request)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }
}

fn resource_session_error(error: anyhow::Error) -> garth::Error {
    match error.downcast::<AgentSessionExchangeError>() {
        Ok(error) => error.source,
        Err(error) => match error.downcast::<garth::Error>() {
            Ok(error) => error,
            Err(error) => garth::Error::Protocol(error.to_string()),
        },
    }
}

impl garth::AuthorityTransport for ArkretHttpClient {
    async fn submit(
        &self,
        request: &arkret::VerifiedSubmission<AuthoritySubmitRequestBody>,
        options: &arkret::http_client::ClientRequestOptions,
    ) -> garth::Result<AuthoritySubmitResponseBody> {
        let realm = match request.wire() {
            AuthoritySubmitRequestBody::Event(value) => &value.event.realm_id,
            AuthoritySubmitRequestBody::MlsCommit(value) => &value.commit_event.realm_id,
        };
        let scoped = self
            .for_realm(realm)
            .await
            .map_err(resource_session_error)?;
        let own = scoped.own_station().map_err(resource_session_error)?;
        garth::AuthorityTransport::submit(own, request, options).await
    }

    async fn submit_self(
        &self,
        request: &arkret::VerifiedSubmission<
            arkret_models_collaboration::authority_commit::SelfAuthoritySubmitRequestBody,
        >,
        options: &arkret::http_client::ClientRequestOptions,
    ) -> garth::Result<arkret_models_collaboration::authority_commit::SelfAuthoritySubmitResponseBody>
    {
        let realm = match request.wire() {
            arkret_models_collaboration::authority_commit::SelfAuthoritySubmitRequestBody::Event(value) => Some(&value.event.realm_id),
            arkret_models_collaboration::authority_commit::SelfAuthoritySubmitRequestBody::MlsCommit(value) => {
                Some(&value.commit_event.realm_id)
            }
            _ => None,
        };
        if let Some(realm) = realm {
            let scoped = self
                .for_realm(realm)
                .await
                .map_err(resource_session_error)?;
            return garth::AuthorityTransport::submit_self(
                scoped.own_station().map_err(resource_session_error)?,
                request,
                options,
            )
            .await;
        }
        // Pre-resource atomic bootstrap retains its existing admission contract.
        garth::AuthorityTransport::submit_self(
            self.own_station().map_err(resource_session_error)?,
            request,
            options,
        )
        .await
    }

    async fn scan(
        &self,
        request: &arkret::StreamScanRequestBody,
    ) -> garth::Result<arkret::StreamScanResponseBody> {
        let scoped = self
            .for_realm(&request.realm_id)
            .await
            .map_err(resource_session_error)?;
        garth::AuthorityTransport::scan(
            scoped.own_station().map_err(resource_session_error)?,
            request,
        )
        .await
    }

    async fn authority_bundle(
        &self,
        _request: &arkret::RealmAuthorityBundleRequestBody,
    ) -> garth::Result<arkret::RealmAuthorityBundle> {
        Err(garth::Error::Protocol(
            "ordinary own-Station consumption does not perform governance discovery".into(),
        ))
    }

    async fn install_handoff(
        &self,
        _request: &arkret::AuthorityRealmAuthorityHandoffRequestBody,
        _options: &arkret::http_client::ClientRequestOptions,
    ) -> garth::Result<arkret::RealmAuthorityHandoff> {
        Err(garth::Error::Protocol(
            "ordinary own-Station consumption cannot install Station handoffs".into(),
        ))
    }

    async fn exact_snapshot(
        &self,
        realm: &RealmId,
        snapshot: &arkret::RealmSnapshotId,
    ) -> garth::Result<arkret::RealmStateSnapshot> {
        let scoped = self
            .for_realm(realm)
            .await
            .map_err(resource_session_error)?;
        garth::AuthorityTransport::exact_snapshot(
            scoped.own_station().map_err(resource_session_error)?,
            realm,
            snapshot,
        )
        .await
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
#[must_use]
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
#[must_use]
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

fn arkret_session_from_state(state: &SessionGrantState) -> ArkretSession {
    ArkretSession {
        grant_ref: state.grant_ref.clone(),
        session_grant: state.session_grant.clone(),
        expires_at: state.expires_at,
        principal_did: state.account_id.principal_id.clone(),
        device_id: state.device_id.clone(),
    }
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

/// Process-only original session proof, never parsed authority or a wire DTO.
pub struct ArkretNativeAgentCredentials {
    pub method: String,
    pub canonical_url: String,
    pub headers: reqwest::header::HeaderMap,
    pub operation: arkret::ServiceOperationId,
}

impl std::fmt::Debug for ArkretNativeAgentCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArkretNativeAgentCredentials")
            .finish_non_exhaustive()
    }
}

/// Holds the SAME ephemeral session signer and live provider as normal receive.
/// No Clone/serde exposure; Debug omits the signer and credentials.
pub struct ArkretNativeCredentialSource {
    provider: ArkretAgentSessionProvider,
    factory: AgentAuthenticatedTransportFactory,
}

impl std::fmt::Debug for ArkretNativeCredentialSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArkretNativeCredentialSource")
            .finish_non_exhaustive()
    }
}
impl ArkretNativeCredentialSource {
    pub async fn for_realm(&self, realm: &RealmId) -> anyhow::Result<Self> {
        let provider = if self.factory.content_realm.as_ref() == Some(realm) {
            self.provider.clone()
        } else {
            self.factory.content_provider(realm).await?
        };
        let factory = provider.factory().clone();
        Ok(Self { provider, factory })
    }

    pub async fn source_request(
        &self,
        event_id: &EventId,
    ) -> anyhow::Result<ArkretNativeAgentCredentials> {
        use arkret::signatures::http_signature::{
            HttpSignatureScenario, SignedRequestParts, sign_http_message_for_scenario,
        };
        use reqwest::header::{HeaderMap, HeaderValue};
        // Refresh/rebuild normal grant before host acquires any local current lock.
        garth::TransportProvider::provide(&self.provider).await?;
        let state = self
            .provider
            .session()
            .current_state()
            .context("native Agent session absent")?;
        self.factory.build(&state)?;
        let method = "GET";
        let path = format!("/_arkret/self/committed-events/{event_id}");
        let canonical_url = joined_htu(&self.factory.base_url, &path)?;
        let operation = arkret::ServiceOperationId::SelfCommittedEventResourceGetV1;
        anyhow::ensure!(
            operation.matches_http_request(method, &path),
            "native source operation mismatch"
        );
        let dpop = arkret::dpop::build_dpop_proof(
            &arkret::dpop::DpopProofRequest::new(method, canonical_url.clone())
                .access_token(state.session_grant.as_str().to_owned()),
            self.factory.dpop_signing_key.as_ref(),
        )?;
        anyhow::ensure!(
            dpop.jkt == self.factory.dpop_jkt,
            "native session holder changed"
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("DPoP {}", state.session_grant.as_str()))?,
        );
        headers.insert("dpop", HeaderValue::from_str(&dpop.header_value)?);
        headers.insert(
            "arkret-operation",
            HeaderValue::from_static(
                arkret::ServiceOperationId::SELF_COMMITTED_EVENT_RESOURCE_GET_V1,
            ),
        );
        let url = Url::parse(&canonical_url)?;
        let authority = url[url::Position::BeforeHost..url::Position::AfterPort].to_owned();
        let parts = SignedRequestParts {
            method: method.into(),
            target_uri: canonical_url.clone(),
            authority,
            path,
            headers: headers
                .iter()
                .map(|(k, v)| Ok((k.as_str().to_owned(), v.to_str()?.to_owned())))
                .collect::<anyhow::Result<Vec<_>>>()?,
            body_digest: None,
        };
        let signed = sign_http_message_for_scenario(
            &parts,
            HttpSignatureScenario::ClientSessionPopV1,
            &[],
            "sig1",
            &self.factory.dpop_jkt,
            Utc::now().timestamp(),
            self.factory.dpop_signing_key.as_ref(),
        )?;
        headers.insert(
            "signature-input",
            HeaderValue::from_str(&signed.signature_input_header)?,
        );
        headers.insert(
            "signature",
            HeaderValue::from_str(&signed.signature_header)?,
        );
        Ok(ArkretNativeAgentCredentials {
            method: method.into(),
            canonical_url,
            headers,
            operation,
        })
    }
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

fn verify_agent_service_audience(expected: &DidCoreId, observed: &DidCoreId) -> anyhow::Result<()> {
    if expected != observed {
        return Err(AgentSessionExchangeError {
            reason: "station_identity_changed".to_owned(),
            action: "this saved pairing belongs to another Station identity; configure a new pairing for the current Station",
            source: garth::Error::Protocol(format!(
                "configured Station {expected}, discovered Station {observed}"
            )),
        }.into());
    }
    Ok(())
}

async fn discover_account_authority_base_url(
    resource_url: &Url,
    audience: &DidCoreId,
) -> anyhow::Result<Url> {
    let discovery = agent_client_builder(resource_url.clone())?
        .build()
        .map_err(|error| anyhow::anyhow!("Arkret service discovery client: {error}"))?;
    let description = discovery
        .describe()
        .await
        .map_err(|error| anyhow::anyhow!("Arkret service discovery: {error}"))?;
    // A database reset can reincarnate the same URL as a different Station.
    // Never send the old Agent's proof to that Station's Account Authority.
    verify_agent_service_audience(audience, &description.service_id)?;
    let authority = description
        .authentication_metadata
        .account_authority
        .context("Arkret service description omitted authentication_metadata.account_authority")?;
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
#[allow(clippy::unwrap_used)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use ed25519_dalek::Signature;
    use serde_json::Value;

    use super::*;

    #[test]
    fn reset_station_cannot_reuse_a_saved_agent_pairing_or_be_reported_as_bad_signature() {
        let prior = DidCoreId::new("ak:did_core:web:prior.example").unwrap();
        let current = DidCoreId::new("ak:did_core:web:current.example").unwrap();
        let error = verify_agent_service_audience(&prior, &current).unwrap_err();
        assert_eq!(
            agent_session_exchange_reason(&error),
            Some("station_identity_changed")
        );
        assert!(!agent_session_reason_is_irreversibly_terminal(
            "station_identity_changed"
        ));
        assert!(verify_agent_service_audience(&current, &current).is_ok());
    }

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
        static STATE: std::sync::OnceLock<SessionGrantState> = std::sync::OnceLock::new();
        STATE
            .get_or_init(|| {
                let public = arkret::base64url_encode(
                    SigningKey::from_bytes(&[44; 32]).verifying_key().to_bytes(),
                );
                let jwk = arkret_models_identity_fixture_jwk(public);
                let mut granted_scope =
                    savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap();
                granted_scope.sort();
                let issuer = DidCoreId::new("ak:did_core:web:station.example").unwrap();
                SessionGrantState {
                    account_id: arkret::AccountId::new(
                        DidCoreId::new("ak:did_core:web:agent.example").unwrap(),
                        issuer.clone(),
                    ),
                    device_id: None,
                    grant_ref: arkret::SessionGrantRef::new(
                        issuer.clone(),
                        arkret::SessionGrantId::new(
                            "ak:session_grant:01904100-0000-7000-8000-000000000001",
                        )
                        .unwrap(),
                    ),
                    session_grant: arkret::SessionGrantSecret::from_bytes([17; 32]),
                    expires_at: arkret::canonical::normalize_timestamp_canonical(
                        Utc::now() + chrono::Duration::minutes(5),
                    ),
                    audience_id: issuer,
                    granted_scope,
                    session_public_key: Some(jwk.clone().into_string()),
                    dpop_jkt: Some(jwk.thumbprint_sha256().unwrap()),
                    pending_refresh: None,
                }
            })
            .clone()
    }

    // Authenticated-transport unit fixture only: this exercises the real signed
    // public request and response binding, not a native issuer or private overlay.
    fn record_test_issue(
        transport: &AgentSessionGrantTransport,
        state: &SessionGrantState,
        realms: Vec<RealmId>,
        strands: Vec<StrandId>,
    ) {
        let proof = arkret::dpop::build_dpop_proof(
            &arkret::dpop::DpopProofRequest::new(
                "POST",
                joined_htu(&transport.grant_base_url, SESSION_GRANT_PATH).unwrap(),
            ),
            &transport.dpop_signing_key,
        )
        .unwrap();
        let at = Utc::now();
        let input = AgentSessionGrantSigningInput::new(
            state.account_id.principal_id.clone(),
            savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap(),
            authorization_event(),
            arkret::RequestedAgentSessionGrantScope {
                realm_ids: realms,
                strand_ids: strands,
                track_names: Vec::new(),
            },
            None,
            SessionGrantDpopBindingProof {
                proof_jwt: proof.header_value,
            },
            arkret::AgentSessionGrantProofInput {
                challenge: arkret::base64url_encode([37; 32]),
                audience_id: state.audience_id.clone(),
                issued_at: at,
                expires_at: at + chrono::Duration::minutes(5),
                verification_method: DidUrl::new("did:web:agent.example#runtime-1").unwrap(),
            },
        )
        .unwrap();
        let arkret::SessionGrantRequestBody::Agent(request) =
            sign_agent_session_request(input, &signing_key()).unwrap()
        else {
            unreachable!()
        };
        let outcome = arkret::SessionGrantResponseBody {
            account_id: state.account_id.clone(),
            issuer_id: state.grant_ref.issuer_id.clone(),
            device_id: None,
            session_grant: state.session_grant.clone(),
            expires_at: state.expires_at,
            session_grant_id: state.grant_ref.id.clone(),
            session_public_key: arkret::CanonicalSessionPublicJwk::new(
                state.session_public_key.clone().unwrap(),
            )
            .unwrap(),
            audience_id: state.audience_id.clone(),
            granted_scope: state.granted_scope.clone(),
            previous_session_grant_id: None,
        };
        transport.accept_issue(request, outcome).unwrap();
    }

    fn arkret_models_identity_fixture_jwk(public: String) -> arkret::CanonicalSessionPublicJwk {
        arkret::CanonicalSessionPublicJwk::new(
            serde_json::json!({"kty":"OKP","crv":"Ed25519","x":public}).to_string(),
        )
        .unwrap()
    }

    fn test_session_transport() -> AgentSessionGrantTransport {
        let grant_base_url = Url::parse("https://arkret.example.org").unwrap();
        let transport = AgentSessionGrantTransport {
            bootstrap: agent_client_builder(grant_base_url.clone())
                .unwrap()
                .build()
                .unwrap(),
            grant_base_url,
            dpop_signing_key: Arc::new(SigningKey::from_bytes(&[44; 32])),
            issuer_id: agent_session_state().grant_ref.issuer_id.clone(),
            issued: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        };
        record_test_issue(&transport, &agent_session_state(), Vec::new(), Vec::new());
        transport
    }

    fn content_test_factory() -> AgentAuthenticatedTransportFactory {
        AgentAuthenticatedTransportFactory {
            base_url: Url::parse("https://arkret.example.org").unwrap(),
            account_id: agent_session_state().account_id.clone(),
            runtime_fence: Arc::new(|| true),
            agent_key_authorization_ref: authorization_event(),
            runtime_signing_key: Arc::new(signing_key()),
            dpop_signing_key: Arc::new(SigningKey::from_bytes(&[44; 32])),
            dpop_jkt: agent_session_state().dpop_jkt.clone().unwrap(),
            verification_method: DidUrl::new("did:web:agent.example#runtime-1").unwrap(),
            requested_scope: agent_session_state().granted_scope.clone(),
            session_transport: test_session_transport(),
            content_realm: None,
            content_sessions: None,
        }
    }

    #[test]
    fn agent_scope_transport_factory_checks_every_initial_or_refreshed_grant() {
        let mut requested_scope =
            savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap();
        requested_scope.sort();
        let factory = AgentAuthenticatedTransportFactory {
            base_url: Url::parse("https://arkret.example.org").unwrap(),
            account_id: agent_session_state().account_id.clone(),
            runtime_fence: Arc::new(|| true),
            agent_key_authorization_ref: authorization_event(),
            runtime_signing_key: Arc::new(signing_key()),
            dpop_signing_key: Arc::new(SigningKey::from_bytes(&[44; 32])),
            dpop_jkt: agent_session_state().dpop_jkt.clone().unwrap(),
            verification_method: DidUrl::new("did:web:agent.example#runtime-1").unwrap(),
            requested_scope: requested_scope.clone(),
            session_transport: test_session_transport(),
            content_realm: None,
            content_sessions: None,
        };
        let mut state = agent_session_state();
        state.granted_scope = requested_scope.clone();
        factory
            .build(&state)
            .expect("exact current grant builds a transport");
        let mut other_station = state.clone();
        other_station.account_id.station_id =
            DidCoreId::new("ak:did_core:webvh:z6mkotherstation").unwrap();
        assert!(factory.build(&other_station).is_err());
        assert!(
            factory
                .refresh_options(&other_station, &SessionRefreshOptions::default())
                .is_err()
        );
        let mut other_audience = state.clone();
        other_audience.audience_id = DidCoreId::new("ak:did_core:webvh:z6mkotherstation").unwrap();
        assert!(factory.build(&other_audience).is_err());
        assert!(
            factory
                .refresh_options(&other_audience, &SessionRefreshOptions::default())
                .is_err()
        );
        let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let guarded = AgentAuthenticatedTransportFactory {
            runtime_fence: {
                let active = Arc::clone(&active);
                Arc::new(move || active.load(std::sync::atomic::Ordering::SeqCst))
            },
            ..factory.clone()
        };
        assert!(guarded.build(&state).is_ok());
        active.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(guarded.build(&state).is_err());
        assert!(
            guarded
                .refresh_options(&state, &SessionRefreshOptions::default())
                .is_err()
        );
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

    #[tokio::test]
    async fn opaque_public_issue_binds_the_runtime_without_fabricating_a_selection_overlay() {
        let state = agent_session_state();
        let factory = content_test_factory();
        factory.build(&state).unwrap();
        let issued = factory.session_transport.accepted_state(&state).unwrap();
        let signature = Signature::from_slice(
            &arkret::base64url_decode(&issued.request.proof.signature).unwrap(),
        )
        .unwrap();
        signing_key()
            .verifying_key()
            .verify_strict(
                &issued.request.proof.canonical_signing_bytes().unwrap(),
                &signature,
            )
            .unwrap();
        let provider = SessionTransportProvider::new(
            SessionEngine::with_state(factory.session_transport.clone(), state.clone()),
            factory,
            SessionRefreshOptions::default(),
        );
        let directory = tempfile::tempdir().unwrap();
        let store = super::super::crypto_state::FileArkretCryptoStore::new(
            directory.path(),
            "opaque-public-issue".into(),
        );
        store
            .accept_station_binding(&arkret::StationConnectionBinding {
                service_id: state.account_id.station_id.clone(),
                base_url: "https://arkret.example.org/".into(),
                trust_domain: arkret::TrustDomainId::new("ak:trust_domain:example.net").unwrap(),
                authentication_metadata: arkret::AuthenticationMetadata::minimal(),
            })
            .unwrap();
        let client = ArkretHttpClient::from_provider(&provider, &store)
            .await
            .unwrap();
        assert!(
            client
                .participation_observation()
                .unwrap_err()
                .to_string()
                .contains("selection overlay is unavailable")
        );
        let public_session = arkret_session_from_state(&state);
        assert_eq!(public_session.grant_ref, state.grant_ref);
        assert_eq!(public_session.session_grant, state.session_grant);
        let mut changed = state.clone();
        changed.session_grant = arkret::SessionGrantSecret::from_bytes([18; 32]);
        assert!(provider.factory().build(&changed).is_err());
        changed = state;
        changed.grant_ref.issuer_id = DidCoreId::new("ak:did_core:web:foreign.example").unwrap();
        assert!(provider.factory().build(&changed).is_err());
        let replaced_key = AgentAuthenticatedTransportFactory {
            agent_key_authorization_ref: EventId::from_digest(
                arkret::canonical::DigestSuite::Sha256,
                [20; 32],
            ),
            ..provider.factory().clone()
        };
        assert!(replaced_key.build(&agent_session_state()).is_err());
        let other_method = AgentAuthenticatedTransportFactory {
            verification_method: DidUrl::new("did:web:agent.example#runtime-2").unwrap(),
            ..provider.factory().clone()
        };
        assert!(other_method.build(&agent_session_state()).is_err());
    }

    #[tokio::test]
    async fn late_transport_refresh_cannot_remove_the_current_public_issuance_source() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        async fn receive(listener: &tokio::net::TcpListener) -> tokio::net::TcpStream {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0 && bytes.len() < 65_536);
                bytes.extend_from_slice(&chunk[..read]);
                if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
            assert!(
                headers.starts_with("POST /_arkret/gate/account/session-grants/refresh HTTP/1.1")
            );
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            while bytes.len() < header_end + length {
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0 && bytes.len() < 65_536);
                bytes.extend_from_slice(&chunk[..read]);
            }
            let request: SessionGrantRefreshRequestBody =
                serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
            request.validate().unwrap();
            socket
        }
        async fn reply(
            mut socket: tokio::net::TcpStream,
            outcome: &arkret::SessionGrantResponseBody,
        ) {
            let bytes = serde_json::to_vec(outcome).unwrap();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).as_bytes()).await.unwrap();
            socket.write_all(&bytes).await.unwrap();
            socket.shutdown().await.unwrap();
        }
        fn state_from(outcome: &arkret::SessionGrantResponseBody) -> SessionGrantState {
            SessionGrantState {
                account_id: outcome.account_id.clone(),
                device_id: outcome.device_id.clone(),
                grant_ref: outcome.grant_ref(),
                session_grant: outcome.session_grant.clone(),
                expires_at: outcome.expires_at,
                audience_id: outcome.audience_id.clone(),
                granted_scope: outcome.granted_scope.clone(),
                session_public_key: Some(outcome.session_public_key.clone().into_string()),
                dpop_jkt: Some(outcome.session_public_key.thumbprint_sha256().unwrap()),
                pending_refresh: None,
            }
        }
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let mut factory = content_test_factory();
            factory.session_transport.grant_base_url = base;
            let transport = factory.session_transport.clone();
            let g0 = agent_session_state();
            let original = transport.accepted_state(&g0).unwrap().outcome;
            let mut g1 = original.clone();
            g1.session_grant_id = arkret::SessionGrantId::new(
                "ak:session_grant:01904100-0000-7000-8000-000000000002",
            )
            .unwrap();
            g1.session_grant = arkret::SessionGrantSecret::from_bytes([18; 32]);
            g1.previous_session_grant_id = Some(original.session_grant_id.clone());
            let mut g2 = g1.clone();
            g2.session_grant_id = arkret::SessionGrantId::new(
                "ak:session_grant:01904100-0000-7000-8000-000000000003",
            )
            .unwrap();
            g2.session_grant = arkret::SessionGrantSecret::from_bytes([19; 32]);
            g2.previous_session_grant_id = Some(g1.session_grant_id.clone());
            let request0 = factory
                .refresh_options(&g0, &SessionRefreshOptions::default())
                .unwrap()
                .request
                .unwrap();
            let late_transport = transport.clone();
            let late_request = request0.clone();
            let late =
                tokio::spawn(
                    async move { late_transport.refresh_session_grant(late_request).await },
                );
            // This is an actual pending HTTP response, not a reordered helper call.
            let pending = receive(&listener).await;
            let early_transport = transport.clone();
            let early =
                tokio::spawn(async move { early_transport.refresh_session_grant(request0).await });
            reply(receive(&listener).await, &g1).await;
            assert_eq!(early.await.unwrap().unwrap(), g1);
            let state1 = state_from(&g1);
            let request1 = factory
                .refresh_options(&state1, &SessionRefreshOptions::default())
                .unwrap()
                .request
                .unwrap();
            let next_transport = transport.clone();
            let next =
                tokio::spawn(async move { next_transport.refresh_session_grant(request1).await });
            reply(receive(&listener).await, &g2).await;
            assert_eq!(next.await.unwrap().unwrap(), g2);
            let state2 = state_from(&g2);
            factory.build(&state2).unwrap();
            let before = transport
                .issued
                .lock()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            reply(pending, &g1).await;
            let refused = late.await.unwrap().unwrap_err().to_string();
            assert!(refused.contains("retained predecessor"), "{refused}");
            assert_eq!(
                transport
                    .issued
                    .lock()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                before
            );
            factory.build(&state2).unwrap();
        })
        .await
        .expect("bounded controlled refresh exchange");
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
                &state.session_grant,
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
        let unsigned = AgentSessionGrantSigningInput::new(
            state.account_id.principal_id.clone(),
            savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap(),
            authorization_event(),
            arkret::RequestedAgentSessionGrantScope {
                realm_ids: Vec::new(),
                strand_ids: Vec::new(),
                track_names: Vec::new(),
            },
            None,
            SessionGrantDpopBindingProof {
                proof_jwt: "unit-only-dpop-binding".to_owned(),
            },
            arkret::AgentSessionGrantProofInput {
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
            account_id: state.account_id.clone(),
            runtime_fence: Arc::new(|| true),
            agent_key_authorization_ref: authorization_event(),
            runtime_signing_key: Arc::new(signing_key()),
            dpop_signing_key: Arc::new(SigningKey::from_bytes(&[44; 32])),
            dpop_jkt: agent_session_state().dpop_jkt.clone().unwrap(),
            verification_method: DidUrl::new("did:web:agent.example#runtime-1").unwrap(),
            requested_scope: savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap(),
            session_transport: test_session_transport(),
            content_realm: None,
            content_sessions: None,
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
                &state.session_grant,
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
    fn fresh_content_issue_binds_one_resource_and_a_new_signed_intent() {
        let factory = content_test_factory();
        let realm = RealmId::new("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI").unwrap();
        let before = factory.requested_scope.clone();
        let first = factory.fresh_content_request(&realm).unwrap();
        let second = factory.fresh_content_request(&realm).unwrap();
        assert_ne!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );
        for request in [first, second] {
            let arkret::SessionGrantRequestBody::Agent(request) = request else {
                panic!("Agent issue required")
            };
            request.validate().unwrap();
            assert_eq!(
                request.agent_scope_request.realm_ids.as_slice(),
                std::slice::from_ref(&realm)
            );
            assert!(request.agent_scope_request.strand_ids.is_empty());
            assert_eq!(request.requested_scope, before);
            assert_eq!(
                request.agent_key_authorization_ref,
                factory.agent_key_authorization_ref
            );
            assert_eq!(
                request.proof.verification_method,
                factory.verification_method
            );
            let signature =
                Signature::from_slice(&arkret::base64url_decode(&request.proof.signature).unwrap())
                    .unwrap();
            factory
                .runtime_signing_key
                .verifying_key()
                .verify_strict(
                    &request.proof.canonical_signing_bytes().unwrap(),
                    &signature,
                )
                .unwrap();
        }
        assert_eq!(factory.requested_scope, before);
        let stopped = AgentAuthenticatedTransportFactory {
            runtime_fence: Arc::new(|| false),
            ..factory
        };
        assert!(stopped.fresh_content_request(&realm).is_err());
    }

    #[tokio::test]
    async fn content_session_reissues_unusable_grants_and_preserves_unknown_intent() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for state_kind in ["fresh", "narrowed", "invalidated", "expired"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let issued = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let observed_issue = Arc::clone(&issued);
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                observed_issue.store(true, std::sync::atomic::Ordering::SeqCst);
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 4096];
                let header_end = loop {
                    let read = socket.read(&mut chunk).await.unwrap();
                    assert!(read > 0 && bytes.len() < 65_536);
                    bytes.extend_from_slice(&chunk[..read]);
                    if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
                assert!(headers.starts_with("POST /_arkret/gate/account/session-grants HTTP/1.1"));
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                while bytes.len() < header_end + length {
                    let read = socket.read(&mut chunk).await.unwrap();
                    assert!(read > 0 && bytes.len() < 65_536);
                    bytes.extend_from_slice(&chunk[..read]);
                }
                let request: arkret::SessionGrantRequestBody =
                    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                // An unusable transient response establishes no issuance outcome.
                socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                socket.shutdown().await.unwrap();
                request
            });
            let mut factory = content_test_factory();
            factory.session_transport = AgentSessionGrantTransport {
                grant_base_url: base.clone(),
                bootstrap: agent_client_builder(base)
                    .unwrap()
                    .retry(arkret::http_client::RetryConfig::disabled())
                    .build()
                    .unwrap(),
                dpop_signing_key: Arc::clone(&factory.dpop_signing_key),
                issuer_id: factory.account_id.station_id.clone(),
                issued: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            };
            let sessions = Arc::new(AgentContentSessions::default());
            factory.content_sessions = Some(Arc::clone(&sessions));
            let realm =
                RealmId::new("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI").unwrap();
            let mut state = agent_session_state();
            if matches!(state_kind, "narrowed" | "invalidated") {
                state
                    .granted_scope
                    .retain(|action| action != "ak.event.read");
            } else if state_kind == "expired" {
                state.expires_at = arkret::canonical::normalize_timestamp_canonical(
                    Utc::now() - chrono::Duration::seconds(1),
                );
            }
            if state_kind != "expired" {
                record_test_issue(
                    &factory.session_transport,
                    &state,
                    vec![realm.clone()],
                    Vec::new(),
                );
            }
            assert_eq!(factory.build(&state).is_ok(), state_kind != "expired");
            let engine = if state_kind != "invalidated" {
                SessionEngine::with_state(factory.session_transport.clone(), state)
            } else {
                SessionEngine::new(factory.session_transport.clone())
            };
            let entry = Arc::new(tokio::sync::Mutex::new(PendingContentSession {
                request: None,
                engine: Some(engine),
            }));
            sessions
                .realms
                .lock()
                .unwrap()
                .insert(realm.clone(), Arc::clone(&entry));
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                factory.content_provider(&realm),
            )
            .await
            .expect("actual fresh issue has a bounded transport");
            if state_kind == "fresh" {
                assert!(result.is_ok());
                assert!(!issued.load(std::sync::atomic::Ordering::SeqCst));
                let entry = entry.lock().await;
                assert!(entry.engine.is_some());
                assert!(entry.request.is_none());
                server.abort();
                assert!(server.await.unwrap_err().is_cancelled());
                continue;
            }
            assert!(
                result.is_err(),
                "the old cached ceiling cannot serve content"
            );
            let observed = tokio::time::timeout(std::time::Duration::from_secs(5), server)
                .await
                .expect("a new issue must actually reach the issuer")
                .unwrap();
            let entry = entry.lock().await;
            assert!(entry.engine.is_none());
            assert_eq!(
                serde_json::to_value(entry.request.as_ref().unwrap()).unwrap(),
                serde_json::to_value(&observed).unwrap()
            );
            let arkret::SessionGrantRequestBody::Agent(request) = observed else {
                panic!("Agent issue required")
            };
            request.validate().unwrap();
            assert_eq!(request.agent_scope_request.realm_ids, [realm]);
            assert!(request.agent_scope_request.strand_ids.is_empty());
            assert_eq!(
                request.agent_key_authorization_ref,
                factory.agent_key_authorization_ref
            );
            assert_eq!(request.requested_scope, factory.requested_scope);
        }
    }

    #[test]
    fn content_factory_rejects_missing_cross_resource_and_widened_refresh_ceilings() {
        let realm = RealmId::new("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI").unwrap();
        let other = RealmId::new("ak:realm:ASOv-EoZPg5yuM1Pv__u1K8vD3Q9342GxwoWmkKwjqOn").unwrap();
        let factory = AgentAuthenticatedTransportFactory {
            content_realm: Some(realm.clone()),
            ..content_test_factory()
        };
        let state = agent_session_state();
        assert!(factory.build(&state).is_err());
        let with_resources = |realms: Vec<RealmId>, strands: Vec<StrandId>| {
            let transport = test_session_transport();
            transport.issued.lock().unwrap().clear();
            record_test_issue(&transport, &state, realms, strands);
            AgentAuthenticatedTransportFactory {
                session_transport: transport,
                ..factory.clone()
            }
        };
        let exact = with_resources(vec![realm.clone()], Vec::new());
        assert!(exact.build(&state).is_ok());
        let options = exact
            .refresh_options(&state, &SessionRefreshOptions::default())
            .unwrap();
        let wire = serde_json::to_value(options.request.unwrap()).unwrap();
        assert!(wire.get("agent_scope_request").is_none());
        assert!(
            with_resources(vec![other.clone()], Vec::new())
                .build(&state)
                .is_err()
        );
        assert!(
            with_resources(vec![realm.clone(), other], Vec::new())
                .build(&state)
                .is_err()
        );
        assert!(
            with_resources(
                vec![realm],
                vec![
                    StrandId::new("ak:strand:AT_TSQZlyY7Fu85J33nzo3fSau9RjJOeu21RspghP1gC")
                        .unwrap()
                ]
            )
            .build(&state)
            .is_err()
        );
    }

    fn locator_page(
        cut: &str,
        revision: u64,
        ids: &[u8],
        next: Option<&str>,
    ) -> arkret_models_collaboration::sync_frames::demand_sync::RealmListPage {
        use arkret_models_collaboration::sync_frames::demand_sync::{
            AccountRealmView, RealmListMembership, RealmListPage,
        };
        RealmListPage {
            snapshot_cursor: format!("ak:cursor:{cut}"),
            snapshot_revision: revision,
            items: ids
                .iter()
                .map(|id| AccountRealmView {
                    realm_id: RealmId::from_event_id(&EventId::from_digest(
                        arkret::DigestSuite::Sha256,
                        [*id; 32],
                    )),
                    revision: 1,
                    activity_position: 1,
                    membership: RealmListMembership::Join,
                    title: None,
                    default_strand_id: None,
                })
                .collect(),
            next_cursor: next.map(|value| format!("ak:cursor:{value}")),
        }
    }

    #[tokio::test]
    async fn joined_realm_locators_use_the_registered_account_list_over_real_http() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            for index in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let (end, length) = loop {
                    let mut block = [0u8; 4096];
                    let n = socket.read(&mut block).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&block[..n]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                while bytes.len() < end + length {
                    let mut block = [0u8; 4096];
                    let n = socket.read(&mut block).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&block[..n]);
                }
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let line = headers.lines().next().unwrap();
                let parts = line.split_whitespace().collect::<Vec<_>>();
                let request: AccountSubscribeParameters = if parts[0] == "GET" {
                    let url = Url::parse(&format!("http://station{}", parts[1])).unwrap();
                    let query = url
                        .query_pairs()
                        .collect::<std::collections::BTreeMap<_, _>>();
                    AccountSubscribeParameters {
                        after: query.get("after").map(|value| value.to_string()),
                        catchup: Some(false),
                        filter: Some(serde_json::from_str(query.get("filter").unwrap()).unwrap()),
                        realm_list_parameters: Some(
                            serde_json::from_str(query.get("realm_list_parameters").unwrap())
                                .unwrap(),
                        ),
                        replace_filter: None,
                    }
                } else {
                    serde_json::from_slice(&bytes[end..end + length]).unwrap()
                };
                assert_eq!(request.filter.unwrap().realm_ids, Some(Vec::new()));
                let list = request.realm_list_parameters.unwrap();
                assert_eq!(list.limit, Some(100));
                assert_eq!(
                    list.after.as_ref().map(|value| value.as_str()),
                    if index == 0 {
                        None
                    } else {
                        Some("ak:cursor:page1")
                    }
                );
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("authorization: dpop ")
                );
                let page = if index == 0 {
                    locator_page("native-cut", 7, &[1], Some("page1"))
                } else {
                    locator_page("native-cut", 7, &[2], None)
                };
                let frame = serde_json::json!({"kind":"delta","cursor":format!("ak:cursor:account{index}"),"realm_list":page});
                let body = format!("{}\n", serde_json::to_string(&frame).unwrap());
                socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
            }
        });
        let factory = AgentAuthenticatedTransportFactory {
            base_url: base.clone(),
            ..content_test_factory()
        };
        let state = agent_session_state();
        record_test_issue(&factory.session_transport, &state, Vec::new(), Vec::new());
        let inner = factory.build(&state).unwrap();
        let provider = SessionTransportProvider::new(
            SessionEngine::with_state(factory.session_transport.clone(), state.clone()),
            factory,
            SessionRefreshOptions::default(),
        );
        let binding = arkret::StationConnectionBinding {
            service_id: state.audience_id.clone(),
            base_url: base.to_string(),
            trust_domain: arkret::TrustDomainId::new("ak:trust_domain:station.example").unwrap(),
            authentication_metadata: arkret::AuthenticationMetadata::minimal(),
        };
        let own = provider
            .own_station_result_client(inner.clone(), binding.clone())
            .unwrap();
        let client = ArkretHttpClient {
            inner,
            own_station: Some(own),
            session_provider: Some(provider),
            station_binding: Some(binding),
        };
        let realms = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.joined_realm_locators(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(realms.len(), 2);
        server.await.unwrap();
    }

    #[test]
    fn joined_locators_keep_one_cut_and_duplicate_pages_cannot_restart_it() {
        use arkret_models_collaboration::sync_frames::demand_sync::RealmListMembership;
        let mut pages = JoinedRealmLocatorPages::default();
        let mut first = locator_page("cut", 7, &[1, 2], Some("page1"));
        first.items[1].membership = RealmListMembership::Knock;
        assert_eq!(
            pages.push(None, &first).unwrap(),
            Some("ak:cursor:page1".into())
        );
        assert_eq!(
            pages.push(None, &first).unwrap(),
            Some("ak:cursor:page1".into())
        );
        // Account/live-head progress is not a Realm-list cut replacement.
        let second = locator_page("cut", 7, &[3], None);
        assert!(
            pages
                .push(Some("ak:cursor:page1"), &second)
                .unwrap()
                .is_none()
        );
        assert!(pages.finished);
        assert_eq!(pages.joined.len(), 2);
        assert!(
            pages.push(None, &first).unwrap().is_none(),
            "late exact duplicate preserves completed progress"
        );
        assert_eq!(pages.joined.len(), 2);
    }

    #[test]
    fn joined_locator_pages_reject_mixed_cuts_wrong_frontiers_and_equivocation() {
        let first = locator_page("cut", 7, &[1], Some("page1"));
        for bad in [
            locator_page("other", 7, &[2], None),
            locator_page("cut", 8, &[2], None),
            locator_page("cut", 7, &[1], None),
            locator_page("cut", 7, &[2], Some("page1")),
        ] {
            let mut pages = JoinedRealmLocatorPages::default();
            let _ = pages.push(None, &first).unwrap();
            assert!(pages.push(Some("ak:cursor:page1"), &bad).is_err());
            assert_eq!(pages.joined.len(), 1);
            assert!(!pages.finished);
        }
        let mut pages = JoinedRealmLocatorPages::default();
        let _ = pages.push(None, &first).unwrap();
        assert!(
            pages
                .push(None, &locator_page("cut", 7, &[9], Some("page1")))
                .is_err()
        );
        assert!(
            pages
                .push(
                    Some("ak:cursor:unknown"),
                    &locator_page("cut", 7, &[2], None)
                )
                .is_err()
        );
        assert!(
            pages
                .push(
                    Some("ak:cursor:page1"),
                    &locator_page("cut", 7, &[], Some("page2"))
                )
                .is_err()
        );
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
                signature: arkret::Base64UrlString::new("AQ").unwrap(),
            },
        };
        let request = build_mls_key_packages_claim_request(
            "AQEBAQEBAQEBAQEBAQEBAQ".to_owned(),
            "ak:did_core:webvh:z6mkbobfixture",
            "ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI",
            arkret::ScopeRef::Realm {
                realm_id: arkret::RealmId::new(
                    "ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI",
                )
                .unwrap(),
            },
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
