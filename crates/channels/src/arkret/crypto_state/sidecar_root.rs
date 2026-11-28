//! First use of a Sidecar epoch and exact retry across cold runtime restarts.

use arkret_models_crypto::{
    ContentEndpoint, ContentRootBackupEntry, ContentRootItemKind, KeyBackup,
};
use ed25519_dalek::Signer as _;

use super::*;

#[derive(Debug)]
pub(super) struct MissingRootBackup;

impl std::fmt::Display for MissingRootBackup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("content is waiting for a qualified durable root backup")
    }
}

impl std::error::Error for MissingRootBackup {}

fn stale_predecessor(error: &arkret::http_client::Error) -> bool {
    let arkret::http_client::Error::Api { status: 409, error } = error else {
        return false;
    };
    matches!(
        error.extensions.get("reason_code").and_then(|v| v.as_str()),
        Some(
            arkret_wire::ReasonCode::SERIES_SEQ_NOT_MONOTONIC
                | arkret_wire::ReasonCode::SERIES_CHAIN_BROKEN
        )
    )
}

impl FileArkretCryptoStore {
    fn missing_sidecar_root(
        &self,
        original: &arkret::Event,
    ) -> anyhow::Result<Option<(ContentRootBackupEntry, ContentEndpoint)>> {
        let state = self.load()?;
        let group_id = original.scope_ref.canonical_mls_group_id()?;
        let record = state
            .mls_group_states
            .get(group_id.as_str())
            .context("no installed Sidecar epoch")?;
        let group = ArkretMlsGroup::restore_from_state_record(record)?;
        let reference = group_state_ref_for_epoch(&state, group_id.as_str(), group.epoch())
            .context("no accepted Sidecar group-state reference")?
            .parse::<EventId>()?;
        anyhow::ensure!(
            original.actual_signer() == group.local_actor_id(),
            "Sidecar author differs from the installed runtime"
        );
        match content_root_basis(&state, &group, original, &reference, group.epoch()) {
            Ok(_) => return Ok(None),
            Err(error) if error.is::<MissingRootBackup>() => (),
            Err(error) => return Err(error),
        }
        let context =
            accepted_content_root_context(&state, &group, original, &reference, group.epoch())?;
        let root = group.export_content_root(&context)?;
        let root_ref = arkret::crypto::content_history::content_root_ref(&root, &context)?;
        let entry = ContentRootBackupEntry {
            item_kind: ContentRootItemKind::ContentEpochRoot,
            secret_id: format!("root_{}", root_ref.as_str().trim_start_matches("sha256:")),
            root_context: context,
            root_ref,
            root_base64url: root,
        };
        Ok(Some((entry, group.local_content_endpoint()?)))
    }

    fn freeze_sidecar_root(
        &self,
        root: &ContentRootBackupEntry,
        producer: &ContentEndpoint,
        account: &crate::arkret::ArkretAccountConfig,
        prepared: &arkret_models_crypto::SidecarBackupPrepareResponseBody,
    ) -> anyhow::Result<KeyBackup> {
        let _guard = self.acquire_mutation()?;
        let mut state = self.load()?;
        let key = root.root_ref.as_str();
        anyhow::ensure!(
            prepared.root_context == root.root_context,
            "Sidecar epoch changed during preparation"
        );
        let arkret::MlsWelcomeRecipientEndpoint::AgentRuntime {
            verification_method,
        } = &producer.endpoint
        else {
            anyhow::bail!("installed Sidecar leaf is not the Agent runtime");
        };
        anyhow::ensure!(
            producer.actor_id.as_account_id() == Some(&account.actor_account_id)
                && account.verification_method.as_deref() == Some(verification_method.as_str())
                && account.authorized_event_ref.as_deref()
                    == Some(producer.authorization_event_ref.as_str()),
            "configured runtime differs from installed Sidecar leaf"
        );
        if let Some(packet) = state.pending_sidecar_roots.get(key) {
            anyhow::ensure!(
                packet.actor_id == producer.actor_id
                    && packet.custody_authorization_ref.as_ref()
                        == Some(&prepared.custody_authorization_ref)
                    && packet
                        .signature_proof
                        .agent_authorization_event_ref
                        .as_ref()
                        == Some(&producer.authorization_event_ref)
                    && packet.signature_proof.verification_method == *verification_method,
                "frozen Sidecar backup belongs to a different authorization"
            );
            return Ok(packet.clone());
        }
        // A competing writer may already have installed a verified receipt while we awaited H.
        if let Some(packet) = state.content_backup_manifests.values().find(|b| {
            b.actor_id == producer.actor_id
                && b.contents.iter().any(|item| {
                    item.root_ref.as_ref() == Some(&root.root_ref)
                        && item.root_context.as_ref() == Some(&root.root_context)
                })
        }) {
            return Ok(packet.clone());
        }
        let signer = crate::arkret::signer::load_ed25519_signing_key(
            account
                .key_ref
                .as_ref()
                .context("runtime key unavailable")?,
        )?;
        let packet = garth::mls::content_roots::agent_sidecar_publication::seal_agent_sidecar_root(
            prepared,
            root,
            producer,
            &account.controller_account_id,
            chrono::Utc::now(),
            |bytes| Ok(signer.sign(bytes).to_bytes().to_vec()),
        )?;
        state
            .pending_sidecar_roots
            .insert(key.to_owned(), packet.clone());
        self.save(&mut state)?;
        Ok(packet)
    }

    pub async fn ensure_sidecar_content_root(
        &self,
        original: &arkret::Event,
        account: &crate::arkret::ArkretAccountConfig,
        own: &arkret::http_client::own_station_results::OwnStationStateClient,
    ) -> anyhow::Result<()> {
        let arkret::ScopeRef::Sidecar { sidecar_id, .. } = &original.scope_ref else {
            return Ok(());
        };
        let Some((root, producer)) = self.missing_sidecar_root(original)? else {
            return Ok(());
        };
        anyhow::ensure!(
            own.session()?.account_id() == &account.actor_account_id,
            "Sidecar publisher session changed"
        );
        let query = arkret_models_crypto::SidecarBackupPrepareQuery {
            sidecar_id: sidecar_id.clone(),
        };
        let mut refreshed = false;
        let (packet, receipt) = loop {
            let prepared = own.prepare_sidecar_backup(&query).await?;
            let packet = self.freeze_sidecar_root(&root, &producer, account, prepared.value()?)?;
            match own.put_own_key_backup(&packet).await {
                Ok(receipt) => break (packet, receipt),
                Err(error) if stale_predecessor(&error) => {
                    // The Station definitively refused this predecessor. Network errors
                    // and lost receipts retain the exact signed packet.
                    let _guard = self.acquire_mutation()?;
                    let mut state = self.load()?;
                    if state.pending_sidecar_roots.get(root.root_ref.as_str()) == Some(&packet) {
                        state.pending_sidecar_roots.remove(root.root_ref.as_str());
                        self.save(&mut state)?;
                    }
                    if refreshed {
                        return Err(error.into());
                    }
                    refreshed = true;
                }
                Err(error) => return Err(error.into()),
            }
        };
        let _guard = self.acquire_mutation()?;
        let mut state = self.load()?;
        receipt.value()?;
        if let Some(pending) = state.pending_sidecar_roots.get(root.root_ref.as_str()) {
            anyhow::ensure!(
                pending == &packet,
                "Sidecar acknowledgement differs from frozen request"
            );
        }
        state.pending_sidecar_roots.remove(root.root_ref.as_str());
        state
            .content_backup_manifests
            .insert(packet.backup_id.clone(), packet);
        self.save(&mut state)
    }
}
