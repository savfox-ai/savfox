//! Crypto fixtures only; local backup selectors do not certify native custody.
use anyhow::{Result, ensure};
use arkret::{ArkretMlsGroup, Event, EventId};
use arkret_models_crypto::*;

pub(super) fn pending_public_refs(
    group: &ArkretMlsGroup,
    commit: &MlsCommitEnvelope,
    binding: &MlsGovernanceBindingPayload,
) -> Result<(arkret::BlobRef, arkret::BlobRef)> {
    let (info, tree) = group.pending_public_group_state_bytes(commit, binding)?;
    let reference = |bytes: &[u8]| {
        arkret::BlobRef::new(format!(
            "ak:blob:{}",
            arkret::canonical::sha256_digest(bytes)
        ))
    };
    Ok((reference(&info)?, reference(&tree)?))
}

pub(super) fn commit_payload(
    group: &ArkretMlsGroup,
    base: EventId,
    revision: u64,
    commit: &MlsCommitEnvelope,
    binding: MlsGovernanceBindingPayload,
) -> Result<MlsCommitPayload> {
    let (info, tree) = pending_public_refs(group, commit, &binding)?;
    Ok(MlsCommitPayload::new(
        base, revision, commit, binding, info, tree,
    )?)
}

pub(super) fn material(
    group: &ArkretMlsGroup,
    event: &Event,
    context: ContentRootContext,
    protection: ContentProtection,
) -> Result<EventContentMaterial> {
    ensure!(
        event.scope_ref == *group.scope() && event.actual_signer() == group.local_actor_id(),
        "content fixture differs from actual producer"
    );
    let root = group.export_content_root(&context)?;
    let root_ref = arkret::crypto::content_history::content_root_ref(&root, &context)?;
    ensure!(
        protection.root_refs.contains(&root_ref) && !protection.backup_refs.is_empty(),
        "content fixture lacks its explicit backup dependency"
    );
    let content_context = ContentContext {
        root_context: context,
        root_ref,
        purpose: ContentPurpose::EventBody,
        encryption_scheme: arkret::EncryptedPayloadScheme::EpochContentAead,
        encryption_algorithm: ContentEncryptionAlgorithm::Aes128Gcm,
        producer: group.local_content_endpoint()?,
        content_key_salt: arkret::crypto::content_history::fresh_content_key_salt()?,
    };
    let key = arkret::crypto::content_history::derive_content_key(&root, &content_context)?;
    Ok(EventContentMaterial {
        key_commitment: arkret::crypto::content_history::content_key_commitment(
            &content_context,
            &key,
        )?,
        content_context,
        outer_event: EventContentMaterial::outer_event_for(event)?,
        protection,
    })
}

/// Standalone AEAD tests only; its local backup selector is not a server receipt.
pub(super) fn local_material(
    group: &ArkretMlsGroup,
    kind: &str,
    reference: &EventId,
) -> Result<EventContentMaterial> {
    let realm = group.scope().realm_id();
    let context = ContentRootContext {
        effective_scope: group.scope().clone(),
        genesis_event_ref: reference.clone(),
        epoch: group.epoch(),
        group_state_ref: reference.clone(),
        cipher_suite: group.group_ciphersuite_canonical_id()?.to_owned(),
        encryption_backup_strategy: arkret::EncryptionBackupStrategy::EpochContentRoots,
        backup_strategy_ref: EventId::from_digest(
            realm.digest_suite_code().digest_suite(),
            realm.digest_bytes(),
        ),
    };
    let root = group.export_content_root(&context)?;
    let root_ref = arkret::crypto::content_history::content_root_ref(&root, &context)?;
    let event = arkret_wire::test_support::raw_event_for_actor_at(
        kind,
        group.scope().clone(),
        group.local_actor_id().clone(),
        serde_json::json!({}),
        "2026-11-09T00:00:00.000Z".parse()?,
    )?;
    material(
        group,
        &event,
        context,
        ContentProtection {
            root_refs: vec![root_ref],
            blob_refs: vec![],
            backup_refs: vec![arkret::BackupId::new(arkret::new_prefixed_uuid7(
                "ak:backup:",
            ))?],
        },
    )
}

pub(super) fn original(scope: &arkret::ScopeRef, actor: &arkret::ActorId) -> Event {
    arkret_wire::test_support::raw_event_for_actor_at(
        "ak.message.create",
        scope.clone(),
        actor.clone(),
        serde_json::json!({}),
        "2026-11-09T00:00:00.000Z".parse().unwrap(),
    )
    .unwrap()
}

pub(super) fn candidate(payload: &arkret::EncryptedPayload) -> super::ArkretEventContent {
    let mut original = payload.pre_encryption_header.material.outer_event.clone();
    original["event_id"] =
        serde_json::to_value(&payload.pre_encryption_header.group_state_ref).unwrap();
    original["payload"] = serde_json::json!({});
    let event: Event = serde_json::from_value(original).unwrap();
    super::ArkretEventContent::from_original(&event, &payload.to_envelope().unwrap()).unwrap()
}
