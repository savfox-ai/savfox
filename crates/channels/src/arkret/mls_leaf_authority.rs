//! Bind an installed MLS roster to checked KeyPackage claims.
//!
//! A BasicCredential identifies an Actor, but cannot by itself authorize a
//! newly occupied leaf. The caller supplies the accepted claim for each new
//! leaf; unchanged leaves retain their previously verified binding.

use std::collections::BTreeMap;

use arkret::mls::{ArkretMlsGroup, AuthorLeafCredential, MlsVerifiedLeafBinding};
use arkret::{
    ActorId, CommittedEventFullView, DidCoreId, EventId, KeyPackageClaimRecord,
    KeyPackagesClaimOutcome, MlsEndpointIdentity, MlsWelcomeDelivery,
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
        claim.validate_shape()?;
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
    claim_outcome: &KeyPackagesClaimOutcome,
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
        MlsEndpointIdentity::MinimalMetadataPairwise { .. } => {
            anyhow::bail!("pairwise MLS endpoint cannot consume an ordinary Welcome")
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

/// Install every occupied leaf or fail without persisting a partial roster.
pub fn install_verified_leaf_authority(
    group: &mut ArkretMlsGroup,
    previous: &[MlsVerifiedLeafBinding],
    new_authority: &[VerifiedMlsLeafAuthority],
) -> anyhow::Result<()> {
    let retained = previous
        .iter()
        .map(|binding| (binding.leaf_index, binding))
        .collect::<BTreeMap<_, _>>();
    let mut installed = Vec::new();
    for leaf in group.active_author_leaves() {
        let AuthorLeafCredential::Basic { identity } = &leaf.credential else {
            anyhow::bail!("accepted MLS leaf does not use BasicCredential");
        };
        let actor_id = arkret::decode_mls_basic_credential_identity(identity)?;
        let signature_key: [u8; 32] = leaf
            .signature_key
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("accepted MLS leaf signature key is not Ed25519"))?;
        let signature_key = arkret::Base64UrlString::new(arkret::base64url_encode(signature_key))
            .map_err(anyhow::Error::msg)?;

        if let Some(binding) = retained.get(&leaf.leaf_index)
            && binding.actor_id == actor_id
            && binding.signature_key == signature_key
        {
            installed.push((*binding).clone());
            continue;
        }

        let mut matches = new_authority
            .iter()
            .filter(|authority| authority.actor_id == actor_id);
        let authority = matches
            .next()
            .ok_or_else(|| anyhow::anyhow!("new MLS leaf has no verified Add authority"))?;
        anyhow::ensure!(
            matches.next().is_none(),
            "new MLS leaf has duplicate authority hints"
        );
        let device_authorize_event_id = match &authority.endpoint {
            MlsEndpointIdentity::HumanDevice { .. } => {
                Some(authority.device_authorize_event_id.clone().ok_or_else(|| {
                    anyhow::anyhow!("ordinary MLS leaf authority omits device authorization Event")
                })?)
            }
            _ => None,
        };
        installed.push(MlsVerifiedLeafBinding {
            leaf_index: leaf.leaf_index,
            actor_id,
            endpoint: authority.endpoint.clone(),
            signature_key,
            device_authorize_event_id,
        });
    }
    group.install_verified_leaf_bindings(installed)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use arkret::mls::{ArkretMlsIdentity, ArkretMlsSigner};
    use arkret::{AccountId, DeviceId, DidCoreId, RealmId, ScopeRef};

    use super::*;

    fn creator_group() -> (ArkretMlsGroup, ActorId, MlsEndpointIdentity) {
        let actor_id = ActorId::account(AccountId::new(
            DidCoreId::new("ak:did_core:web:alice.example").unwrap(),
            DidCoreId::new("ak:did_core:web:station.example").unwrap(),
        ));
        let device_id = DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006").unwrap();
        let identity = ArkretMlsIdentity::new_human_device(
            actor_id.clone(),
            device_id,
            ArkretMlsSigner::from_ed25519_signing_key(ed25519_dalek::SigningKey::from_bytes(
                &[6; 32],
            )),
        )
        .unwrap();
        let endpoint = identity.endpoint_identity().clone();
        let group = identity
            .create_group(&ScopeRef::Realm {
                realm_id: RealmId::new("ak:realm:AXGA0fM2a_L3afx2ffIvrX5YVKbExabYEkxTUwvKu9HR")
                    .unwrap(),
            })
            .unwrap();
        (group, actor_id, endpoint)
    }

    #[test]
    fn new_leaf_requires_claim_authority_and_retained_leaf_needs_no_new_claim() {
        let (mut group, actor_id, endpoint) = creator_group();
        assert!(install_verified_leaf_authority(&mut group, &[], &[]).is_err());
        let hint = VerifiedMlsLeafAuthority {
            actor_id,
            endpoint,
            device_authorize_event_id: Some(
                EventId::new("ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6").unwrap(),
            ),
        };
        install_verified_leaf_authority(&mut group, &[], &[hint]).unwrap();
        let retained = group.verified_leaf_bindings().unwrap();
        install_verified_leaf_authority(&mut group, &retained, &[]).unwrap();
        assert_eq!(group.verified_leaf_bindings().unwrap(), retained);
    }
}
