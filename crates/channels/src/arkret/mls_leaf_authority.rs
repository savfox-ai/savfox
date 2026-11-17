//! Bind a recipient Welcome to its checked KeyPackage claim and accepted Commit.

use arkret::{
    ActorId, CommittedEventFullView, DidCoreId, EventId, KeyPackageClaimRecord,
    KeyPackagesClaimResponseBody, MlsEndpointIdentity, MlsWelcomeDelivery,
};

use super::crypto_state::mls_key_package_record_from_claim;

#[derive(Clone, Debug)]
pub struct VerifiedMlsLeafAuthority {
    pub actor_id: ActorId,
    pub endpoint: MlsEndpointIdentity,
    pub device_authorize_event_id: Option<EventId>,
}

impl VerifiedMlsLeafAuthority {
    /// Call only after the claim outcome and its Station receipt are verified.
    pub fn from_verified_claim(claim: &KeyPackageClaimRecord) -> anyhow::Result<Self> {
        claim.validate_shape().map_err(anyhow::Error::msg)?;
        let record = mls_key_package_record_from_claim(claim)?;
        Ok(Self {
            actor_id: claim.actor_id.clone(),
            endpoint: record.endpoint,
            device_authorize_event_id: claim.device_authorize_event_id.clone(),
        })
    }
}

/// Derive this endpoint's Add authority only after the caller has verified the
/// claim outcome's Station signature and the delivery's producer proof. The
/// accepted full Commit view, rather than a bare Event, supplies the stream
/// coordinate that the MLS join will subsequently check.
pub fn verified_welcome_leaf_authority(
    delivery: &MlsWelcomeDelivery,
    accepted_commit: &CommittedEventFullView,
    local_endpoint: &MlsEndpointIdentity,
    claim_outcome: &KeyPackagesClaimResponseBody,
    local_station: &DidCoreId,
    endpoint_authorization: &EventId,
) -> anyhow::Result<VerifiedMlsLeafAuthority> {
    accepted_commit.validate_shape()?;
    anyhow::ensure!(
        delivery.effective_scope == accepted_commit.event.scope_ref
            && delivery.realm_id == accepted_commit.event.realm_id,
        "MLS Welcome and accepted Commit belong to different scopes"
    );
    anyhow::ensure!(
        delivery.recipient_actor_id.signing_principal_id() == local_endpoint.principal_id(),
        "MLS Welcome recipient differs from the local endpoint principal"
    );
    let endpoint = match local_endpoint {
        MlsEndpointIdentity::HumanDevice { device_id, .. } => garth::LocalMlsEndpoint::device(
            delivery.realm_id.clone(),
            delivery.recipient_actor_id.clone(),
            device_id.clone(),
        ),
        MlsEndpointIdentity::AgentRuntime {
            verification_method,
            agent_key_authorize_event_id,
            ..
        } => {
            anyhow::ensure!(
                agent_key_authorize_event_id == endpoint_authorization,
                "MLS Welcome Agent endpoint authorization differs from the current binding"
            );
            garth::LocalMlsEndpoint::agent_runtime(
                delivery.realm_id.clone(),
                delivery.recipient_actor_id.clone(),
                verification_method.clone(),
            )
        }
    };
    let claim = garth::mls::verify_welcome_claim(
        delivery,
        &endpoint,
        &accepted_commit.event,
        claim_outcome,
        local_station,
        endpoint_authorization,
    )?;
    VerifiedMlsLeafAuthority::from_verified_claim(&claim)
}
