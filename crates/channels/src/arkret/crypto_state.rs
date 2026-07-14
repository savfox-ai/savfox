//! Local Arkret crypto state for Savfox channel adapters.
//!
//! The SDK owns the protocol objects (`CryptoStoreBinding`, `MemoryCryptoStore`
//! and `ArkretMlsGroup`). This module gives Savfox a small file-backed wrapper
//! so account-mode and applet-mode can persist decryption failures, MLS group
//! snapshots, recovery plans and realm encryption policy under `SAVFOX_HOME`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use arkret::crypto_protocol::{CryptoStoreBinding, UnableToDecryptReason, UnableToDecryptRecord};
use arkret::mls::{ArkretMlsGroup, ArkretMlsIdentity};
use arkret::{
    CryptoStore, DeviceId, Did, EncryptedPayload, EncryptedPayloadScheme, EventId,
    FeatureSafetyReport, MemoryCryptoStore, MlsGroupStateRecord, MlsKeyPackageRecord,
    MlsKeyPackageState, MlsRecoveryAction, MlsWelcomeEnvelope, MlsWelcomePayload, RealmId,
    current_feature_safety_report,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const STATE_VERSION: &str = "savfox.arkret.crypto_state.v1";
const CONTENT_BLOCK_JSON: &str = "application/vnd.arkret.content-block+json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArkretContentEncryptionFloor {
    AllowPlaintext,
    E2eeRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArkretRealmCryptoPolicy {
    pub realm_id: String,
    pub content_encryption_floor: ArkretContentEncryptionFloor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mls_group_id: Option<String>,
    pub source: String,
    pub updated_at: DateTime<Utc>,
}

impl ArkretRealmCryptoPolicy {
    #[must_use]
    pub fn requires_e2ee(&self) -> bool {
        self.content_encryption_floor == ArkretContentEncryptionFloor::E2eeRequired
    }

    #[must_use]
    pub fn group_id_for_realm(&self) -> &str {
        self.mls_group_id
            .as_deref()
            .unwrap_or(self.realm_id.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArkretBootstrapRecord {
    pub group_id: String,
    pub required_epoch: u64,
    pub local_epoch: Option<u64>,
    pub action: MlsRecoveryAction,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArkretKeyBackupState {
    pub restore_needed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_needed_for_group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_needed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub restored_secret_count: usize,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArkretMlsIdentityStateRecord {
    pub principal_id: Did,
    pub device_id: DeviceId,
    pub private_state: Vec<u8>,
    #[serde(default)]
    pub last_resort_key_package: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keypackage_id: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for ArkretMlsIdentityStateRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArkretMlsIdentityStateRecord")
            .field("principal_id", &self.principal_id)
            .field("device_id", &self.device_id)
            .field(
                "private_state",
                &format_args!("<redacted {} bytes>", self.private_state.len()),
            )
            .field("last_resort_key_package", &self.last_resort_key_package)
            .field("keypackage_id", &self.keypackage_id)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArkretMlsWelcomeConsumeBinding {
    pub keypackage_ref: String,
    pub claim_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub welcome_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strand_id: Option<String>,
    pub mls_group_id: String,
    pub epoch: u64,
}

impl ArkretMlsWelcomeConsumeBinding {
    #[must_use]
    pub fn cache_key(&self) -> String {
        format!(
            "{}#{}#{}#{}",
            self.mls_group_id, self.epoch, self.keypackage_ref, self.claim_id
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArkretCryptoStateFile {
    pub version: String,
    pub scope_id: String,
    pub sdk_features: Vec<String>,
    pub binding: CryptoStoreBinding,
    pub mls_store_json: String,
    #[serde(default)]
    pub mls_identities: BTreeMap<String, ArkretMlsIdentityStateRecord>,
    #[serde(default)]
    pub mls_key_packages: BTreeMap<String, MlsKeyPackageRecord>,
    #[serde(default)]
    pub mls_welcome_consume_bindings: BTreeMap<String, ArkretMlsWelcomeConsumeBinding>,
    #[serde(default)]
    pub realm_policies: BTreeMap<String, ArkretRealmCryptoPolicy>,
    #[serde(default)]
    pub bootstrap: BTreeMap<String, ArkretBootstrapRecord>,
    #[serde(default)]
    pub key_backup: ArkretKeyBackupState,
}

impl ArkretCryptoStateFile {
    fn new(scope_id: String) -> anyhow::Result<Self> {
        let store = MemoryCryptoStore::new();
        Ok(Self {
            version: STATE_VERSION.to_owned(),
            scope_id,
            sdk_features: current_feature_safety_report().enabled_features,
            binding: CryptoStoreBinding::default(),
            mls_store_json: store
                .export_backup_json()
                .map_err(|err| anyhow::anyhow!("arkret crypto store export: {err}"))?,
            mls_identities: BTreeMap::new(),
            mls_key_packages: BTreeMap::new(),
            mls_welcome_consume_bindings: BTreeMap::new(),
            realm_policies: BTreeMap::new(),
            bootstrap: BTreeMap::new(),
            key_backup: ArkretKeyBackupState::default(),
        })
    }

    fn mls_store(&self) -> anyhow::Result<MemoryCryptoStore> {
        let mut store = MemoryCryptoStore::new();
        store
            .import_backup_json(&self.mls_store_json)
            .map_err(|err| anyhow::anyhow!("arkret crypto store import: {err}"))?;
        Ok(store)
    }

    fn set_mls_store(&mut self, store: &MemoryCryptoStore) -> anyhow::Result<()> {
        self.mls_store_json = store
            .export_backup_json()
            .map_err(|err| anyhow::anyhow!("arkret crypto store export: {err}"))?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct FileArkretCryptoStore {
    path: PathBuf,
    scope_id: String,
}

impl FileArkretCryptoStore {
    #[must_use]
    pub fn for_account(savfox_home: &Path, channel_id: &str, account_id: &str) -> Self {
        let scope_id = account_scope_id(channel_id, account_id);
        Self::new(savfox_home, scope_id)
    }

    #[must_use]
    pub fn for_applet(savfox_home: &Path, config_id: &str) -> Self {
        let scope_id = applet_scope_id(config_id);
        Self::new(savfox_home, scope_id)
    }

    #[must_use]
    pub fn new(savfox_home: &Path, scope_id: String) -> Self {
        let path = savfox_home
            .join("gateway")
            .join("arkret-crypto")
            .join(format!("{}.json", safe_file_stem(&scope_id)));
        Self { path, scope_id }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn feature_report() -> anyhow::Result<FeatureSafetyReport> {
        let report = current_feature_safety_report();
        report
            .validate()
            .map_err(|err| anyhow::anyhow!("arkret crypto feature set is unsafe: {err}"))?;
        Ok(report)
    }

    pub fn load(&self) -> anyhow::Result<ArkretCryptoStateFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.is_empty() => ArkretCryptoStateFile::new(self.scope_id.clone()),
            Ok(bytes) => {
                let mut state: ArkretCryptoStateFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {}", self.path.display()))?;
                if state.version != STATE_VERSION {
                    anyhow::bail!(
                        "unsupported Arkret crypto state version '{}' in {}",
                        state.version,
                        self.path.display()
                    );
                }
                if state.scope_id != self.scope_id {
                    anyhow::bail!(
                        "Arkret crypto state scope mismatch: expected '{}', got '{}'",
                        self.scope_id,
                        state.scope_id
                    );
                }
                state.sdk_features = current_feature_safety_report().enabled_features;
                Ok(state)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                ArkretCryptoStateFile::new(self.scope_id.clone())
            }
            Err(err) => Err(err).with_context(|| format!("read {}", self.path.display())),
        }
    }

    pub fn save(&self, state: &ArkretCryptoStateFile) -> anyhow::Result<()> {
        if state.scope_id != self.scope_id {
            anyhow::bail!(
                "refusing to save Arkret crypto state for scope '{}' into '{}'",
                state.scope_id,
                self.scope_id
            );
        }
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(state)?;
        let tmp = self.tmp_path();
        std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).with_context(|| {
            let _ = std::fs::remove_file(&tmp);
            format!("rename {} -> {}", tmp.display(), self.path.display())
        })?;
        Ok(())
    }

    pub fn ensure_created(&self) -> anyhow::Result<()> {
        let state = self.load()?;
        self.save(&state)
    }

    pub fn upsert_realm_policy(&self, policy: ArkretRealmCryptoPolicy) -> anyhow::Result<()> {
        let mut state = self.load()?;
        state.realm_policies.insert(policy.realm_id.clone(), policy);
        self.save(&state)
    }

    pub fn update_realm_policies_from_sync(&self, realms_value: &Value) -> anyhow::Result<usize> {
        let Some(realms) = realms_value.as_object() else {
            return Ok(0);
        };
        let mut state = self.load()?;
        let mut updated = 0usize;
        for (realm_id, realm_value) in realms {
            if let Some(policy) = extract_realm_crypto_policy(realm_id, realm_value) {
                state.realm_policies.insert(policy.realm_id.clone(), policy);
                updated += 1;
            }
        }
        if updated > 0 {
            self.save(&state)?;
        }
        Ok(updated)
    }

    pub fn ensure_mls_key_package(
        &self,
        principal_id: &str,
        device_id: &str,
        last_resort: bool,
    ) -> anyhow::Result<MlsKeyPackageRecord> {
        let mut state = self.load()?;
        let principal = Did::new(principal_id.to_owned())
            .with_context(|| format!("invalid Arkret principal DID '{principal_id}'"))?;
        let device = DeviceId::new(device_id.to_owned())
            .with_context(|| format!("invalid Arkret device id '{device_id}'"))?;
        let identity_key = mls_identity_key(&principal, &device);
        let cache_key = mls_key_package_cache_key(&principal, &device, last_resort);

        if state.mls_identities.contains_key(&identity_key)
            && let Some(record) = state
                .mls_key_packages
                .get(&cache_key)
                .or_else(|| state.mls_key_packages.get(&identity_key))
                .cloned()
            && local_key_package_can_be_published(&record, last_resort)
        {
            return Ok(record);
        }

        let identity = if let Some(record) = state.mls_identities.get(&identity_key) {
            restore_mls_identity(record)?
        } else {
            ArkretMlsIdentity::new_basic(principal.clone(), device.clone())
                .map_err(|err| anyhow::anyhow!("create Arkret MLS identity: {err}"))?
        };
        let record = if last_resort {
            identity
                .last_resort_key_package_record()
                .map_err(|err| anyhow::anyhow!("create Arkret MLS last-resort KeyPackage: {err}"))?
        } else {
            identity
                .key_package_record()
                .map_err(|err| anyhow::anyhow!("create Arkret MLS KeyPackage: {err}"))?
        };
        let private_state = identity
            .export_private_state()
            .map_err(|err| anyhow::anyhow!("export Arkret MLS identity state: {err}"))?;

        // Keep KeyPackages in a Savfox-owned string-keyed map: the SDK store
        // currently serializes its keypackage map with tuple keys, which
        // serde_json cannot export as an object key.
        state.mls_key_packages.insert(cache_key, record.clone());
        state.mls_identities.insert(
            identity_key,
            ArkretMlsIdentityStateRecord {
                principal_id: principal,
                device_id: device,
                private_state,
                last_resort_key_package: last_resort,
                keypackage_id: Some(record.keypackage_id.clone()),
                updated_at: Utc::now(),
            },
        );
        self.save(&state)?;
        Ok(record)
    }

    pub fn mark_mls_key_package_claimed(
        &self,
        keypackage_ref_or_id: &str,
        claim_id: &str,
    ) -> anyhow::Result<Option<MlsKeyPackageRecord>> {
        if claim_id.trim().is_empty() {
            anyhow::bail!("Arkret MLS KeyPackage claim id must not be empty");
        }
        self.update_cached_mls_key_package(keypackage_ref_or_id, |record| {
            if matches!(
                record.state,
                MlsKeyPackageState::Consumed | MlsKeyPackageState::Revoked
            ) {
                anyhow::bail!(
                    "refusing to claim Arkret MLS KeyPackage '{}' in state {:?}",
                    record.keypackage_id,
                    record.state
                );
            }
            record.state = MlsKeyPackageState::Claimed;
            record.claim_id = Some(claim_id.to_owned());
            Ok(())
        })
    }

    pub fn mark_mls_key_package_consumed(
        &self,
        keypackage_ref_or_id: &str,
    ) -> anyhow::Result<Option<MlsKeyPackageRecord>> {
        self.update_cached_mls_key_package(keypackage_ref_or_id, |record| {
            if record.last_resort {
                return Ok(());
            }
            if record.state == MlsKeyPackageState::Revoked {
                anyhow::bail!(
                    "refusing to consume revoked Arkret MLS KeyPackage '{}'",
                    record.keypackage_id
                );
            }
            record.state = MlsKeyPackageState::Consumed;
            Ok(())
        })
    }

    pub fn record_mls_welcome(&self, welcome: MlsWelcomeEnvelope) -> anyhow::Result<()> {
        self.record_mls_welcome_inner(welcome, None)
    }

    fn record_mls_welcome_inner(
        &self,
        welcome: MlsWelcomeEnvelope,
        consume_binding: Option<ArkretMlsWelcomeConsumeBinding>,
    ) -> anyhow::Result<()> {
        let mut state = self.load()?;
        let mut store = state.mls_store()?;
        let local_epoch = store
            .mls_group_state(&welcome.group_id)
            .map(|record| record.epoch);
        store
            .put_welcome(welcome.clone())
            .map_err(|err| anyhow::anyhow!("persist Arkret MLS Welcome: {err}"))?;
        state.bootstrap.insert(
            welcome.group_id.clone(),
            ArkretBootstrapRecord {
                group_id: welcome.group_id,
                required_epoch: welcome.epoch,
                local_epoch,
                action: MlsRecoveryAction::ConsumeWelcome,
                updated_at: Utc::now(),
            },
        );
        if let Some(binding) = consume_binding {
            if let Some(cache_key) =
                find_mls_key_package_cache_key(&state.mls_key_packages, &binding.keypackage_ref)
                && let Some(record) = state.mls_key_packages.get_mut(&cache_key)
                && !matches!(
                    record.state,
                    MlsKeyPackageState::Consumed | MlsKeyPackageState::Revoked
                )
            {
                record.state = MlsKeyPackageState::Claimed;
                record.claim_id = Some(binding.claim_id.clone());
            }
            state
                .mls_welcome_consume_bindings
                .insert(binding.cache_key(), binding);
        }
        state.set_mls_store(&store)?;
        self.save(&state)
    }

    pub fn record_mls_welcome_from_value(
        &self,
        value: &Value,
    ) -> anyhow::Result<Option<MlsWelcomeEnvelope>> {
        let Some(welcome) = extract_mls_welcome_envelope(value) else {
            return Ok(None);
        };
        let consume_binding = extract_mls_welcome_consume_binding(value);
        self.record_mls_welcome_inner(welcome.clone(), consume_binding)?;
        Ok(Some(welcome))
    }

    pub fn mark_mls_welcome_consume_binding_acked(
        &self,
        binding: &ArkretMlsWelcomeConsumeBinding,
    ) -> anyhow::Result<()> {
        let mut state = self.load()?;
        state
            .mls_welcome_consume_bindings
            .remove(&binding.cache_key());
        self.save(&state)
    }

    pub fn plan_bootstrap_for_payload(
        &self,
        principal_id: &str,
        device_id: &str,
        payload: &EncryptedPayload,
    ) -> anyhow::Result<ArkretBootstrapRecord> {
        let mut state = self.load()?;
        let store = state.mls_store()?;
        let principal = Did::new(principal_id.to_owned())
            .with_context(|| format!("invalid Arkret principal DID '{principal_id}'"))?;
        let device = DeviceId::new(device_id.to_owned())
            .with_context(|| format!("invalid Arkret device id '{device_id}'"))?;
        let local_epoch = store
            .mls_group_state(&payload.group_id)
            .map(|record| record.epoch);
        let plan = store.plan_mls_recovery(
            &payload.group_id,
            local_epoch,
            payload.epoch,
            &principal,
            &device,
        );
        let record = ArkretBootstrapRecord {
            group_id: plan.group_id,
            required_epoch: payload.epoch,
            local_epoch,
            action: plan.action,
            updated_at: Utc::now(),
        };
        if matches!(
            record.action,
            MlsRecoveryAction::RequestEpochRecovery { .. }
        ) {
            state.key_backup.restore_needed = true;
            state.key_backup.last_needed_for_group_id = Some(record.group_id.clone());
            state.key_backup.last_needed_at = Some(record.updated_at);
        }
        state
            .bootstrap
            .insert(record.group_id.clone(), record.clone());
        self.save(&state)?;
        Ok(record)
    }

    pub fn record_unable_to_decrypt(
        &self,
        event_id: &str,
        realm_id: &str,
        sender: &str,
        encrypted_content: EncryptedPayload,
        reason: UnableToDecryptReason,
    ) -> anyhow::Result<()> {
        let mut state = self.load()?;
        let record = UnableToDecryptRecord {
            event_id: EventId::new(event_id.to_owned())
                .with_context(|| format!("invalid Arkret event id '{event_id}'"))?,
            realm_id: RealmId::new(realm_id.to_owned())
                .with_context(|| format!("invalid Arkret realm id '{realm_id}'"))?,
            sender: Did::new(sender.to_owned())
                .with_context(|| format!("invalid Arkret sender DID '{sender}'"))?,
            reason,
            encrypted_content,
            first_seen_at: Utc::now(),
        };
        state.binding.record_unable_to_decrypt(record);
        self.save(&state)
    }

    pub fn try_decrypt_content_block(
        &self,
        payload: &EncryptedPayload,
    ) -> anyhow::Result<ArkretDecryptOutcome> {
        let detailed = self.try_decrypt_content_block_detailed(payload)?;
        Ok(match detailed {
            ArkretDecryptDetailedOutcome::Decrypted {
                content,
                consume_bindings: _,
            } => ArkretDecryptOutcome::Decrypted(content),
            ArkretDecryptDetailedOutcome::MissingGroupState => {
                ArkretDecryptOutcome::MissingGroupState
            }
            ArkretDecryptDetailedOutcome::UnsupportedScheme(scheme) => {
                ArkretDecryptOutcome::UnsupportedScheme(scheme)
            }
        })
    }

    pub fn try_decrypt_content_block_detailed(
        &self,
        payload: &EncryptedPayload,
    ) -> anyhow::Result<ArkretDecryptDetailedOutcome> {
        let mut state = self.load()?;
        let mut store = state.mls_store()?;
        let mut joined_from_welcome = None;
        if store.mls_group_state(&payload.group_id).is_none() {
            let Some((updated, welcome)) =
                try_consume_stored_welcome_for_payload(&state, &mut store, payload)?
            else {
                return Ok(ArkretDecryptDetailedOutcome::MissingGroupState);
            };
            joined_from_welcome = Some(welcome);
            state.bootstrap.insert(
                updated.group_id.clone(),
                ArkretBootstrapRecord {
                    group_id: updated.group_id,
                    required_epoch: updated.epoch,
                    local_epoch: Some(updated.epoch),
                    action: MlsRecoveryAction::ConsumeWelcome,
                    updated_at: Utc::now(),
                },
            );
            state.set_mls_store(&store)?;
            self.save(&state)?;
        }
        let record = store
            .mls_group_state(&payload.group_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Arkret MLS admission did not produce group state"))?;
        let mut group = ArkretMlsGroup::restore_from_state_record(&record)
            .map_err(|err| anyhow::anyhow!("restore Arkret MLS group: {err}"))?;
        let plaintext = match payload.scheme {
            EncryptedPayloadScheme::MlsRfc9420 => group
                .decrypt_payload(payload)
                .map_err(|err| anyhow::anyhow!("decrypt Arkret MLS payload: {err}"))?,
            EncryptedPayloadScheme::MlsExporterAeadV1 => {
                return Ok(ArkretDecryptDetailedOutcome::UnsupportedScheme(
                    payload.scheme.as_str().to_owned(),
                ));
            }
        };
        let content = serde_json::from_slice(&plaintext)
            .with_context(|| "decrypted Arkret content block is not JSON")?;
        let updated = group
            .persist_state(&mut store)
            .map_err(|err| anyhow::anyhow!("persist Arkret MLS group: {err}"))?;
        state.set_mls_store(&store)?;
        state.bootstrap.insert(
            updated.group_id.clone(),
            ArkretBootstrapRecord {
                group_id: updated.group_id,
                required_epoch: updated.epoch,
                local_epoch: Some(updated.epoch),
                action: MlsRecoveryAction::UseLocalState,
                updated_at: Utc::now(),
            },
        );
        let consume_bindings = joined_from_welcome
            .as_ref()
            .map(|welcome| {
                state
                    .mls_welcome_consume_bindings
                    .values()
                    .filter(|binding| {
                        binding.mls_group_id == welcome.group_id && binding.epoch == welcome.epoch
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.save(&state)?;
        Ok(ArkretDecryptDetailedOutcome::Decrypted {
            content,
            consume_bindings,
        })
    }

    pub fn encrypt_content_block_for_realm(
        &self,
        realm_id: &str,
        content: &Value,
    ) -> anyhow::Result<ArkretEncryptOutcome> {
        let mut state = self.load()?;
        let Some(policy) = state.realm_policies.get(realm_id).cloned() else {
            return Ok(ArkretEncryptOutcome::PlaintextAllowed);
        };
        if !policy.requires_e2ee() {
            return Ok(ArkretEncryptOutcome::PlaintextAllowed);
        }
        let mut store = state.mls_store()?;
        let group_id = policy.group_id_for_realm().to_owned();
        let Some(record) = store.mls_group_state(&group_id).cloned() else {
            return Ok(ArkretEncryptOutcome::MissingRequiredGroupState {
                group_id,
                realm_id: realm_id.to_owned(),
            });
        };
        let mut group = ArkretMlsGroup::restore_from_state_record(&record)
            .map_err(|err| anyhow::anyhow!("restore Arkret MLS group: {err}"))?;
        let plaintext = serde_json::to_vec(content)?;
        let payload = group
            .encrypt_payload(CONTENT_BLOCK_JSON, &plaintext)
            .map_err(|err| anyhow::anyhow!("encrypt Arkret MLS payload: {err}"))?;
        group
            .persist_state(&mut store)
            .map_err(|err| anyhow::anyhow!("persist Arkret MLS group: {err}"))?;
        state.set_mls_store(&store)?;
        self.save(&state)?;
        Ok(ArkretEncryptOutcome::Encrypted(serde_json::to_value(
            payload,
        )?))
    }

    fn update_cached_mls_key_package(
        &self,
        keypackage_ref_or_id: &str,
        update: impl FnOnce(&mut MlsKeyPackageRecord) -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<MlsKeyPackageRecord>> {
        let mut state = self.load()?;
        let Some(cache_key) =
            find_mls_key_package_cache_key(&state.mls_key_packages, keypackage_ref_or_id)
        else {
            return Ok(None);
        };
        let record = state
            .mls_key_packages
            .get_mut(&cache_key)
            .ok_or_else(|| anyhow::anyhow!("Arkret MLS KeyPackage cache key disappeared"))?;
        update(record)?;
        let updated = record.clone();
        self.save(&state)?;
        Ok(Some(updated))
    }

    fn tmp_path(&self) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "arkret-crypto.json".into());
        name.push(".tmp");
        self.path.with_file_name(name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArkretDecryptOutcome {
    Decrypted(Value),
    MissingGroupState,
    UnsupportedScheme(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArkretDecryptDetailedOutcome {
    Decrypted {
        content: Value,
        consume_bindings: Vec<ArkretMlsWelcomeConsumeBinding>,
    },
    MissingGroupState,
    UnsupportedScheme(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArkretEncryptOutcome {
    PlaintextAllowed,
    Encrypted(Value),
    MissingRequiredGroupState { realm_id: String, group_id: String },
}

pub fn mls_key_package_record_from_claim(
    claim: &arkret::KeyPackageClaimRecord,
) -> anyhow::Result<MlsKeyPackageRecord> {
    let device_id = DeviceId::new(claim.device_id.clone()).with_context(|| {
        format!(
            "invalid Arkret KeyPackage claim device id '{}'",
            claim.device_id
        )
    })?;
    Ok(MlsKeyPackageRecord {
        keypackage_id: claim.keypackage_ref.clone(),
        principal_id: claim.principal_id.clone(),
        device_id,
        key_package: claim.key_package.clone(),
        keypackage_ref: claim.keypackage_digest.clone(),
        cipher_suites: Vec::new(),
        capabilities: claim.capabilities.clone(),
        state: MlsKeyPackageState::Claimed,
        claim_id: Some(claim.claim_id.clone()),
        created_at: Utc::now(),
        expires_at: Some(claim.expires_at),
        device_signature: None,
        last_resort: claim.last_resort.unwrap_or(false),
    })
}

#[must_use]
pub fn account_scope_id(channel_id: &str, account_id: &str) -> String {
    format!("account:{channel_id}:{account_id}")
}

#[must_use]
pub fn applet_scope_id(config_id: &str) -> String {
    format!("applet:{config_id}")
}

pub fn extract_encrypted_payload_from_message_content(content: &Value) -> Option<EncryptedPayload> {
    for key in ["encrypted_content", "encrypted_payload"] {
        if let Some(value) = content.get(key)
            && let Ok(payload) = serde_json::from_value::<EncryptedPayload>(value.clone())
        {
            return Some(payload);
        }
    }
    let inner = content.get("content")?;
    if inner
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "ak.content.encrypted")
    {
        for key in ["payload", "encrypted_payload", "encrypted_content"] {
            if let Some(value) = inner.get(key)
                && let Ok(payload) = serde_json::from_value::<EncryptedPayload>(value.clone())
            {
                return Some(payload);
            }
        }
        if let Ok(payload) = serde_json::from_value::<EncryptedPayload>(inner.clone()) {
            return Some(payload);
        }
    }
    None
}

#[must_use]
pub fn message_content_has_encrypted_carrier(content: &Value) -> bool {
    content.get("encrypted_content").is_some()
        || content.get("encrypted_payload").is_some()
        || content
            .get("content")
            .and_then(|content| content.get("kind"))
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "ak.content.encrypted")
}

pub fn extract_mls_welcome_envelope(value: &Value) -> Option<MlsWelcomeEnvelope> {
    if let Ok(welcome) = serde_json::from_value::<MlsWelcomeEnvelope>(value.clone()) {
        return Some(welcome);
    }
    for key in ["mls_welcome", "welcome_envelope", "payload", "content"] {
        if let Some(candidate) = value.get(key)
            && let Ok(welcome) = serde_json::from_value::<MlsWelcomeEnvelope>(candidate.clone())
        {
            return Some(welcome);
        }
    }
    None
}

pub fn extract_mls_welcome_consume_binding(
    value: &Value,
) -> Option<ArkretMlsWelcomeConsumeBinding> {
    extract_mls_welcome_consume_binding_inner(value, 6)
}

fn extract_mls_welcome_consume_binding_inner(
    value: &Value,
    remaining_depth: usize,
) -> Option<ArkretMlsWelcomeConsumeBinding> {
    if let Ok(payload) = serde_json::from_value::<MlsWelcomePayload>(value.clone()) {
        let welcome_ref = payload
            .carrier
            .welcome_ref()
            .or_else(|| payload.carrier.encrypted_welcome_ref())
            .map(str::to_owned);
        return Some(ArkretMlsWelcomeConsumeBinding {
            keypackage_ref: payload.keypackage_ref.to_string(),
            claim_id: payload.claim_id.into_string(),
            welcome_ref,
            realm_id: Some(payload.claim_envelope.intended_realm_id.as_str().to_owned()),
            strand_id: None,
            mls_group_id: payload.mls_group_id.to_string(),
            epoch: payload.epoch,
        });
    }

    if let Value::Object(object) = value
        && let Some(binding) = extract_mls_welcome_consume_binding_from_object(object)
    {
        return Some(binding);
    }

    if remaining_depth == 0 {
        return None;
    }

    match value {
        Value::Array(items) => items
            .iter()
            .find_map(|item| extract_mls_welcome_consume_binding_inner(item, remaining_depth - 1)),
        Value::Object(object) => object
            .values()
            .find_map(|item| extract_mls_welcome_consume_binding_inner(item, remaining_depth - 1)),
        _ => None,
    }
}

fn extract_mls_welcome_consume_binding_from_object(
    object: &serde_json::Map<String, Value>,
) -> Option<ArkretMlsWelcomeConsumeBinding> {
    let keypackage_ref = string_field(object, &["keypackage_ref", "keyPackageRef"])?;
    let claim_id = string_field(object, &["claim_id", "claimId"]).or_else(|| {
        object
            .get("claim_ref")
            .or_else(|| object.get("claimRef"))
            .and_then(Value::as_object)
            .and_then(|claim_ref| string_field(claim_ref, &["claim_id", "claimId"]))
    })?;
    let mls_group_id = string_field(
        object,
        &["mls_group_id", "mlsGroupId", "group_id", "groupId"],
    )?;
    let epoch = object.get("epoch").and_then(Value::as_u64)?;
    let realm_id = string_field(object, &["realm_id", "realmId"]).or_else(|| {
        object
            .get("claim_envelope")
            .or_else(|| object.get("claimEnvelope"))
            .and_then(Value::as_object)
            .and_then(|envelope| string_field(envelope, &["intended_realm_id", "intendedRealmId"]))
    });
    let strand_id = string_field(object, &["strand_id", "strandId"]);
    let welcome_ref = string_field(
        object,
        &[
            "welcome_ref",
            "welcomeRef",
            "encrypted_welcome_ref",
            "encryptedWelcomeRef",
        ],
    );

    Some(ArkretMlsWelcomeConsumeBinding {
        keypackage_ref,
        claim_id,
        welcome_ref,
        realm_id,
        strand_id,
        mls_group_id,
        epoch,
    })
}

fn string_field(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
}

fn try_consume_stored_welcome_for_payload(
    state: &ArkretCryptoStateFile,
    store: &mut MemoryCryptoStore,
    payload: &EncryptedPayload,
) -> anyhow::Result<Option<(MlsGroupStateRecord, MlsWelcomeEnvelope)>> {
    let mut candidates = Vec::new();
    for identity_record in state.mls_identities.values() {
        candidates.extend(
            store
                .welcomes_for_device(&identity_record.principal_id, &identity_record.device_id)
                .into_iter()
                .filter(|welcome| {
                    welcome.group_id == payload.group_id && welcome.epoch <= payload.epoch
                })
                .cloned()
                .map(|welcome| (identity_record.clone(), welcome)),
        );
    }
    candidates.sort_by(|(_, left), (_, right)| right.epoch.cmp(&left.epoch));

    let mut last_error = None;
    for (identity_record, welcome) in candidates {
        let identity = match restore_mls_identity(&identity_record) {
            Ok(identity) => identity,
            Err(err) => {
                last_error = Some(err);
                continue;
            }
        };
        match ArkretMlsGroup::join_from_welcome(identity, &welcome) {
            Ok(group) => {
                let record = group
                    .persist_state(store)
                    .map_err(|err| anyhow::anyhow!("persist Arkret MLS group: {err}"))?;
                return Ok(Some((record, welcome)));
            }
            Err(err) => {
                last_error = Some(anyhow::anyhow!("{err}"));
            }
        }
    }

    if let Some(err) = last_error {
        anyhow::bail!("consume Arkret MLS Welcome failed: {err}");
    }
    Ok(None)
}

fn restore_mls_identity(
    record: &ArkretMlsIdentityStateRecord,
) -> anyhow::Result<ArkretMlsIdentity> {
    ArkretMlsIdentity::restore_from_private_state(
        record.principal_id.clone(),
        record.device_id.clone(),
        &record.private_state,
    )
    .map_err(|err| anyhow::anyhow!("restore Arkret MLS identity state: {err}"))
}

fn mls_identity_key(principal_id: &Did, device_id: &DeviceId) -> String {
    format!("{}#{}", principal_id.as_str(), device_id.as_str())
}

fn mls_key_package_cache_key(
    principal_id: &Did,
    device_id: &DeviceId,
    last_resort: bool,
) -> String {
    let kind = if last_resort {
        "last_resort"
    } else {
        "single_use"
    };
    format!("{}#{kind}", mls_identity_key(principal_id, device_id))
}

fn local_key_package_can_be_published(record: &MlsKeyPackageRecord, last_resort: bool) -> bool {
    record.last_resort == last_resort
        && record.is_usable()
        && (record.last_resort || record.state == MlsKeyPackageState::Published)
}

fn find_mls_key_package_cache_key(
    records: &BTreeMap<String, MlsKeyPackageRecord>,
    keypackage_ref_or_id: &str,
) -> Option<String> {
    records
        .iter()
        .find(|(_, record)| {
            record.keypackage_id == keypackage_ref_or_id
                || record.keypackage_ref.as_str() == keypackage_ref_or_id
        })
        .map(|(key, _)| key.clone())
}

fn extract_realm_crypto_policy(
    realm_id: &str,
    realm_value: &Value,
) -> Option<ArkretRealmCryptoPolicy> {
    let candidate = first_policy_candidate(realm_value)?;
    let floor = candidate
        .get("content_encryption_floor")
        .or_else(|| candidate.get("contentEncryptionFloor"))
        .and_then(Value::as_str)
        .and_then(parse_content_encryption_floor)?;
    let encryption_profile = candidate
        .get("encryption_profile")
        .or_else(|| candidate.get("encryptionProfile"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mls_group_id = candidate
        .get("mls_group_id")
        .or_else(|| candidate.get("mlsGroupId"))
        .or_else(|| candidate.get("group_id"))
        .or_else(|| candidate.get("groupId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(ArkretRealmCryptoPolicy {
        realm_id: realm_id.to_owned(),
        content_encryption_floor: floor,
        encryption_profile,
        mls_group_id,
        source: "account_subscribe".to_owned(),
        updated_at: Utc::now(),
    })
}

fn first_policy_candidate(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    let object = value.as_object()?;
    if object.contains_key("content_encryption_floor")
        || object.contains_key("contentEncryptionFloor")
    {
        return Some(object);
    }
    for path in [
        ["realm"].as_slice(),
        ["state", "realm"].as_slice(),
        ["summary", "realm"].as_slice(),
        ["timeline", "realm"].as_slice(),
    ] {
        let mut current = value;
        let mut path_exists = true;
        for segment in path {
            if let Some(next) = current.get(*segment) {
                current = next;
            } else {
                path_exists = false;
                break;
            }
        }
        if !path_exists {
            continue;
        }
        if let Some(object) = current.as_object()
            && (object.contains_key("content_encryption_floor")
                || object.contains_key("contentEncryptionFloor"))
        {
            return Some(object);
        }
    }
    None
}

fn parse_content_encryption_floor(value: &str) -> Option<ArkretContentEncryptionFloor> {
    match value.trim().to_ascii_lowercase().as_str() {
        "allow_plaintext" | "allow-plaintext" | "none" | "plaintext" => {
            Some(ArkretContentEncryptionFloor::AllowPlaintext)
        }
        "e2ee_required" | "e2ee-required" | "required" | "mls_required" | "mls-required" => {
            Some(ArkretContentEncryptionFloor::E2eeRequired)
        }
        _ => None,
    }
}

fn safe_file_stem(scope_id: &str) -> String {
    let mut out = String::with_capacity(scope_id.len());
    for ch in scope_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "default".to_owned()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use arkret::crypto_protocol::UnableToDecryptReason;
    use arkret::{EncryptedPayloadScheme, Hash, KeyOperationSignature, KeyPackageClaimRecord};
    use serde_json::json;

    use super::*;

    fn temp_home(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "savfox-arkret-crypto-{}-{}-{}",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ))
    }

    fn encrypted_payload() -> EncryptedPayload {
        EncryptedPayload {
            scheme: EncryptedPayloadScheme::MlsRfc9420,
            group_id: "group1".to_owned(),
            epoch: 3,
            content_type: CONTENT_BLOCK_JSON.to_owned(),
            ciphertext: "abc".to_owned(),
            aad: None,
            payload_digest: Hash::new(
                "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            )
            .expect("test digest should parse"),
            key_ref: None,
        }
    }

    #[test]
    fn state_file_persists_unable_to_decrypt() {
        let home = temp_home("utd");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        store.ensure_created().expect("create");
        store
            .record_unable_to_decrypt(
                "ak:event:01904100-0000-7000-8000-000000000001",
                "ak:realm:01904100-0000-7000-8000-000000000001",
                "did:webvh:example.org:alice",
                encrypted_payload(),
                UnableToDecryptReason::NoSession,
            )
            .expect("record");
        let state = store.load().expect("load");
        assert_eq!(state.binding.unable_to_decrypt.len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn extracts_encrypted_payload_carriers() {
        let payload = serde_json::to_value(encrypted_payload()).expect("payload should serialize");
        let content = json!({
            "message_id": "ak:message:1",
            "encrypted_content": payload,
        });
        assert!(message_content_has_encrypted_carrier(&content));
        let parsed = extract_encrypted_payload_from_message_content(&content).expect("payload");
        assert_eq!(parsed.group_id, "group1");
    }

    #[test]
    fn sync_realm_policy_is_persisted() {
        let home = temp_home("policy");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        let realms = json!({
            "ak:realm:01904100-0000-7000-8000-000000000001": {
                "state": {
                    "realm": {
                        "content_encryption_floor": "e2ee_required",
                        "encryption_profile": "mls",
                        "mls_group_id": "group1"
                    }
                }
            }
        });
        assert_eq!(
            store
                .update_realm_policies_from_sync(&realms)
                .expect("policy sync should persist"),
            1
        );
        let state = store.load().expect("state should load");
        let policy = state
            .realm_policies
            .get("ak:realm:01904100-0000-7000-8000-000000000001")
            .expect("policy should be present");
        assert!(policy.requires_e2ee());
        assert_eq!(policy.group_id_for_realm(), "group1");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn missing_required_group_state_blocks_encryption() {
        let home = temp_home("encrypt-missing");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        store
            .upsert_realm_policy(ArkretRealmCryptoPolicy {
                realm_id: "ak:realm:01904100-0000-7000-8000-000000000001".to_owned(),
                content_encryption_floor: ArkretContentEncryptionFloor::E2eeRequired,
                encryption_profile: Some("mls".to_owned()),
                mls_group_id: Some("group1".to_owned()),
                source: "test".to_owned(),
                updated_at: Utc::now(),
            })
            .expect("policy should persist");
        let outcome = store
            .encrypt_content_block_for_realm(
                "ak:realm:01904100-0000-7000-8000-000000000001",
                &json!({"kind":"ak.content.text","body":"secret"}),
            )
            .expect("encryption decision should complete");
        assert_eq!(
            outcome,
            ArkretEncryptOutcome::MissingRequiredGroupState {
                realm_id: "ak:realm:01904100-0000-7000-8000-000000000001".to_owned(),
                group_id: "group1".to_owned()
            }
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn bootstrap_plan_marks_key_backup_restore_needed() {
        let home = temp_home("bootstrap");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        let record = store
            .plan_bootstrap_for_payload(
                "did:webvh:example.org:alice",
                "ak:device:01904100-0000-7000-8000-000000000001",
                &encrypted_payload(),
            )
            .expect("bootstrap plan should persist");
        assert!(matches!(
            record.action,
            MlsRecoveryAction::RequestEpochRecovery { .. }
        ));
        let state = store.load().expect("state should load");
        assert!(state.key_backup.restore_needed);
        assert_eq!(
            state.key_backup.last_needed_for_group_id.as_deref(),
            Some("group1")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn single_use_key_package_claim_rotates_local_cache() {
        let home = temp_home("kp-single-use");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "bob");
        let principal = "did:webvh:z6mkfixture:bob.example";
        let device = "ak:device:01904100-0000-7000-8000-00000000000e";
        let first = store
            .ensure_mls_key_package(principal, device, false)
            .expect("single-use KeyPackage should be created");

        let claimed = store
            .mark_mls_key_package_claimed(first.keypackage_ref.as_str(), "ak:claim:test")
            .expect("claim marker should persist")
            .expect("KeyPackage should be found");
        assert_eq!(claimed.state, MlsKeyPackageState::Claimed);
        assert_eq!(claimed.claim_id.as_deref(), Some("ak:claim:test"));

        let rotated = store
            .ensure_mls_key_package(principal, device, false)
            .expect("claimed single-use KeyPackage should rotate");
        assert_ne!(rotated.keypackage_id, first.keypackage_id);
        assert_eq!(rotated.state, MlsKeyPackageState::Published);
        assert!(!rotated.last_resort);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn last_resort_key_package_survives_claim_and_consume_markers() {
        let home = temp_home("kp-last-resort");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "bob");
        let principal = "did:webvh:z6mkfixture:bob.example";
        let device = "ak:device:01904100-0000-7000-8000-00000000000e";
        let single_use = store
            .ensure_mls_key_package(principal, device, false)
            .expect("single-use KeyPackage should be created");
        let last_resort = store
            .ensure_mls_key_package(principal, device, true)
            .expect("last-resort KeyPackage should be created");
        assert_ne!(single_use.keypackage_id, last_resort.keypackage_id);

        let claimed = store
            .mark_mls_key_package_claimed(last_resort.keypackage_id.as_str(), "ak:claim:last")
            .expect("claim marker should persist")
            .expect("last-resort KeyPackage should be found");
        assert_eq!(claimed.state, MlsKeyPackageState::Claimed);
        assert!(claimed.last_resort);

        let cached = store
            .ensure_mls_key_package(principal, device, true)
            .expect("claimed last-resort KeyPackage should stay reusable");
        assert_eq!(cached.keypackage_id, last_resort.keypackage_id);

        let consumed = store
            .mark_mls_key_package_consumed(last_resort.keypackage_ref.as_str())
            .expect("consume marker should persist")
            .expect("last-resort KeyPackage should be found");
        assert_eq!(consumed.state, MlsKeyPackageState::Claimed);
        assert!(consumed.last_resort);

        let cached_after_consume = store
            .ensure_mls_key_package(principal, device, true)
            .expect("last-resort KeyPackage should stay reusable after consume ack");
        assert_eq!(
            cached_after_consume.keypackage_id,
            last_resort.keypackage_id
        );
        let state = store.load().expect("state should load");
        assert_eq!(state.mls_key_packages.len(), 2);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn claimed_key_package_record_can_feed_group_add_member() {
        let home = temp_home("kp-claim-record");
        let bob_store = FileArkretCryptoStore::for_account(&home, "c1", "bob");
        let bob_principal = "did:webvh:z6mkfixture:bob.example";
        let bob_device = "ak:device:01904100-0000-7000-8000-00000000000e";
        let bob_key_package = bob_store
            .ensure_mls_key_package(bob_principal, bob_device, false)
            .expect("Bob KeyPackage should be created");
        let claim = KeyPackageClaimRecord {
            claim_id: "ak:claim:test-claim-record".to_owned(),
            keypackage_ref: "ak:mls:keypackage:test-claim-record".to_owned(),
            keypackage_digest: bob_key_package.keypackage_ref.clone(),
            principal_id: Did::new(bob_principal.to_owned()).unwrap(),
            device_id: bob_device.to_owned(),
            key_package: bob_key_package.key_package.clone(),
            capabilities: bob_key_package.capabilities.clone(),
            capabilities_digest: Hash::new(format!("sha256:{}", "bb".repeat(32))).unwrap(),
            ssk_generation: None,
            device_authorize_event_id: None,
            expires_at: Utc::now() + chrono::Duration::days(1),
            device_signature: KeyOperationSignature {
                kid: arkret::NonEmptyString::new(format!("{bob_principal}#runtime-1")).unwrap(),
                alg: Some(arkret::NonEmptyString::new("Ed25519").unwrap()),
                sig: arkret::Base64UrlString::new("c2ln").unwrap(),
            },
            revocation_status: Some("active".to_owned()),
            last_resort: Some(false),
        };

        let claimed = mls_key_package_record_from_claim(&claim)
            .expect("claim record should project to local MLS record");
        assert_eq!(claimed.state, MlsKeyPackageState::Claimed);
        assert_eq!(claimed.claim_id.as_deref(), Some(claim.claim_id.as_str()));
        assert_eq!(claimed.keypackage_ref, bob_key_package.keypackage_ref);

        let alice = ArkretMlsIdentity::new_basic(
            Did::new("did:webvh:z6mkfixture:alice.example".to_owned()).unwrap(),
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006".to_owned()).unwrap(),
        )
        .unwrap();
        let mut alice_group = alice
            .create_group(b"ak:realm:01904100-0000-7000-8000-f7claim00001")
            .unwrap();
        let add = alice_group
            .add_member(&claimed)
            .expect("claimed KeyPackage should add to MLS group");
        assert_eq!(add.welcome.recipient_principal_id.as_str(), bob_principal);
        assert_eq!(add.welcome.recipient_device_id.as_str(), bob_device);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn stored_welcome_admits_group_and_decrypts_content_block() {
        let home = temp_home("welcome-admit");
        let bob_store = FileArkretCryptoStore::for_account(&home, "c1", "bob");
        let bob_principal = "did:webvh:z6mkfixture:bob.example";
        let bob_device = "ak:device:01904100-0000-7000-8000-00000000000e";
        let bob_key_package = bob_store
            .ensure_mls_key_package(bob_principal, bob_device, false)
            .expect("Bob KeyPackage should be stored with private identity state");
        let state = bob_store.load().expect("state should load");
        assert_eq!(state.mls_identities.len(), 1);
        assert_eq!(state.mls_key_packages.len(), 1);

        let alice = ArkretMlsIdentity::new_basic(
            Did::new("did:webvh:z6mkfixture:alice.example").unwrap(),
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006").unwrap(),
        )
        .unwrap();
        let mut alice_group = alice
            .create_group(b"ak:realm:01904100-0000-7000-8000-f7admission")
            .unwrap();
        let add = alice_group.add_member(&bob_key_package).unwrap();
        let expected_binding = ArkretMlsWelcomeConsumeBinding {
            keypackage_ref: bob_key_package.keypackage_ref.as_str().to_owned(),
            claim_id: "ak:claim:test-welcome".to_owned(),
            welcome_ref: Some("ak:welcome:test".to_owned()),
            realm_id: Some("ak:realm:01904100-0000-7000-8000-000000000001".to_owned()),
            strand_id: None,
            mls_group_id: add.welcome.group_id.clone(),
            epoch: add.welcome.epoch,
        };
        let welcome_carrier = json!({
            "keypackage_ref": expected_binding.keypackage_ref.as_str(),
            "claim_ref": {
                "claim_id": expected_binding.claim_id.as_str()
            },
            "claim_envelope": {
                "intended_realm_id": expected_binding.realm_id.as_deref().unwrap()
            },
            "welcome_ref": expected_binding.welcome_ref.as_deref().unwrap(),
            "mls_group_id": expected_binding.mls_group_id.as_str(),
            "epoch": expected_binding.epoch,
            "content": serde_json::to_value(&add.welcome).unwrap()
        });
        let recorded = bob_store
            .record_mls_welcome_from_value(&welcome_carrier)
            .expect("welcome carrier should persist")
            .expect("welcome should be extracted");
        assert_eq!(recorded.group_id, add.welcome.group_id);
        let state = bob_store.load().expect("state should load");
        assert_eq!(
            state
                .mls_welcome_consume_bindings
                .get(&expected_binding.cache_key()),
            Some(&expected_binding)
        );
        let claimed_package = state
            .mls_key_packages
            .values()
            .find(|record| record.keypackage_ref.as_str() == expected_binding.keypackage_ref)
            .expect("claimed KeyPackage should remain cached");
        assert_eq!(claimed_package.state, MlsKeyPackageState::Claimed);
        assert_eq!(
            claimed_package.claim_id.as_deref(),
            Some(expected_binding.claim_id.as_str())
        );

        let content = json!({"kind":"ak.content.text","body":"secret"});
        let payload = alice_group
            .encrypt_payload(CONTENT_BLOCK_JSON, &serde_json::to_vec(&content).unwrap())
            .unwrap();
        let outcome = bob_store
            .try_decrypt_content_block_detailed(&payload)
            .expect("stored Welcome should admit Bob before decrypt");
        let ArkretDecryptDetailedOutcome::Decrypted {
            content: decrypted,
            consume_bindings,
        } = outcome
        else {
            panic!("stored Welcome should decrypt");
        };
        assert_eq!(decrypted, content);
        assert_eq!(consume_bindings, vec![expected_binding.clone()]);

        bob_store
            .mark_mls_welcome_consume_binding_acked(&expected_binding)
            .expect("consume binding ack should persist");
        let state = bob_store.load().expect("state should load");
        assert!(state.mls_welcome_consume_bindings.is_empty());

        let state = bob_store.load().expect("state should load");
        let store = state.mls_store().expect("MLS store should load");
        assert!(store.mls_group_state(&payload.group_id).is_some());

        let content_after_restart = json!({"kind":"ak.content.text","body":"after restart"});
        let payload_after_restart = alice_group
            .encrypt_payload(
                CONTENT_BLOCK_JSON,
                &serde_json::to_vec(&content_after_restart).unwrap(),
            )
            .unwrap();
        let reloaded_bob_store = FileArkretCryptoStore::for_account(&home, "c1", "bob");
        let outcome_after_restart = reloaded_bob_store
            .try_decrypt_content_block(&payload_after_restart)
            .expect("persisted group state should decrypt after restart");
        assert_eq!(
            outcome_after_restart,
            ArkretDecryptOutcome::Decrypted(content_after_restart)
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
