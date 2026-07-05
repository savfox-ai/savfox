//! Local Cokret crypto state for Savfox channel adapters.
//!
//! The SDK owns the protocol objects (`CryptoStoreBinding`, `MemoryCryptoStore`
//! and `CokretMlsGroup`). This module gives Savfox a small file-backed wrapper
//! so account-mode and applet-mode can persist decryption failures, MLS group
//! snapshots, recovery plans and realm encryption policy under `SAVFOX_HOME`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, Utc};
use cokret::crypto_protocol::{CryptoStoreBinding, UnableToDecryptReason, UnableToDecryptRecord};
use cokret::{
    CokretMlsGroup, CryptoStore, DeviceId, Did, EncryptedPayload, EncryptedPayloadScheme, EventId,
    FeatureSafetyReport, MemoryCryptoStore, MlsRecoveryAction, RealmId,
    current_feature_safety_report,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const STATE_VERSION: &str = "savfox.cokret.crypto_state.v1";
const CONTENT_BLOCK_JSON: &str = "application/vnd.cokret.content-block+json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CokretContentEncryptionFloor {
    AllowPlaintext,
    E2eeRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CokretRealmCryptoPolicy {
    pub realm_id: String,
    pub content_encryption_floor: CokretContentEncryptionFloor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mls_group_id: Option<String>,
    pub source: String,
    pub updated_at: DateTime<Utc>,
}

impl CokretRealmCryptoPolicy {
    #[must_use]
    pub fn requires_e2ee(&self) -> bool {
        self.content_encryption_floor == CokretContentEncryptionFloor::E2eeRequired
    }

    #[must_use]
    pub fn group_id_for_realm(&self) -> &str {
        self.mls_group_id
            .as_deref()
            .unwrap_or(self.realm_id.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CokretBootstrapRecord {
    pub group_id: String,
    pub required_epoch: u64,
    pub local_epoch: Option<u64>,
    pub action: MlsRecoveryAction,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CokretKeyBackupState {
    pub restore_needed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_needed_for_group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_needed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub restored_secret_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CokretCryptoStateFile {
    pub version: String,
    pub scope_id: String,
    pub sdk_features: Vec<String>,
    pub binding: CryptoStoreBinding,
    pub mls_store_json: String,
    #[serde(default)]
    pub realm_policies: BTreeMap<String, CokretRealmCryptoPolicy>,
    #[serde(default)]
    pub bootstrap: BTreeMap<String, CokretBootstrapRecord>,
    #[serde(default)]
    pub key_backup: CokretKeyBackupState,
}

impl CokretCryptoStateFile {
    fn new(scope_id: String) -> anyhow::Result<Self> {
        let store = MemoryCryptoStore::new();
        Ok(Self {
            version: STATE_VERSION.to_owned(),
            scope_id,
            sdk_features: current_feature_safety_report().enabled_features,
            binding: CryptoStoreBinding::default(),
            mls_store_json: store
                .export_backup_json()
                .map_err(|err| anyhow::anyhow!("cokret crypto store export: {err}"))?,
            realm_policies: BTreeMap::new(),
            bootstrap: BTreeMap::new(),
            key_backup: CokretKeyBackupState::default(),
        })
    }

    fn mls_store(&self) -> anyhow::Result<MemoryCryptoStore> {
        let mut store = MemoryCryptoStore::new();
        store
            .import_backup_json(&self.mls_store_json)
            .map_err(|err| anyhow::anyhow!("cokret crypto store import: {err}"))?;
        Ok(store)
    }

    fn set_mls_store(&mut self, store: &MemoryCryptoStore) -> anyhow::Result<()> {
        self.mls_store_json = store
            .export_backup_json()
            .map_err(|err| anyhow::anyhow!("cokret crypto store export: {err}"))?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct FileCokretCryptoStore {
    path: PathBuf,
    scope_id: String,
}

impl FileCokretCryptoStore {
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
            .join("cokret-crypto")
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
            .map_err(|err| anyhow::anyhow!("cokret crypto feature set is unsafe: {err}"))?;
        Ok(report)
    }

    pub fn load(&self) -> anyhow::Result<CokretCryptoStateFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.is_empty() => CokretCryptoStateFile::new(self.scope_id.clone()),
            Ok(bytes) => {
                let mut state: CokretCryptoStateFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {}", self.path.display()))?;
                if state.version != STATE_VERSION {
                    anyhow::bail!(
                        "unsupported Cokret crypto state version '{}' in {}",
                        state.version,
                        self.path.display()
                    );
                }
                if state.scope_id != self.scope_id {
                    anyhow::bail!(
                        "Cokret crypto state scope mismatch: expected '{}', got '{}'",
                        self.scope_id,
                        state.scope_id
                    );
                }
                state.sdk_features = current_feature_safety_report().enabled_features;
                Ok(state)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                CokretCryptoStateFile::new(self.scope_id.clone())
            }
            Err(err) => Err(err).with_context(|| format!("read {}", self.path.display())),
        }
    }

    pub fn save(&self, state: &CokretCryptoStateFile) -> anyhow::Result<()> {
        if state.scope_id != self.scope_id {
            anyhow::bail!(
                "refusing to save Cokret crypto state for scope '{}' into '{}'",
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

    pub fn upsert_realm_policy(&self, policy: CokretRealmCryptoPolicy) -> anyhow::Result<()> {
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

    pub fn plan_bootstrap_for_payload(
        &self,
        principal_id: &str,
        device_id: &str,
        payload: &EncryptedPayload,
    ) -> anyhow::Result<CokretBootstrapRecord> {
        let mut state = self.load()?;
        let store = state.mls_store()?;
        let principal = Did::new(principal_id.to_owned())
            .with_context(|| format!("invalid Cokret principal DID '{principal_id}'"))?;
        let device = DeviceId::new(device_id.to_owned())
            .with_context(|| format!("invalid Cokret device id '{device_id}'"))?;
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
        let record = CokretBootstrapRecord {
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
                .with_context(|| format!("invalid Cokret event id '{event_id}'"))?,
            realm_id: RealmId::new(realm_id.to_owned())
                .with_context(|| format!("invalid Cokret realm id '{realm_id}'"))?,
            sender: Did::new(sender.to_owned())
                .with_context(|| format!("invalid Cokret sender DID '{sender}'"))?,
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
    ) -> anyhow::Result<CokretDecryptOutcome> {
        let mut state = self.load()?;
        let mut store = state.mls_store()?;
        let Some(record) = store.mls_group_state(&payload.group_id).cloned() else {
            return Ok(CokretDecryptOutcome::MissingGroupState);
        };
        let mut group = CokretMlsGroup::restore_from_state_record(&record)
            .map_err(|err| anyhow::anyhow!("restore Cokret MLS group: {err}"))?;
        let plaintext = match payload.scheme {
            EncryptedPayloadScheme::MlsRfc9420 => group
                .decrypt_payload(payload)
                .map_err(|err| anyhow::anyhow!("decrypt Cokret MLS payload: {err}"))?,
            EncryptedPayloadScheme::MlsExporterAeadV1 => {
                return Ok(CokretDecryptOutcome::UnsupportedScheme(
                    payload.scheme.as_str().to_owned(),
                ));
            }
        };
        let content = serde_json::from_slice(&plaintext)
            .with_context(|| "decrypted Cokret content block is not JSON")?;
        let updated = group
            .persist_state(&mut store)
            .map_err(|err| anyhow::anyhow!("persist Cokret MLS group: {err}"))?;
        state.set_mls_store(&store)?;
        state.bootstrap.insert(
            updated.group_id.clone(),
            CokretBootstrapRecord {
                group_id: updated.group_id,
                required_epoch: updated.epoch,
                local_epoch: Some(updated.epoch),
                action: MlsRecoveryAction::UseLocalState,
                updated_at: Utc::now(),
            },
        );
        self.save(&state)?;
        Ok(CokretDecryptOutcome::Decrypted(content))
    }

    pub fn encrypt_content_block_for_realm(
        &self,
        realm_id: &str,
        content: &Value,
    ) -> anyhow::Result<CokretEncryptOutcome> {
        let mut state = self.load()?;
        let Some(policy) = state.realm_policies.get(realm_id).cloned() else {
            return Ok(CokretEncryptOutcome::PlaintextAllowed);
        };
        if !policy.requires_e2ee() {
            return Ok(CokretEncryptOutcome::PlaintextAllowed);
        }
        let mut store = state.mls_store()?;
        let group_id = policy.group_id_for_realm().to_owned();
        let Some(record) = store.mls_group_state(&group_id).cloned() else {
            return Ok(CokretEncryptOutcome::MissingRequiredGroupState {
                group_id,
                realm_id: realm_id.to_owned(),
            });
        };
        let mut group = CokretMlsGroup::restore_from_state_record(&record)
            .map_err(|err| anyhow::anyhow!("restore Cokret MLS group: {err}"))?;
        let plaintext = serde_json::to_vec(content)?;
        let payload = group
            .encrypt_payload(CONTENT_BLOCK_JSON, &plaintext)
            .map_err(|err| anyhow::anyhow!("encrypt Cokret MLS payload: {err}"))?;
        group
            .persist_state(&mut store)
            .map_err(|err| anyhow::anyhow!("persist Cokret MLS group: {err}"))?;
        state.set_mls_store(&store)?;
        self.save(&state)?;
        Ok(CokretEncryptOutcome::Encrypted(serde_json::to_value(
            payload,
        )?))
    }

    fn tmp_path(&self) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "cokret-crypto.json".into());
        name.push(".tmp");
        self.path.with_file_name(name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CokretDecryptOutcome {
    Decrypted(Value),
    MissingGroupState,
    UnsupportedScheme(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CokretEncryptOutcome {
    PlaintextAllowed,
    Encrypted(Value),
    MissingRequiredGroupState { realm_id: String, group_id: String },
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
        .is_some_and(|kind| kind == "ck.content.encrypted")
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
            .is_some_and(|kind| kind == "ck.content.encrypted")
}

fn extract_realm_crypto_policy(
    realm_id: &str,
    realm_value: &Value,
) -> Option<CokretRealmCryptoPolicy> {
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
    Some(CokretRealmCryptoPolicy {
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

fn parse_content_encryption_floor(value: &str) -> Option<CokretContentEncryptionFloor> {
    match value.trim().to_ascii_lowercase().as_str() {
        "allow_plaintext" | "allow-plaintext" | "none" | "plaintext" => {
            Some(CokretContentEncryptionFloor::AllowPlaintext)
        }
        "e2ee_required" | "e2ee-required" | "required" | "mls_required" | "mls-required" => {
            Some(CokretContentEncryptionFloor::E2eeRequired)
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
    use cokret::crypto_protocol::UnableToDecryptReason;
    use cokret::{EncryptedPayloadScheme, Hash};
    use serde_json::json;

    use super::*;

    fn temp_home(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "savfox-cokret-crypto-{}-{}-{}",
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
        let store = FileCokretCryptoStore::for_account(&home, "c1", "a1");
        store.ensure_created().expect("create");
        store
            .record_unable_to_decrypt(
                "ck:event:01904100-0000-7000-8000-000000000001",
                "ck:realm:01904100-0000-7000-8000-000000000001",
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
            "message_id": "ck:message:1",
            "encrypted_content": payload,
        });
        assert!(message_content_has_encrypted_carrier(&content));
        let parsed = extract_encrypted_payload_from_message_content(&content).expect("payload");
        assert_eq!(parsed.group_id, "group1");
    }

    #[test]
    fn sync_realm_policy_is_persisted() {
        let home = temp_home("policy");
        let store = FileCokretCryptoStore::for_account(&home, "c1", "a1");
        let realms = json!({
            "ck:realm:01904100-0000-7000-8000-000000000001": {
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
            .get("ck:realm:01904100-0000-7000-8000-000000000001")
            .expect("policy should be present");
        assert!(policy.requires_e2ee());
        assert_eq!(policy.group_id_for_realm(), "group1");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn missing_required_group_state_blocks_encryption() {
        let home = temp_home("encrypt-missing");
        let store = FileCokretCryptoStore::for_account(&home, "c1", "a1");
        store
            .upsert_realm_policy(CokretRealmCryptoPolicy {
                realm_id: "ck:realm:01904100-0000-7000-8000-000000000001".to_owned(),
                content_encryption_floor: CokretContentEncryptionFloor::E2eeRequired,
                encryption_profile: Some("mls".to_owned()),
                mls_group_id: Some("group1".to_owned()),
                source: "test".to_owned(),
                updated_at: Utc::now(),
            })
            .expect("policy should persist");
        let outcome = store
            .encrypt_content_block_for_realm(
                "ck:realm:01904100-0000-7000-8000-000000000001",
                &json!({"kind":"ck.content.text","body":"secret"}),
            )
            .expect("encryption decision should complete");
        assert_eq!(
            outcome,
            CokretEncryptOutcome::MissingRequiredGroupState {
                realm_id: "ck:realm:01904100-0000-7000-8000-000000000001".to_owned(),
                group_id: "group1".to_owned()
            }
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn bootstrap_plan_marks_key_backup_restore_needed() {
        let home = temp_home("bootstrap");
        let store = FileCokretCryptoStore::for_account(&home, "c1", "a1");
        let record = store
            .plan_bootstrap_for_payload(
                "did:webvh:example.org:alice",
                "ck:device:01904100-0000-7000-8000-000000000001",
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
}
