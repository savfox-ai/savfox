//! Local Arkret crypto state for Savfox channel adapters.
//!
//! The SDK owns the protocol objects (`CryptoStoreBinding`, `MemoryCryptoStore`
//! and `ArkretMlsGroup`). This module gives Savfox a small file-backed wrapper
//! so account-mode and applet-mode can persist decryption failures, MLS group
//! snapshots, recovery plans and realm encryption policy under `SAVFOX_HOME`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context;
use arkret::mls::{ArkretMlsGroup, ArkretMlsIdentity};
use arkret::{
    ContentBlock, DeviceId, Did, DirectConversationBoundPayload, EncryptedPayload,
    EncryptedPayloadScheme, EventId, MessageMetadata, MlsCommitPayload, MlsEncryptedPayload,
    MlsKeyPackageRecord, MlsKeyPackageState, MlsPayloadType, MlsWelcomeEnvelope, MlsWelcomePayload,
    PresencePlaintext, PresenceState, RealmId, ScopeRef, SealId, StrandCreatePayload,
    seal_signal_plaintext,
};
use arkret_crypto::{CryptoStoreBinding, UnableToDecryptReason, UnableToDecryptRecord};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{DateTime, Utc};
use garth::{CryptoStore, MemoryCryptoStore, MlsGroupStateRecord, MlsRecoveryAction};
use parking_lot::ReentrantMutex;
use savfox_keyring_store::KeyringStore as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::signer::{ArkretKeyRef, load_ed25519_signing_key};

const STATE_VERSION: &str = "savfox.arkret.crypto_state.v1";
const WRAPPED_STATE_VERSION: &str = "savfox.arkret.crypto_state.wrapped.v1";
const WRAPPING_KEY_SERVICE: &str = "savfox-arkret-crypto-state";
#[cfg(test)]
const CONTENT_BLOCK_JSON: &str = arkret::MESSAGE_CONTENT_BLOCK_MLS_CONTENT_TYPE;
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
    pub fn group_id_for_realm(&self) -> Cow<'_, str> {
        if let Some(group_id) = self.mls_group_id.as_deref() {
            return Cow::Borrowed(group_id);
        }
        if self.source == "account_subscribe_direct_conversation" {
            return Cow::Owned(URL_SAFE_NO_PAD.encode(self.realm_id.trim().as_bytes()));
        }
        Cow::Borrowed(self.realm_id.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArkretBootstrapRecord {
    pub group_id: String,
    pub required_epoch: u64,
    pub local_epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_state_ref: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_state_ref: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArkretDirectConversationWelcomeBinding {
    pub welcome_ref: String,
    pub realm_id: String,
    pub strand_id: String,
    pub mls_group_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArkretCryptoStateFile {
    pub version: String,
    pub scope_id: String,
    #[serde(default)]
    pub generation: u64,
    pub binding: CryptoStoreBinding,
    pub mls_store_json: String,
    #[serde(default)]
    pub mls_identities: BTreeMap<String, ArkretMlsIdentityStateRecord>,
    #[serde(default)]
    pub mls_key_packages: BTreeMap<String, MlsKeyPackageRecord>,
    #[serde(default)]
    pub mls_welcome_consume_bindings: BTreeMap<String, ArkretMlsWelcomeConsumeBinding>,
    #[serde(default)]
    pub direct_conversation_welcome_bindings:
        BTreeMap<String, ArkretDirectConversationWelcomeBinding>,
    #[serde(default)]
    pub realm_policies: BTreeMap<String, ArkretRealmCryptoPolicy>,
    #[serde(default)]
    pub bootstrap: BTreeMap<String, ArkretBootstrapRecord>,
    /// Next sender-device sequence per Signal scope. The value is advanced and
    /// persisted before each submit so a failed HTTP request can skip but never
    /// reuse a sequence or the MLS Signal nonce consumed alongside it.
    #[serde(default)]
    pub signal_sequences: BTreeMap<String, u64>,
    #[serde(default)]
    pub key_backup: ArkretKeyBackupState,
}

impl ArkretCryptoStateFile {
    fn new(scope_id: String) -> anyhow::Result<Self> {
        let store = MemoryCryptoStore::new();
        Ok(Self {
            version: STATE_VERSION.to_owned(),
            scope_id,
            generation: 0,
            binding: CryptoStoreBinding::default(),
            mls_store_json: store
                .export_backup_json()
                .map_err(|err| anyhow::anyhow!("arkret crypto store export: {err}"))?,
            mls_identities: BTreeMap::new(),
            mls_key_packages: BTreeMap::new(),
            mls_welcome_consume_bindings: BTreeMap::new(),
            direct_conversation_welcome_bindings: BTreeMap::new(),
            realm_policies: BTreeMap::new(),
            bootstrap: BTreeMap::new(),
            signal_sequences: BTreeMap::new(),
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
    mutation_lock: Arc<ReentrantMutex<()>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WrappedCryptoStateFile {
    version: String,
    nonce: String,
    ciphertext: String,
}

fn crypto_scope_lock(scope_id: &str) -> Arc<ReentrantMutex<()>> {
    static LOCKS: OnceLock<Mutex<BTreeMap<String, Arc<ReentrantMutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut locks = locks.lock().expect("Arkret crypto scope lock registry");
    locks
        .entry(scope_id.to_owned())
        .or_insert_with(|| Arc::new(ReentrantMutex::new(())))
        .clone()
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
        let mutation_lock = crypto_scope_lock(&scope_id);
        Self {
            path,
            scope_id,
            mutation_lock,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> anyhow::Result<ArkretCryptoStateFile> {
        let _guard = self.mutation_lock.lock();
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> anyhow::Result<ArkretCryptoStateFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.is_empty() => ArkretCryptoStateFile::new(self.scope_id.clone()),
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {}", self.path.display()))?;
                let state: ArkretCryptoStateFile = if value.get("version").and_then(Value::as_str)
                    == Some(WRAPPED_STATE_VERSION)
                {
                    let wrapped: WrappedCryptoStateFile = serde_json::from_value(value)?;
                    let mut plaintext = self.decrypt_wrapped_state(&wrapped)?;
                    let decoded = serde_json::from_slice(&plaintext)
                        .with_context(|| format!("decrypt {}", self.path.display()));
                    use zeroize::Zeroize as _;
                    plaintext.zeroize();
                    decoded?
                } else {
                    // One-time migration for legacy plaintext state. The next
                    // successful mutation rewrites it as a wrapped envelope.
                    serde_json::from_value(value)
                        .with_context(|| format!("parse legacy {}", self.path.display()))?
                };
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
                Ok(state)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                ArkretCryptoStateFile::new(self.scope_id.clone())
            }
            Err(err) => Err(err).with_context(|| format!("read {}", self.path.display())),
        }
    }

    pub fn save(&self, state: &mut ArkretCryptoStateFile) -> anyhow::Result<()> {
        let _guard = self.mutation_lock.lock();
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
        let current_generation = self.load_unlocked()?.generation;
        anyhow::ensure!(
            current_generation == state.generation,
            "Arkret crypto state generation conflict for scope '{}': disk={}, caller={}",
            self.scope_id,
            current_generation,
            state.generation
        );
        let next_generation = state.generation.saturating_add(1);
        let mut next_state = state.clone();
        next_state.generation = next_generation;
        let mut plaintext = serde_json::to_vec(&next_state)?;
        let wrapped = self.encrypt_wrapped_state(&plaintext)?;
        use zeroize::Zeroize as _;
        plaintext.zeroize();
        let bytes = serde_json::to_vec_pretty(&wrapped)?;
        savfox_utils::fs::write_atomically(&self.path, &bytes, Some(0o600))
            .with_context(|| format!("persist {}", self.path.display()))?;
        state.generation = next_generation;
        Ok(())
    }

    fn wrapping_key_account(&self) -> String {
        use sha2::Digest as _;
        format!(
            "scope-{}",
            hex::encode(sha2::Sha256::digest(self.scope_id.as_bytes()))
        )
    }

    fn wrapping_key(&self) -> anyhow::Result<[u8; 32]> {
        let account = self.wrapping_key_account();
        let store = savfox_keyring_store::DefaultKeyringStore;
        if let Some(encoded) = store
            .load(WRAPPING_KEY_SERVICE, &account)
            .context("load Arkret crypto-state wrapping key from platform credential vault")?
        {
            let decoded = URL_SAFE_NO_PAD
                .decode(encoded)
                .context("decode Arkret crypto-state wrapping key")?;
            return decoded.try_into().map_err(|value: Vec<u8>| {
                anyhow::anyhow!(
                    "Arkret crypto-state wrapping key has invalid length {}",
                    value.len()
                )
            });
        }
        let key = rand::random::<[u8; 32]>();
        store
            .save(WRAPPING_KEY_SERVICE, &account, &URL_SAFE_NO_PAD.encode(key))
            .context("save Arkret crypto-state wrapping key in platform credential vault")?;
        Ok(key)
    }

    fn wrapping_aad(&self) -> Vec<u8> {
        format!("{WRAPPED_STATE_VERSION}\0{}", self.scope_id).into_bytes()
    }

    fn encrypt_wrapped_state(&self, plaintext: &[u8]) -> anyhow::Result<WrappedCryptoStateFile> {
        let mut key = self.wrapping_key()?;
        let nonce_bytes = rand::random::<[u8; 24]>();
        let cipher = XChaCha20Poly1305::new_from_slice(&key)
            .map_err(|_| anyhow::anyhow!("initialize Arkret crypto-state wrapper"))?;
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce_bytes),
                Payload {
                    msg: plaintext,
                    aad: &self.wrapping_aad(),
                },
            )
            .map_err(|_| anyhow::anyhow!("wrap Arkret crypto state"))?;
        use zeroize::Zeroize as _;
        key.zeroize();
        Ok(WrappedCryptoStateFile {
            version: WRAPPED_STATE_VERSION.to_owned(),
            nonce: URL_SAFE_NO_PAD.encode(nonce_bytes),
            ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
        })
    }

    fn decrypt_wrapped_state(&self, wrapped: &WrappedCryptoStateFile) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(
            wrapped.version == WRAPPED_STATE_VERSION,
            "unsupported wrapped Arkret crypto state version '{}'",
            wrapped.version
        );
        let nonce = URL_SAFE_NO_PAD
            .decode(&wrapped.nonce)
            .context("decode Arkret crypto-state nonce")?;
        anyhow::ensure!(nonce.len() == 24, "invalid Arkret crypto-state nonce");
        let ciphertext = URL_SAFE_NO_PAD
            .decode(&wrapped.ciphertext)
            .context("decode Arkret crypto-state ciphertext")?;
        let mut key = self.wrapping_key()?;
        let cipher = XChaCha20Poly1305::new_from_slice(&key)
            .map_err(|_| anyhow::anyhow!("initialize Arkret crypto-state wrapper"))?;
        let plaintext = cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &self.wrapping_aad(),
                },
            )
            .map_err(|_| anyhow::anyhow!("unwrap Arkret crypto state"));
        use zeroize::Zeroize as _;
        key.zeroize();
        plaintext
    }

    pub fn ensure_created(&self) -> anyhow::Result<()> {
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        self.save(&mut state)
    }

    pub fn upsert_realm_policy(&self, policy: ArkretRealmCryptoPolicy) -> anyhow::Result<()> {
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        state.realm_policies.insert(policy.realm_id.clone(), policy);
        self.save(&mut state)
    }

    pub fn realm_requires_e2ee(&self, realm_id: &str) -> anyhow::Result<bool> {
        Ok(self
            .load()?
            .realm_policies
            .get(realm_id)
            .is_some_and(ArkretRealmCryptoPolicy::requires_e2ee))
    }

    /// Realm ids for which the account has both an E2EE policy and mutable MLS
    /// state accepted at a known group-state reference. Only these scopes can
    /// safely advertise encrypted v1 presence.
    pub fn presence_ready_realm_ids(&self) -> anyhow::Result<Vec<String>> {
        let state = self.load()?;
        let store = state.mls_store()?;
        let mut realms = state
            .realm_policies
            .values()
            .filter(|policy| policy.requires_e2ee())
            .filter_map(|policy| {
                let group_id = policy.group_id_for_realm();
                let record = store.mls_group_state(group_id.as_ref())?;
                group_state_ref_for_epoch(&state, group_id.as_ref(), record.epoch)?;
                Some(policy.realm_id.clone())
            })
            .collect::<Vec<_>>();
        realms.sort();
        realms.dedup();
        Ok(realms)
    }

    /// Seal and sign one Realm-scoped `ak.presence` Signal.
    ///
    /// The post-seal MLS state and strictly increasing payload sequence are
    /// persisted before the caller performs HTTP submit. This intentionally
    /// burns both values on an uncertain request and prevents replay/nonce
    /// reuse after a crash.
    #[allow(clippy::too_many_arguments)]
    pub fn seal_online_presence_signal(
        &self,
        realm_id: &str,
        principal_id: &str,
        device_id: &str,
        verification_method: &str,
        key_ref: &ArkretKeyRef,
        seal_ref: &str,
        sent_at: DateTime<Utc>,
    ) -> anyhow::Result<arkret_wire::SignalEnvelope> {
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let policy = state
            .realm_policies
            .get(realm_id)
            .filter(|policy| policy.requires_e2ee())
            .cloned()
            .with_context(|| format!("Realm '{realm_id}' has no E2EE Signal policy"))?;
        let mut store = state.mls_store()?;
        let group_id = policy.group_id_for_realm().into_owned();
        let record = store
            .mls_group_state(&group_id)
            .cloned()
            .with_context(|| format!("Realm '{realm_id}' has no accepted MLS group state"))?;
        let group_state_ref = group_state_ref_for_epoch(&state, &group_id, record.epoch)
            .with_context(|| {
                format!(
                    "Realm '{realm_id}' MLS epoch {} has no accepted group-state reference",
                    record.epoch
                )
            })?;
        // `ArkretMlsGroup` admits exactly the protocol ciphersuite exported by
        // the SDK. Use that negotiated wire id directly; a global registry
        // scan would become wrong as soon as a second suite is activated.
        let aead_profile = arkret::mls::ARKRET_MLS_CIPHERSUITE_CANONICAL_ID;
        let realm_id = RealmId::new(realm_id.to_owned())?;
        let actor_id = Did::new(principal_id.to_owned())?;
        let device_id = DeviceId::new(device_id.to_owned())?;
        let seal_ref = SealId::new(seal_ref.to_owned())?;
        let scope_ref = ScopeRef::Realm {
            realm_id: realm_id.clone(),
        };
        let expires_at = sent_at + chrono::Duration::seconds(30);
        let sequence_key = format!("realm:{}", realm_id.as_str());
        let payload_sequence = state
            .signal_sequences
            .get(&sequence_key)
            .copied()
            .unwrap_or(0);
        let plaintext = PresencePlaintext::new(
            payload_sequence,
            actor_id.clone(),
            PresenceState::Online,
            30_000,
        )?;
        let plaintext = seal_signal_plaintext(&plaintext)?;
        let signal_key_ref = arkret_wire::SignalKeyRef {
            algorithm: "MLS-EXPORTER-AEAD".to_owned(),
            group_state_ref: group_state_ref.clone(),
        };
        let mut group = ArkretMlsGroup::restore_from_state_record(&record)
            .map_err(|error| anyhow::anyhow!("restore Arkret MLS group: {error}"))?;
        let binding = arkret_wire::SignalAeadBinding {
            realm_id: &realm_id,
            scope_ref: &scope_ref,
            sender_actor_id: &actor_id,
            sender_device_id: &device_id,
            seal_ref: &seal_ref,
            signal_class: arkret_wire::SignalClass::Session,
            sent_at,
            expires_at,
            scheme: arkret_wire::signal::SIGNAL_AEAD_SCHEME,
            key_ref: &signal_key_ref,
            purpose: arkret_wire::signal::SIGNAL_AEAD_PURPOSE,
            aead_profile,
            epoch: record.epoch,
        };
        let sealed = group
            .seal_signal_payload(&binding, &plaintext)
            .map_err(|error| anyhow::anyhow!("seal Arkret presence Signal: {error}"))?;

        let updated = group
            .persist_state(&mut store)
            .map_err(|error| anyhow::anyhow!("persist post-Signal MLS group: {error}"))?;
        state.set_mls_store(&store)?;
        let next_sequence = payload_sequence
            .checked_add(1)
            .context("Arkret presence payload sequence exhausted")?;
        state.signal_sequences.insert(sequence_key, next_sequence);
        state.bootstrap.insert(
            updated.group_id.clone(),
            ArkretBootstrapRecord {
                group_id: updated.group_id,
                required_epoch: updated.epoch,
                local_epoch: Some(updated.epoch),
                group_state_ref: Some(group_state_ref),
                action: MlsRecoveryAction::UseLocalState,
                updated_at: sent_at,
            },
        );
        self.save(&mut state)?;

        let verification_method = arkret::DidUrl::new(verification_method.to_owned())
            .map_err(|error| anyhow::anyhow!("invalid Arkret verification method: {error}"))?;
        let mut envelope = arkret_wire::SignalEnvelope {
            realm_id,
            scope_ref,
            sender_actor_id: actor_id,
            sender_device_id: device_id,
            seal_ref,
            signal_class: arkret_wire::SignalClass::Session,
            sent_at,
            expires_at,
            encrypted_payload: sealed.encrypted_payload,
            proof: arkret_wire::SignalProof {
                kind: arkret::proof_kind::DETACHED_JWS.to_owned(),
                verification_method,
                alg: "EdDSA".to_owned(),
                envelope_digest: arkret::Hash::new(format!("sha256:{}", "0".repeat(64)))?,
                created_at: sent_at,
                domain: None,
                audience: None,
                jws: String::new(),
            },
        };
        let expected_aad = envelope.expected_aad_digest()?;
        if envelope.encrypted_payload.aad_digest != expected_aad {
            anyhow::bail!("Arkret presence ciphertext was sealed against a different header");
        }
        envelope.proof.envelope_digest = envelope.envelope_digest()?;
        let proof_bytes = envelope.proof_binding_bytes()?;
        let signing_key = load_ed25519_signing_key(key_ref)?;
        envelope.proof.jws =
            arkret_signatures::sign_eddsa_detached_jws(&signing_key, &proof_bytes)?;
        envelope.validate_structural()?;
        Ok(envelope)
    }

    pub fn update_realm_policies_from_sync(&self, realms_value: &Value) -> anyhow::Result<usize> {
        let Some(realms) = realms_value.as_object() else {
            return Ok(0);
        };
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let mut updated = 0usize;
        for (realm_id, realm_value) in realms {
            if let Some(policy) = extract_realm_crypto_policy(realm_id, realm_value) {
                state.realm_policies.insert(policy.realm_id.clone(), policy);
                updated += 1;
            }
        }
        if updated > 0 {
            self.save(&mut state)?;
        }
        Ok(updated)
    }

    pub fn ensure_mls_key_package(
        &self,
        principal_id: &str,
        device_id: &str,
        last_resort: bool,
    ) -> anyhow::Result<MlsKeyPackageRecord> {
        self.ensure_mls_key_package_inner(principal_id, device_id, last_resort, None)
    }

    pub fn ensure_agent_mls_key_package(
        &self,
        principal_id: &str,
        device_id: &str,
        last_resort: bool,
        key_ref: &super::signer::ArkretKeyRef,
    ) -> anyhow::Result<MlsKeyPackageRecord> {
        let signing_seed = super::signer::load_seed_array(key_ref)?;
        self.ensure_mls_key_package_inner(principal_id, device_id, last_resort, Some(signing_seed))
    }

    /// Create fresh ordinary Agent KeyPackages without replacing any
    /// previously generated private init-key material.
    ///
    /// The Principal Server's self-visible `available_count` is authoritative
    /// for pool replenishment. A locally `published` record may already be
    /// `claimed` remotely when a prior response or Welcome was lost, so the
    /// caller deliberately supplies the server-observed deficit here.
    pub fn create_fresh_agent_mls_key_packages(
        &self,
        principal_id: &str,
        device_id: &str,
        count: usize,
        key_ref: &super::signer::ArkretKeyRef,
    ) -> anyhow::Result<Vec<MlsKeyPackageRecord>> {
        use zeroize::Zeroize as _;

        if count == 0 {
            return Ok(Vec::new());
        }
        let mut signing_seed = super::signer::load_seed_array(key_ref)?;
        let expected_signature_key = ed25519_dalek::SigningKey::from_bytes(&signing_seed)
            .verifying_key()
            .to_bytes();
        signing_seed.zeroize();

        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let principal = Did::new(principal_id.to_owned())
            .with_context(|| format!("invalid Arkret principal DID '{principal_id}'"))?;
        let device = DeviceId::new(device_id.to_owned())
            .with_context(|| format!("invalid Arkret device id '{device_id}'"))?;
        let identity_key = mls_identity_key(&principal, &device);
        let identity_record = state.mls_identities.get(&identity_key).ok_or_else(|| {
            anyhow::anyhow!("Agent MLS identity must be initialized before pool replenishment")
        })?;
        let identity = restore_mls_identity(identity_record)?;
        anyhow::ensure!(
            identity.signature_public_key() == expected_signature_key.as_slice(),
            "Agent MLS identity does not match the currently authorized runtime key"
        );

        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let record = identity
                .key_package_record()
                .map_err(|err| anyhow::anyhow!("create Agent MLS KeyPackage: {err}"))?;
            let cache_key = mls_fresh_key_package_cache_key(&identity_key, &record.keypackage_id);
            state.mls_key_packages.insert(cache_key, record.clone());
            records.push(record);
        }
        let private_state = identity
            .export_private_state()
            .map_err(|err| anyhow::anyhow!("export Arkret MLS identity state: {err}"))?;
        state.mls_identities.insert(
            identity_key,
            ArkretMlsIdentityStateRecord {
                principal_id: principal,
                device_id: device,
                private_state,
                last_resort_key_package: false,
                keypackage_id: records.last().map(|record| record.keypackage_id.clone()),
                updated_at: Utc::now(),
            },
        );
        self.save(&mut state)?;
        Ok(records)
    }

    fn ensure_mls_key_package_inner(
        &self,
        principal_id: &str,
        device_id: &str,
        last_resort: bool,
        mut signing_seed: Option<[u8; 32]>,
    ) -> anyhow::Result<MlsKeyPackageRecord> {
        use zeroize::Zeroize as _;

        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let principal = Did::new(principal_id.to_owned())
            .with_context(|| format!("invalid Arkret principal DID '{principal_id}'"))?;
        let device = DeviceId::new(device_id.to_owned())
            .with_context(|| format!("invalid Arkret device id '{device_id}'"))?;
        let identity_key = mls_identity_key(&principal, &device);
        let cache_key = mls_key_package_cache_key(&principal, &device, last_resort);

        let expected_signature_key = signing_seed.as_ref().map(|seed| {
            ed25519_dalek::SigningKey::from_bytes(seed)
                .verifying_key()
                .to_bytes()
        });
        let restored = state
            .mls_identities
            .get(&identity_key)
            .map(restore_mls_identity)
            .transpose()?;
        let restored_matches_authorization = restored.as_ref().is_some_and(|identity| {
            expected_signature_key
                .as_ref()
                .is_none_or(|expected| identity.signature_public_key() == expected.as_slice())
        });

        if restored_matches_authorization
            && let Some(record) = state
                .mls_key_packages
                .get(&cache_key)
                .or_else(|| state.mls_key_packages.get(&identity_key))
                .cloned()
            && local_key_package_can_be_published(&record, last_resort)
        {
            if let Some(seed) = signing_seed.as_mut() {
                seed.zeroize();
            }
            return Ok(record);
        }

        let identity = if let Some(identity) = restored.filter(|identity| {
            expected_signature_key
                .as_ref()
                .is_none_or(|expected| identity.signature_public_key() == expected.as_slice())
        }) {
            if let Some(seed) = signing_seed.as_mut() {
                seed.zeroize();
            }
            identity
        } else if let Some(seed) = signing_seed.take() {
            ArkretMlsIdentity::from_ed25519_signing_seed(principal.clone(), device.clone(), seed)
                .map_err(|err| anyhow::anyhow!("create Agent-bound Arkret MLS identity: {err}"))?
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
        self.save(&mut state)?;
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

    pub fn mark_mls_key_package_revoked(
        &self,
        keypackage_ref_or_id: &str,
    ) -> anyhow::Result<Option<MlsKeyPackageRecord>> {
        self.update_cached_mls_key_package(keypackage_ref_or_id, |record| {
            record.state = MlsKeyPackageState::Revoked;
            Ok(())
        })
    }

    /// Canonical KeyPackage refs for every locally-tracked pool KeyPackage that
    /// has not already been consumed or revoked.
    ///
    /// The revoke endpoint resolves its signed targets by `keypackage_ref`,
    /// not by the client-generated `keypackage_id` used as the durable row
    /// primary key.
    pub fn revocable_keypackage_refs(&self) -> anyhow::Result<Vec<String>> {
        let state = self.load()?;
        Ok(Self::revocable_keypackage_refs_matching(&state, None, None))
    }

    /// Canonical KeyPackage refs in this store that belong to one exact Agent
    /// runtime binding and have not already been consumed or revoked.
    ///
    /// This is used by the pairing-scoped account migration.  Older Savfox
    /// releases keyed Arkret crypto state only by channel/account id, so one
    /// legacy file can contain material from several replaced Agents.  A new
    /// Agent MUST revoke only its own principal/device rows; attempting to
    /// revoke another Agent's rows both fails authorization and leaves the
    /// current runtime exposed to claims for private material it no longer
    /// opens.
    pub fn revocable_keypackage_refs_for_agent(
        &self,
        principal_id: &str,
        device_id: &str,
    ) -> anyhow::Result<Vec<String>> {
        let principal = Did::new(principal_id.to_owned())
            .with_context(|| format!("invalid Arkret principal DID '{principal_id}'"))?;
        let device = DeviceId::new(device_id.to_owned())
            .with_context(|| format!("invalid Arkret device id '{device_id}'"))?;
        let state = self.load()?;
        Ok(Self::revocable_keypackage_refs_matching(
            &state,
            Some(&principal),
            Some(&device),
        ))
    }

    fn revocable_keypackage_refs_matching(
        state: &ArkretCryptoStateFile,
        principal_id: Option<&Did>,
        device_id: Option<&DeviceId>,
    ) -> Vec<String> {
        let mut refs = Vec::new();
        for record in state.mls_key_packages.values() {
            if matches!(
                record.state,
                MlsKeyPackageState::Consumed | MlsKeyPackageState::Revoked
            ) {
                continue;
            }
            if principal_id.is_some_and(|principal| &record.principal_id != principal)
                || device_id.is_some_and(|device| &record.device_id != device)
            {
                continue;
            }
            let keypackage_ref = record.keypackage_ref.as_str().to_owned();
            if !refs.contains(&keypackage_ref) {
                refs.push(keypackage_ref);
            }
        }
        refs
    }

    /// Delete the persisted crypto-state file for this account. Used by unbind
    /// to purge the Agent's MLS identity and private KeyPackage material after
    /// the server-side pool has been revoked. Removing a missing file is a
    /// no-op, not an error.
    pub fn delete_persisted(&self) -> std::io::Result<()> {
        match std::fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    pub fn record_mls_welcome(&self, welcome: MlsWelcomeEnvelope) -> anyhow::Result<()> {
        self.record_mls_welcome_inner(welcome, None)
    }

    fn record_mls_welcome_inner(
        &self,
        welcome: MlsWelcomeEnvelope,
        consume_binding: Option<ArkretMlsWelcomeConsumeBinding>,
    ) -> anyhow::Result<()> {
        let _guard = self.mutation_lock.lock();
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
                group_state_ref: consume_binding
                    .as_ref()
                    .and_then(|binding| binding.group_state_ref.clone()),
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
        self.save(&mut state)
    }

    pub fn record_mls_welcome_from_value(
        &self,
        value: &Value,
    ) -> anyhow::Result<Option<MlsWelcomeEnvelope>> {
        let Some(welcome) = extract_mls_welcome_envelope(value) else {
            return Ok(None);
        };
        let state = self.load()?;
        let mut consume_binding = extract_mls_welcome_consume_binding(value);
        if let Some(binding) = consume_binding.as_mut() {
            binding.welcome_ref = binding
                .welcome_ref
                .clone()
                .or_else(|| extract_mls_welcome_event_ref(value));
            enrich_mls_welcome_consume_binding(
                binding,
                &state.direct_conversation_welcome_bindings,
            );
        }
        drop(state);
        self.record_mls_welcome_inner(welcome.clone(), consume_binding)?;
        Ok(Some(welcome))
    }

    /// Apply one accepted durable `ak.mls.commit` to the local MLS group and
    /// persist the post-Commit snapshot before any later encrypted DataEvent is
    /// handled. Replaying the same accepted Commit is idempotent; an epoch gap
    /// fails closed instead of fabricating ratchet state.
    pub fn apply_mls_commit(
        &self,
        payload: &MlsCommitPayload,
        accepted_event_ref: &EventId,
    ) -> anyhow::Result<bool> {
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let mut store = state.mls_store()?;
        if store.mls_group_state(payload.mls_group_id()).is_none() {
            consume_stored_welcome_for_commit(
                &state,
                &mut store,
                payload.mls_group_id(),
                payload.base_epoch(),
            )?;
        }
        let record = store
            .mls_group_state(payload.mls_group_id())
            .cloned()
            .with_context(|| {
                format!(
                    "Arkret MLS Commit for group '{}' has no admitted local group state",
                    payload.mls_group_id()
                )
            })?;
        if record.epoch > payload.next_epoch() {
            return Ok(false);
        }
        if record.epoch == payload.next_epoch() {
            state.bootstrap.insert(
                record.group_id.clone(),
                ArkretBootstrapRecord {
                    group_id: record.group_id,
                    required_epoch: record.epoch,
                    local_epoch: Some(record.epoch),
                    group_state_ref: Some(accepted_event_ref.to_string()),
                    action: MlsRecoveryAction::UseLocalState,
                    updated_at: Utc::now(),
                },
            );
            state.set_mls_store(&store)?;
            self.save(&mut state)?;
            return Ok(false);
        }
        if record.epoch != payload.base_epoch() {
            anyhow::bail!(
                "Arkret MLS Commit epoch gap for group '{}': local={}, commit base={}, next={}",
                payload.mls_group_id(),
                record.epoch,
                payload.base_epoch(),
                payload.next_epoch()
            );
        }
        let mut group = ArkretMlsGroup::restore_from_state_record(&record)
            .map_err(|err| anyhow::anyhow!("restore Arkret MLS group: {err}"))?;
        let applied_epoch = group
            .apply_commit(&payload.commit_envelope())
            .map_err(|err| anyhow::anyhow!("apply Arkret MLS Commit: {err}"))?;
        if applied_epoch != payload.next_epoch() {
            anyhow::bail!(
                "Arkret MLS Commit produced epoch {applied_epoch}, expected {}",
                payload.next_epoch()
            );
        }
        let updated = group
            .persist_state(&mut store)
            .map_err(|err| anyhow::anyhow!("persist post-Commit Arkret MLS group: {err}"))?;
        state.set_mls_store(&store)?;
        state.bootstrap.insert(
            updated.group_id.clone(),
            ArkretBootstrapRecord {
                group_id: updated.group_id,
                required_epoch: updated.epoch,
                local_epoch: Some(updated.epoch),
                group_state_ref: Some(accepted_event_ref.to_string()),
                action: MlsRecoveryAction::UseLocalState,
                updated_at: Utc::now(),
            },
        );
        self.save(&mut state)?;
        Ok(true)
    }

    pub fn record_direct_conversation_binding_from_value(
        &self,
        value: &Value,
    ) -> anyhow::Result<usize> {
        let mut payloads = Vec::new();
        collect_direct_conversation_bound_payloads(value, 8, &mut payloads);
        if payloads.is_empty() {
            return Ok(0);
        }
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        for payload in &payloads {
            let binding = ArkretDirectConversationWelcomeBinding {
                welcome_ref: payload.mls_welcome_event_ref.to_string(),
                realm_id: payload.realm_id.to_string(),
                strand_id: payload.main_strand_id.to_string(),
                mls_group_id: payload.mls_group_id.to_string(),
            };
            state
                .direct_conversation_welcome_bindings
                .insert(binding.welcome_ref.clone(), binding);
        }
        for consume_binding in state.mls_welcome_consume_bindings.values_mut() {
            enrich_mls_welcome_consume_binding(
                consume_binding,
                &state.direct_conversation_welcome_bindings,
            );
        }
        self.save(&mut state)?;
        Ok(payloads.len())
    }

    pub fn validate_agent_mls_welcome_value_tree(
        &self,
        value: &Value,
        principal_id: &str,
        device_id: &str,
        authorized_event_ref: &str,
    ) -> anyhow::Result<bool> {
        let mut payloads = Vec::new();
        collect_typed_mls_welcome_payloads(value, 8, &mut payloads);
        if payloads.is_empty() {
            return Ok(false);
        }
        for payload in &payloads {
            self.validate_agent_mls_welcome_payload(
                payload,
                principal_id,
                device_id,
                authorized_event_ref,
            )?;
        }
        Ok(true)
    }

    fn validate_agent_mls_welcome_payload(
        &self,
        payload: &MlsWelcomePayload,
        principal_id: &str,
        device_id: &str,
        authorized_event_ref: &str,
    ) -> anyhow::Result<()> {
        if payload.recipient_principal_id.as_str() != principal_id
            || payload.recipient_device_id.as_str() != device_id
        {
            anyhow::bail!("MLS Welcome recipient does not match this Agent runtime");
        }
        match &payload.claim_ref.trust_binding {
            arkret::MlsClaimTrustBinding::AgentKeyAuthorizeEventId(event_id)
                if event_id.as_str() == authorized_event_ref => {}
            _ => anyhow::bail!(
                "MLS Welcome claim_ref is not bound to the current ak.agent.key.authorize Event"
            ),
        }
        let state = self.load()?;
        let local_keypackage = state.mls_key_packages.values().find(|record| {
            record.keypackage_ref.as_str() == payload.keypackage_ref.as_str()
                || record.keypackage_id == payload.keypackage_ref.as_str()
        });
        let Some(local_keypackage) = local_keypackage else {
            anyhow::bail!("MLS Welcome references no locally held Agent KeyPackage");
        };
        if local_keypackage.keypackage_ref.as_str() != payload.keypackage_digest.as_str()
            || payload.claim_ref.keypackage_digest != payload.keypackage_digest
            || payload.claim_ref.keypackage_ref != payload.keypackage_ref
        {
            anyhow::bail!("MLS Welcome KeyPackage claim binding does not match local state");
        }
        Ok(())
    }

    pub fn mark_mls_welcome_consume_binding_acked(
        &self,
        binding: &ArkretMlsWelcomeConsumeBinding,
    ) -> anyhow::Result<()> {
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        state
            .mls_welcome_consume_bindings
            .remove(&binding.cache_key());
        self.save(&mut state)
    }

    pub fn pending_mls_welcome_consume_bindings(
        &self,
    ) -> anyhow::Result<Vec<ArkretMlsWelcomeConsumeBinding>> {
        Ok(self
            .load()?
            .mls_welcome_consume_bindings
            .into_values()
            .collect())
    }

    pub fn repair_pending_direct_conversation_bindings_from_accepted_events(
        &self,
        events: &[arkret::Event],
    ) -> anyhow::Result<usize> {
        let strand_ids = events
            .iter()
            .filter(|event| event.kind.as_str() == "ak.strand.create")
            .filter_map(|event| {
                serde_json::to_value(&event.payload)
                    .ok()
                    .and_then(|value| serde_json::from_value::<StrandCreatePayload>(value).ok())
                    .filter(|payload| payload.object.realm_id == event.realm_id)
                    .map(|payload| payload.object.id.to_string())
            })
            .collect::<Vec<_>>();
        let [strand_id] = strand_ids.as_slice() else {
            return Ok(0);
        };

        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let mut repaired = 0;
        for event in events
            .iter()
            .filter(|event| event.kind.as_str() == "ak.mls.welcome")
        {
            let Ok(value) = serde_json::to_value(&event.payload) else {
                continue;
            };
            let Ok(payload) = serde_json::from_value::<MlsWelcomePayload>(value) else {
                continue;
            };
            for binding in state.mls_welcome_consume_bindings.values_mut() {
                if binding.keypackage_ref == payload.keypackage_ref
                    && binding.claim_id == payload.claim_id.to_string()
                    && binding.mls_group_id == payload.mls_group_id.to_string()
                    && binding.epoch == payload.epoch
                    && binding.realm_id.as_deref()
                        == Some(payload.claim_envelope.intended_realm_id.as_str())
                {
                    binding.welcome_ref = Some(event.event_id.to_string());
                    binding.strand_id = Some(strand_id.clone());
                    repaired += 1;
                }
            }
        }
        if repaired > 0 {
            self.save(&mut state)?;
        }
        Ok(repaired)
    }

    pub fn plan_bootstrap_for_payload(
        &self,
        principal_id: &str,
        device_id: &str,
        payload: &EncryptedPayload,
    ) -> anyhow::Result<ArkretBootstrapRecord> {
        let _guard = self.mutation_lock.lock();
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
        let group_state_ref = group_state_ref_for_epoch(&state, &payload.group_id, payload.epoch);
        let record = ArkretBootstrapRecord {
            group_id: plan.group_id,
            required_epoch: payload.epoch,
            local_epoch,
            group_state_ref,
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
        self.save(&mut state)?;
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
        let _guard = self.mutation_lock.lock();
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
        self.save(&mut state)
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
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let mut store = state.mls_store()?;
        if store.mls_group_state(&payload.group_id).is_none() {
            let Some((updated, _welcome)) =
                try_consume_stored_welcome_for_payload(&state, &mut store, payload)?
            else {
                return Ok(ArkretDecryptDetailedOutcome::MissingGroupState);
            };
            let group_state_ref =
                group_state_ref_for_epoch(&state, &payload.group_id, updated.epoch);
            state.bootstrap.insert(
                updated.group_id.clone(),
                ArkretBootstrapRecord {
                    group_id: updated.group_id,
                    required_epoch: updated.epoch,
                    local_epoch: Some(updated.epoch),
                    group_state_ref,
                    action: MlsRecoveryAction::ConsumeWelcome,
                    updated_at: Utc::now(),
                },
            );
            state.set_mls_store(&store)?;
            self.save(&mut state)?;
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
        let group_state_ref = group_state_ref_for_epoch(&state, &payload.group_id, updated.epoch);
        state.bootstrap.insert(
            updated.group_id.clone(),
            ArkretBootstrapRecord {
                group_id: updated.group_id,
                required_epoch: updated.epoch,
                local_epoch: Some(updated.epoch),
                group_state_ref,
                action: MlsRecoveryAction::UseLocalState,
                updated_at: Utc::now(),
            },
        );
        let consume_bindings = state
            .mls_welcome_consume_bindings
            .values()
            .filter(|binding| {
                binding.mls_group_id == payload.group_id && binding.epoch <= updated.epoch
            })
            .cloned()
            .collect::<Vec<_>>();
        self.save(&mut state)?;
        Ok(ArkretDecryptDetailedOutcome::Decrypted {
            content,
            consume_bindings,
        })
    }

    pub fn encrypt_content_block_for_realm(
        &self,
        realm_id: &str,
        content: &Value,
    ) -> anyhow::Result<ArkretEncryptOutcome<ContentBlock>> {
        let content: ContentBlock = serde_json::from_value(content.clone())
            .with_context(|| "Arkret message content is not a ContentBlock")?;
        self.encrypt_typed_payload_for_realm(realm_id, &content)
    }

    pub fn encrypt_message_metadata_for_realm(
        &self,
        realm_id: &str,
        metadata: &MessageMetadata,
    ) -> anyhow::Result<ArkretEncryptOutcome<MessageMetadata>> {
        self.encrypt_typed_payload_for_realm(realm_id, metadata)
    }

    fn encrypt_typed_payload_for_realm<T>(
        &self,
        realm_id: &str,
        plaintext_value: &T,
    ) -> anyhow::Result<ArkretEncryptOutcome<T>>
    where
        T: MlsPayloadType + Serialize,
    {
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let Some(policy) = state.realm_policies.get(realm_id).cloned() else {
            return Ok(ArkretEncryptOutcome::PlaintextAllowed);
        };
        if !policy.requires_e2ee() {
            return Ok(ArkretEncryptOutcome::PlaintextAllowed);
        }
        let mut store = state.mls_store()?;
        let group_id = policy.group_id_for_realm().into_owned();
        let Some(record) = store.mls_group_state(&group_id).cloned() else {
            return Ok(ArkretEncryptOutcome::MissingRequiredGroupState {
                group_id,
                realm_id: realm_id.to_owned(),
            });
        };
        let mut group = ArkretMlsGroup::restore_from_state_record(&record)
            .map_err(|err| anyhow::anyhow!("restore Arkret MLS group: {err}"))?;
        let group_state_ref = group_state_ref_for_epoch(&state, &group_id, record.epoch)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Arkret MLS group '{group_id}' epoch {} has no verified group_state_ref",
                    record.epoch
                )
            })?;
        let aad = arkret::EncryptedEnvelopeAad::hidden(
            RealmId::new(realm_id.to_owned())
                .with_context(|| format!("invalid Arkret realm id '{realm_id}'"))?,
            arkret::events::EventKind::MESSAGE_CREATE,
        );
        let plaintext = serde_json::to_vec(plaintext_value)?;
        let payload = group
            .encrypt_payload_with_aad(T::MLS_CONTENT_TYPE, Some(aad.clone()), &plaintext)
            .map_err(|err| anyhow::anyhow!("encrypt Arkret MLS payload: {err}"))?;
        let envelope = arkret::mls::encrypted_envelope_from_payload(
            &payload,
            aad,
            arkret::EncryptedEnvelopeAadVisibility::Hidden,
            arkret::AadVisibilityCeiling::from_declared(None),
            group_state_ref.clone(),
        )
        .map_err(|err| anyhow::anyhow!("build Arkret encrypted envelope: {err}"))?;
        let envelope = MlsEncryptedPayload::<T>::new(envelope)
            .map_err(|err| anyhow::anyhow!("type Arkret MLS payload: {err}"))?;
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
                group_state_ref: Some(group_state_ref),
                action: MlsRecoveryAction::UseLocalState,
                updated_at: Utc::now(),
            },
        );
        self.save(&mut state)?;
        Ok(ArkretEncryptOutcome::Encrypted(envelope))
    }

    fn update_cached_mls_key_package(
        &self,
        keypackage_ref_or_id: &str,
        update: impl FnOnce(&mut MlsKeyPackageRecord) -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<MlsKeyPackageRecord>> {
        let _guard = self.mutation_lock.lock();
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
        self.save(&mut state)?;
        Ok(Some(updated))
    }
}

fn group_state_ref_for_epoch(
    state: &ArkretCryptoStateFile,
    group_id: &str,
    epoch: u64,
) -> Option<String> {
    state
        .bootstrap
        .get(group_id)
        .filter(|bootstrap| bootstrap.local_epoch == Some(epoch))
        .and_then(|bootstrap| bootstrap.group_state_ref.clone())
        .or_else(|| {
            state
                .mls_welcome_consume_bindings
                .values()
                .find(|binding| {
                    binding.mls_group_id == group_id
                        && binding.epoch == epoch
                        && binding.group_state_ref.is_some()
                })
                .and_then(|binding| binding.group_state_ref.clone())
        })
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

#[derive(Debug, PartialEq)]
pub enum ArkretEncryptOutcome<T: MlsPayloadType> {
    PlaintextAllowed,
    Encrypted(MlsEncryptedPayload<T>),
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

#[must_use]
pub fn extract_encrypted_payload_from_message_content(
    content: &BTreeMap<String, Value>,
) -> Option<EncryptedPayload> {
    let envelope = serde_json::from_value(content.get("encrypted_content")?.clone()).ok()?;
    arkret::mls::encrypted_envelope_to_payload(&envelope).ok()
}

/// Extract the `encrypted_metadata` carrier of a message payload, if any.
///
/// The envelope shape is identical to `encrypted_content`
/// (`encrypted-envelope.schema.json`); only the payload plaintext differs —
/// `message_metadata` JSON instead of a content block. Decryption goes
/// through the same MLS group as the content carrier.
#[must_use]
pub fn extract_encrypted_metadata_payload_from_message_content(
    content: &BTreeMap<String, Value>,
) -> Option<EncryptedPayload> {
    let envelope = serde_json::from_value(content.get("encrypted_metadata")?.clone()).ok()?;
    arkret::mls::encrypted_envelope_to_payload(&envelope).ok()
}

#[must_use]
pub fn message_content_has_encrypted_carrier(content: &BTreeMap<String, Value>) -> bool {
    content.get("encrypted_content").is_some()
}

#[must_use]
pub fn extract_mls_welcome_envelope(value: &Value) -> Option<MlsWelcomeEnvelope> {
    if let Ok(welcome) = serde_json::from_value::<MlsWelcomeEnvelope>(value.clone()) {
        return Some(welcome);
    }
    if let Ok(payload) = serde_json::from_value::<MlsWelcomePayload>(value.clone()) {
        let ciphertext = payload.carrier.ciphertext()?.to_owned();
        let welcome_bytes = arkret::base64url_decode(ciphertext.as_bytes()).ok()?;
        let welcome_hash =
            arkret::Hash::new(arkret::canonical::sha256_digest(&welcome_bytes)).ok()?;
        if welcome_hash != payload.claim_envelope.welcome_digest {
            return None;
        }
        return Some(MlsWelcomeEnvelope {
            group_id: payload.mls_group_id.as_str().to_owned(),
            epoch: payload.epoch,
            recipient_principal_id: payload.recipient_principal_id,
            recipient_device_id: payload.recipient_device_id,
            welcome: ciphertext,
            welcome_hash,
            ratchet_tree: None,
        });
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

#[must_use]
pub fn extract_mls_welcome_consume_binding(
    value: &Value,
) -> Option<ArkretMlsWelcomeConsumeBinding> {
    extract_mls_welcome_consume_binding_inner(value, 6)
}

fn collect_typed_mls_welcome_payloads(
    value: &Value,
    remaining_depth: usize,
    payloads: &mut Vec<MlsWelcomePayload>,
) {
    if let Ok(payload) = serde_json::from_value::<MlsWelcomePayload>(value.clone()) {
        payloads.push(payload);
        return;
    }
    if remaining_depth == 0 {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect_typed_mls_welcome_payloads(item, remaining_depth - 1, payloads);
            }
        }
        Value::Object(object) => {
            for item in object.values() {
                collect_typed_mls_welcome_payloads(item, remaining_depth - 1, payloads);
            }
        }
        _ => {}
    }
}

fn collect_direct_conversation_bound_payloads(
    value: &Value,
    remaining_depth: usize,
    payloads: &mut Vec<DirectConversationBoundPayload>,
) {
    if let Ok(payload) = serde_json::from_value::<DirectConversationBoundPayload>(value.clone()) {
        payloads.push(payload);
        return;
    }
    if remaining_depth == 0 {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect_direct_conversation_bound_payloads(item, remaining_depth - 1, payloads);
            }
        }
        Value::Object(object) => {
            for item in object.values() {
                collect_direct_conversation_bound_payloads(item, remaining_depth - 1, payloads);
            }
        }
        _ => {}
    }
}

fn extract_mls_welcome_event_ref(value: &Value) -> Option<String> {
    let Value::Object(object) = value else {
        return None;
    };
    if string_field(object, &["kind"]).as_deref() == Some("ak.mls.welcome") {
        return string_field(object, &["event_id", "eventId"]);
    }
    object.values().find_map(extract_mls_welcome_event_ref)
}

fn enrich_mls_welcome_consume_binding(
    binding: &mut ArkretMlsWelcomeConsumeBinding,
    direct_bindings: &BTreeMap<String, ArkretDirectConversationWelcomeBinding>,
) {
    let exact = binding
        .welcome_ref
        .as_ref()
        .and_then(|welcome_ref| direct_bindings.get(welcome_ref));
    let matched = exact.or_else(|| {
        let mut candidates = direct_bindings.values().filter(|candidate| {
            candidate.mls_group_id == binding.mls_group_id
                && binding
                    .realm_id
                    .as_ref()
                    .is_none_or(|realm_id| realm_id == &candidate.realm_id)
        });
        let candidate = candidates.next()?;
        candidates.next().is_none().then_some(candidate)
    });
    let Some(direct) = matched else {
        return;
    };
    binding.welcome_ref = Some(direct.welcome_ref.clone());
    binding.realm_id = Some(direct.realm_id.clone());
    binding.strand_id = Some(direct.strand_id.clone());
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
            keypackage_ref: payload.keypackage_ref.clone(),
            claim_id: payload.claim_id.into_string(),
            welcome_ref,
            realm_id: Some(payload.claim_envelope.intended_realm_id.as_str().to_owned()),
            strand_id: None,
            mls_group_id: payload.mls_group_id.to_string(),
            epoch: payload.epoch,
            group_state_ref: payload.commit_ref.map(|event_id| event_id.to_string()),
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
    let group_state_ref = string_field(object, &["commit_ref", "commitRef"]);

    Some(ArkretMlsWelcomeConsumeBinding {
        keypackage_ref,
        claim_id,
        welcome_ref,
        realm_id,
        strand_id,
        mls_group_id,
        epoch,
        group_state_ref,
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
    candidates.sort_by_key(|(_, welcome)| std::cmp::Reverse(welcome.epoch));

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

fn consume_stored_welcome_for_commit(
    state: &ArkretCryptoStateFile,
    store: &mut MemoryCryptoStore,
    group_id: &str,
    base_epoch: u64,
) -> anyhow::Result<()> {
    let mut candidates = Vec::new();
    for identity_record in state.mls_identities.values() {
        candidates.extend(
            store
                .welcomes_for_device(&identity_record.principal_id, &identity_record.device_id)
                .into_iter()
                .filter(|welcome| welcome.group_id == group_id && welcome.epoch <= base_epoch)
                .cloned()
                .map(|welcome| (identity_record.clone(), welcome)),
        );
    }
    candidates.sort_by_key(|(_, welcome)| std::cmp::Reverse(welcome.epoch));

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
            Ok(group) if group.epoch() == base_epoch => {
                group
                    .persist_state(store)
                    .map_err(|err| anyhow::anyhow!("persist Arkret MLS group: {err}"))?;
                return Ok(());
            }
            Ok(group) => {
                last_error = Some(anyhow::anyhow!(
                    "MLS Welcome joined epoch {}, but Commit requires base epoch {base_epoch}",
                    group.epoch()
                ));
            }
            Err(err) => last_error = Some(anyhow::anyhow!("{err}")),
        }
    }
    if let Some(err) = last_error {
        anyhow::bail!("consume Arkret MLS Welcome before Commit failed: {err}");
    }
    anyhow::bail!(
        "no usable Arkret MLS Welcome for group '{group_id}' at Commit base epoch {base_epoch}"
    )
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

fn mls_fresh_key_package_cache_key(identity_key: &str, keypackage_id: &str) -> String {
    format!("{identity_key}#single_use#{keypackage_id}")
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
    if realm_value
        .pointer("/state_at_window_start/realm_metadata/collaboration_role")
        .and_then(Value::as_str)
        .is_some_and(|role| role.eq_ignore_ascii_case("direct_conversation"))
    {
        return Some(ArkretRealmCryptoPolicy {
            realm_id: realm_id.to_owned(),
            content_encryption_floor: ArkretContentEncryptionFloor::E2eeRequired,
            encryption_profile: Some("mls_rfc9420".to_owned()),
            // Inkson derives the realm-scoped MLS group identifier from the
            // base64url-no-pad encoding of the canonical realm identifier.
            mls_group_id: Some(URL_SAFE_NO_PAD.encode(realm_id.trim().as_bytes())),
            source: "account_subscribe_direct_conversation".to_owned(),
            updated_at: Utc::now(),
        });
    }
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
    use arkret::{EncryptedPayloadScheme, Hash, KeyOperationSignature, KeyPackageClaimRecord};
    use arkret_crypto::UnableToDecryptReason;
    use base64::Engine as _;
    use serde_json::json;

    use super::*;

    #[test]
    fn crypto_state_is_wrapped_at_rest_and_rejects_stale_generation() {
        let home = tempfile::tempdir().expect("tempdir");
        let store = FileArkretCryptoStore::for_account(home.path(), "wrapped", "agent");
        store.ensure_created().expect("create wrapped state");

        let raw = std::fs::read_to_string(store.path()).expect("read wrapped file");
        assert!(raw.contains(WRAPPED_STATE_VERSION));
        assert!(!raw.contains("mls_store_json"));
        assert!(!raw.contains("private_state"));

        let mut stale = store.load().expect("load stale snapshot");
        let mut current = store.load().expect("load current snapshot");
        current.key_backup.restore_needed = true;
        store.save(&mut current).expect("advance generation");
        stale.key_backup.restore_needed = false;
        let error = store
            .save(&mut stale)
            .expect_err("stale generation must fail closed");
        assert!(error.to_string().contains("generation conflict"));
    }

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

    fn direct_conversation_bound_payload(
        realm_id: &str,
        strand_id: &str,
        group_id: &str,
        welcome_ref: &str,
    ) -> Value {
        json!({
            "pair_key": format!("sha256:{}", "aa".repeat(32)),
            "participants_unordered": [
                "did:webvh:z6mkfixture:alice.example",
                "did:webvh:z6mkfixture:agent.example"
            ],
            "realm_id": realm_id,
            "main_strand_id": strand_id,
            "authorization_basis": {
                "kind": "accepted_contact",
                "event_refs": [
                    "ak:event:01904100-0000-7000-8000-000000000001",
                    "ak:event:01904100-0000-7000-8000-000000000002"
                ]
            },
            "member_event_refs": [
                "ak:event:01904100-0000-7000-8000-000000000003",
                "ak:event:01904100-0000-7000-8000-000000000004"
            ],
            "main_strand_create_ref": "ak:event:01904100-0000-7000-8000-000000000005",
            "mls_group_id": group_id,
            "mls_genesis_event_ref": "ak:event:01904100-0000-7000-8000-000000000006",
            "mls_commit_event_ref": "ak:event:01904100-0000-7000-8000-000000000007",
            "mls_welcome_event_ref": welcome_ref,
            "created_at": "2026-08-01T00:00:00.000Z",
            "binding_state": "active"
        })
    }

    #[test]
    fn direct_conversation_binding_enriches_pending_welcome_consume_in_either_order() {
        let home = temp_home("direct-welcome-binding-order");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let realm_id = "ak:realm:01904100-0000-7000-8000-000000000011";
        let strand_id = "ak:strand:01904100-0000-7000-8000-000000000012";
        let group_id = "AZZBmwAAAACAAAAAAAAAAQ";
        let welcome_ref = "ak:event:01904100-0000-7000-8000-000000000013";
        let pending = ArkretMlsWelcomeConsumeBinding {
            keypackage_ref: "ak:mls:keypackage:pending".to_owned(),
            claim_id: "ak:claim:pending".to_owned(),
            welcome_ref: Some(welcome_ref.to_owned()),
            realm_id: Some(realm_id.to_owned()),
            strand_id: None,
            mls_group_id: group_id.to_owned(),
            epoch: 1,
            group_state_ref: None,
        };
        let mut state = store.load().expect("state should load");
        state
            .mls_welcome_consume_bindings
            .insert(pending.cache_key(), pending.clone());
        store
            .save(&mut state)
            .expect("pending binding should persist");

        let bound = direct_conversation_bound_payload(realm_id, strand_id, group_id, welcome_ref);
        assert_eq!(
            store
                .record_direct_conversation_binding_from_value(&json!({"payload": bound}))
                .expect("typed Direct Conversation binding should persist"),
            1
        );
        let state = store.load().expect("enriched state should load");
        let enriched = state
            .mls_welcome_consume_bindings
            .get(&pending.cache_key())
            .expect("pending consume should remain queued");
        assert_eq!(enriched.strand_id.as_deref(), Some(strand_id));

        let mut later = ArkretMlsWelcomeConsumeBinding {
            keypackage_ref: "ak:mls:keypackage:later".to_owned(),
            claim_id: "ak:claim:later".to_owned(),
            welcome_ref: None,
            realm_id: Some(realm_id.to_owned()),
            strand_id: None,
            mls_group_id: group_id.to_owned(),
            epoch: 1,
            group_state_ref: None,
        };
        enrich_mls_welcome_consume_binding(&mut later, &state.direct_conversation_welcome_bindings);
        assert_eq!(later.welcome_ref.as_deref(), Some(welcome_ref));
        assert_eq!(later.strand_id.as_deref(), Some(strand_id));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn agent_mls_identity_reuses_authorized_runtime_key() {
        let home = temp_home("agent-runtime-mls-key");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let seed = [9_u8; 32];
        let key_ref = crate::arkret::ArkretKeyRef::InlineSeedBase64 {
            value: base64::engine::general_purpose::STANDARD_NO_PAD.encode(seed),
        };
        let principal = "did:webvh:z6mkfixture:agent.example";
        let device = "ak:device:01904100-0000-7000-8000-00000000000f";
        let first = store
            .ensure_agent_mls_key_package(principal, device, false, &key_ref)
            .unwrap();

        let rotated_seed = [10_u8; 32];
        let rotated_key_ref = crate::arkret::ArkretKeyRef::InlineSeedBase64 {
            value: base64::engine::general_purpose::STANDARD_NO_PAD.encode(rotated_seed),
        };
        let rotated = store
            .ensure_agent_mls_key_package(principal, device, false, &rotated_key_ref)
            .unwrap();

        let state = store.load().unwrap();
        let identity = restore_mls_identity(state.mls_identities.values().next().unwrap()).unwrap();
        let expected = ed25519_dalek::SigningKey::from_bytes(&rotated_seed)
            .verifying_key()
            .to_bytes();
        assert_eq!(identity.signature_public_key(), expected.as_slice());
        assert_ne!(first.keypackage_ref, rotated.keypackage_ref);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn agent_keypackage_replenishment_keeps_distinct_private_material() {
        let home = temp_home("agent-keypackage-pool");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let seed = [11_u8; 32];
        let key_ref = crate::arkret::ArkretKeyRef::InlineSeedBase64 {
            value: base64::engine::general_purpose::STANDARD_NO_PAD.encode(seed),
        };
        let principal = "did:webvh:z6mkfixture:agent.example";
        let device = "ak:device:01904100-0000-7000-8000-000000000010";
        let initial = store
            .ensure_agent_mls_key_package(principal, device, false, &key_ref)
            .unwrap();
        let fresh = store
            .create_fresh_agent_mls_key_packages(principal, device, 8, &key_ref)
            .unwrap();

        assert_eq!(fresh.len(), 8);
        assert!(fresh.iter().all(|record| {
            !record.last_resort && record.state == MlsKeyPackageState::Published
        }));
        let mut refs = fresh
            .iter()
            .map(|record| record.keypackage_ref.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        refs.insert(initial.keypackage_ref.as_str());
        assert_eq!(refs.len(), 9);

        let state = store.load().unwrap();
        assert_eq!(state.mls_key_packages.len(), 9);
        let identity = restore_mls_identity(state.mls_identities.values().next().unwrap()).unwrap();
        let expected = ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes();
        assert_eq!(identity.signature_public_key(), expected.as_slice());
        let _ = std::fs::remove_dir_all(&home);
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
            purpose: None,
            aead_profile: None,
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
        let aad = arkret::EncryptedEnvelopeAad::hidden(
            RealmId::new("ak:realm:01904100-0000-7000-8000-000000000001").unwrap(),
            arkret::events::EventKind::MESSAGE_CREATE,
        );
        let mut payload = encrypted_payload();
        payload.aad = Some(aad.clone());
        let envelope = arkret::mls::encrypted_envelope_from_payload(
            &payload,
            aad,
            arkret::EncryptedEnvelopeAadVisibility::Hidden,
            arkret::AadVisibilityCeiling::from_declared(None),
            "sha256:2222222222222222222222222222222222222222222222222222222222222222",
        )
        .expect("envelope");
        let content = BTreeMap::from([(
            "encrypted_content".to_owned(),
            serde_json::to_value(envelope).expect("envelope should serialize"),
        )]);
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
    fn direct_conversation_sync_projection_persists_required_e2ee_policy() {
        let home = temp_home("direct-conversation-policy");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        let realm_id = "ak:realm:01904100-0000-7000-8000-000000000001";
        let realms = json!({
            realm_id: {
                "state_at_window_start": {
                    "actor_profiles": {},
                    "realm_metadata": {
                        "title": "Direct conversation",
                        "collaboration_role": "direct_conversation"
                    },
                    "e2ee_epoch": null
                }
            }
        });

        assert_eq!(
            store
                .update_realm_policies_from_sync(&realms)
                .expect("direct-conversation policy should persist"),
            1
        );
        let state = store.load().expect("state should load");
        let policy = state
            .realm_policies
            .get(realm_id)
            .expect("direct-conversation policy should be present");
        assert!(policy.requires_e2ee());
        assert_eq!(
            policy.group_id_for_realm(),
            "YWs6cmVhbG06MDE5MDQxMDAtMDAwMC03MDAwLTgwMDAtMDAwMDAwMDAwMDAx"
        );
        assert_eq!(policy.encryption_profile.as_deref(), Some("mls_rfc9420"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn legacy_direct_conversation_policy_derives_canonical_group_id() {
        let realm_id = "ak:realm:01904100-0000-7000-8000-000000000001";
        let policy = ArkretRealmCryptoPolicy {
            realm_id: realm_id.to_owned(),
            content_encryption_floor: ArkretContentEncryptionFloor::E2eeRequired,
            encryption_profile: Some("mls_rfc9420".to_owned()),
            mls_group_id: None,
            source: "account_subscribe_direct_conversation".to_owned(),
            updated_at: Utc::now(),
        };
        assert_eq!(
            policy.group_id_for_realm(),
            "YWs6cmVhbG06MDE5MDQxMDAtMDAwMC03MDAwLTgwMDAtMDAwMDAwMDAwMDAx"
        );
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

    /// Outbound Sidecar reply metadata: the binding plaintext built by
    /// `build_user_facing_response_metadata` must round-trip through the same
    /// MLS group used for `encrypted_content`, and the resulting envelope is a
    /// ciphertext carrier whose plaintext parses back into a valid
    /// `user_facing_response` binding.
    #[test]
    fn sidecar_reply_metadata_encrypts_and_round_trips_through_group() {
        use super::super::sidecar::{
            SidecarExchangeContext, build_user_facing_response_metadata,
            sidecar_binding_from_metadata_plaintext,
        };

        let home = temp_home("sidecar-metadata-encrypt");
        let realm_id = "ak:realm:01904100-0000-7000-8000-000000000001";
        let bob_store = FileArkretCryptoStore::for_account(&home, "c1", "bob");
        let bob_key_package = bob_store
            .ensure_mls_key_package(
                "did:webvh:z6mkfixture:bob.example",
                "ak:device:01904100-0000-7000-8000-00000000000e",
                false,
            )
            .expect("Bob KeyPackage should be stored");

        let alice = ArkretMlsIdentity::new_basic(
            Did::new("did:webvh:z6mkfixture:alice.example").unwrap(),
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006").unwrap(),
        )
        .unwrap();
        let mut alice_group = alice.create_group(realm_id.as_bytes()).unwrap();
        let add = alice_group.add_member(&bob_key_package).unwrap();
        let welcome_carrier = json!({
            "keypackage_ref": bob_key_package.keypackage_ref.as_str(),
            "claim_ref": { "claim_id": "ak:claim:sidecar-metadata" },
            "claim_envelope": { "intended_realm_id": realm_id },
            "welcome_ref": "ak:welcome:sidecar-metadata",
            "mls_group_id": add.welcome.group_id.as_str(),
            "epoch": add.welcome.epoch,
            "commit_ref": "ak:event:01904100-0000-7000-8000-0000000000aa",
            "content": serde_json::to_value(&add.welcome).unwrap()
        });
        bob_store
            .record_mls_welcome_from_value(&welcome_carrier)
            .expect("welcome carrier should persist")
            .expect("welcome should be extracted");

        // Bob joins by decrypting one inbound payload, which also seeds the
        // bootstrap record (group_state_ref) that outbound encryption needs.
        let inbound = alice_group
            .encrypt_payload(
                CONTENT_BLOCK_JSON,
                &serde_json::to_vec(&json!({"kind":"ak.content.text","body":"request"})).unwrap(),
            )
            .unwrap();
        let ArkretDecryptDetailedOutcome::Decrypted { .. } = bob_store
            .try_decrypt_content_block_detailed(&inbound)
            .expect("bob should join and decrypt")
        else {
            panic!("stored Welcome should admit bob");
        };
        let bootstrap = bob_store
            .plan_bootstrap_for_payload(
                "did:webvh:z6mkfixture:bob.example",
                "ak:device:01904100-0000-7000-8000-00000000000e",
                &inbound,
            )
            .expect("planning with local state should preserve the verified commit ref");
        assert_eq!(
            bootstrap.group_state_ref.as_deref(),
            Some("ak:event:01904100-0000-7000-8000-0000000000aa")
        );
        bob_store
            .upsert_realm_policy(ArkretRealmCryptoPolicy {
                realm_id: realm_id.to_owned(),
                content_encryption_floor: ArkretContentEncryptionFloor::E2eeRequired,
                encryption_profile: Some("mls".to_owned()),
                mls_group_id: Some(add.welcome.group_id.clone()),
                source: "test".to_owned(),
                updated_at: Utc::now(),
            })
            .expect("policy should persist");

        let context = SidecarExchangeContext {
            exchange_id: "01904100-0000-7000-8000-0000000000aa".to_owned(),
            request_event_id: "ak:event:01904100-0000-7000-8000-000000000031".to_owned(),
            coordinator_assignment_event_id: Some(
                "ak:event:01904100-0000-7000-8000-000000000031".to_owned(),
            ),
        };
        let metadata_plaintext = build_user_facing_response_metadata(&context).expect("metadata");
        let ArkretEncryptOutcome::Encrypted(encrypted_metadata) = bob_store
            .encrypt_message_metadata_for_realm(realm_id, &metadata_plaintext)
            .expect("encryption should complete")
        else {
            panic!("sidecar metadata must be encrypted, never plaintext");
        };
        let envelope_value = serde_json::to_value(encrypted_metadata.into_envelope()).unwrap();
        assert_eq!(
            envelope_value["content_type"],
            arkret::MESSAGE_METADATA_MLS_CONTENT_TYPE
        );

        // The wire envelope is ciphertext only: no binding key leaks.
        assert!(
            !serde_json::to_string(&envelope_value)
                .unwrap()
                .contains("sidecar_exchange_binding")
        );

        // Alice (same MLS group) decrypts the carrier back to the binding.
        let envelope = serde_json::from_value(envelope_value).expect("envelope shape");
        let payload =
            arkret::mls::encrypted_envelope_to_payload(&envelope).expect("payload conversion");
        let plaintext_bytes = alice_group
            .decrypt_payload(&payload)
            .expect("group member should decrypt metadata carrier");
        let plaintext: Value = serde_json::from_slice(&plaintext_bytes).expect("plaintext json");
        let binding =
            sidecar_binding_from_metadata_plaintext(&plaintext).expect("binding round-trips");
        assert_eq!(binding.exchange_id.as_str(), context.exchange_id);
        assert_eq!(
            binding.request_event_id.as_ref().map(|id| id.as_str()),
            Some(context.request_event_id.as_str())
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn online_presence_is_encrypted_signed_and_monotonic_across_refreshes() {
        let home = temp_home("presence-heartbeat");
        let realm_id = "ak:realm:01904100-0000-7000-8000-000000000001";
        let agent_id = "did:webvh:z6mkfixture:agent.example";
        let agent_device = "ak:device:01904100-0000-7000-8000-00000000000e";
        let verification_method =
            "did:webvh:z6mkfixture:agent.example#ak:device:01904100-0000-7000-8000-00000000000e";
        let seed = [42_u8; 32];
        let key_ref = crate::arkret::ArkretKeyRef::InlineSeedBase64 {
            value: base64::engine::general_purpose::STANDARD_NO_PAD.encode(seed),
        };
        let agent_store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let agent_key_package = agent_store
            .ensure_agent_mls_key_package(agent_id, agent_device, false, &key_ref)
            .expect("Agent KeyPackage should use its authorized runtime key");

        let alice = ArkretMlsIdentity::new_basic(
            Did::new("did:webvh:z6mkfixture:alice.example").unwrap(),
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006").unwrap(),
        )
        .unwrap();
        let mut alice_group = alice.create_group(realm_id.as_bytes()).unwrap();
        let add = alice_group.add_member(&agent_key_package).unwrap();
        let group_id = add.welcome.group_id.clone();
        let group_state_ref = "ak:event:01904100-0000-7000-8000-0000000000aa";
        agent_store
            .record_mls_welcome_from_value(&json!({
                "keypackage_ref": agent_key_package.keypackage_ref.as_str(),
                "claim_ref": { "claim_id": "ak:claim:presence-heartbeat" },
                "claim_envelope": { "intended_realm_id": realm_id },
                "welcome_ref": "ak:welcome:presence-heartbeat",
                "mls_group_id": group_id,
                "epoch": add.welcome.epoch,
                "commit_ref": group_state_ref,
                "content": serde_json::to_value(&add.welcome).unwrap()
            }))
            .expect("Welcome carrier should persist")
            .expect("Welcome should be extracted");

        // Consuming one ordinary MLS payload admits the Welcome and persists
        // the accepted group-state reference used by Signal exporter AEAD.
        let inbound = alice_group
            .encrypt_payload(
                CONTENT_BLOCK_JSON,
                &serde_json::to_vec(&json!({"kind":"ak.content.text","body":"ready"})).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            agent_store
                .try_decrypt_content_block_detailed(&inbound)
                .expect("Agent should join and decrypt"),
            ArkretDecryptDetailedOutcome::Decrypted { .. }
        ));
        agent_store
            .upsert_realm_policy(ArkretRealmCryptoPolicy {
                realm_id: realm_id.to_owned(),
                content_encryption_floor: ArkretContentEncryptionFloor::E2eeRequired,
                encryption_profile: Some("mls_rfc9420".to_owned()),
                mls_group_id: Some(group_id),
                source: "test".to_owned(),
                updated_at: Utc::now(),
            })
            .expect("presence Realm policy should persist");
        assert_eq!(
            agent_store.presence_ready_realm_ids().unwrap(),
            vec![realm_id.to_owned()]
        );

        let sent_at = Utc::now();
        let seal_ref = format!("ak:seal:sha256:{}", "d".repeat(64));
        let first = agent_store
            .seal_online_presence_signal(
                realm_id,
                agent_id,
                agent_device,
                verification_method,
                &key_ref,
                &seal_ref,
                sent_at,
            )
            .expect("first presence heartbeat should seal");
        let second = agent_store
            .seal_online_presence_signal(
                realm_id,
                agent_id,
                agent_device,
                verification_method,
                &key_ref,
                &seal_ref,
                sent_at + chrono::Duration::seconds(20),
            )
            .expect("second presence heartbeat should seal");

        let verifying_key = ed25519_dalek::SigningKey::from_bytes(&seed).verifying_key();
        let public_key = arkret_signatures::PublicKeyMaterial::Ed25519Raw {
            bytes: verifying_key.to_bytes().to_vec(),
        };
        arkret_signatures::verify_eddsa_signal_proof(&first, &public_key)
            .expect("first Signal proof should verify");
        arkret_signatures::verify_eddsa_signal_proof(&second, &public_key)
            .expect("second Signal proof should verify");
        assert_ne!(
            first.encrypted_payload.nonce, second.encrypted_payload.nonce,
            "each refresh must spend a distinct MLS Signal nonce"
        );

        let mut replay = arkret::AeadNonceReplayTracker::new();
        for (expected_sequence, envelope) in [(0, &first), (1, &second)] {
            let plaintext = alice_group
                .open_signal_envelope(envelope, &mut replay)
                .expect("peer should decrypt encrypted presence");
            let arkret::SignalPlaintext::Presence(presence) =
                arkret::open_signal_plaintext(&plaintext).expect("presence plaintext should open")
            else {
                panic!("Signal plaintext must be ak.presence");
            };
            assert_eq!(presence.payload_sequence, expected_sequence);
            assert_eq!(presence.actor_id.as_str(), agent_id);
            assert_eq!(presence.state, PresenceState::Online);
            assert_eq!(presence.ttl_ms, 30_000);
        }
        assert_eq!(
            agent_store
                .load()
                .unwrap()
                .signal_sequences
                .get(&format!("realm:{realm_id}")),
            Some(&2)
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
    fn revocable_pool_uses_wire_refs_instead_of_local_ids() {
        let home = temp_home("kp-revoke-refs");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let principal = "did:web:agent.example";
        let device = "ak:device:01904100-0000-7000-8000-000000000001";
        let record = store
            .ensure_mls_key_package(principal, device, false)
            .expect("KeyPackage should be created");

        let refs = store
            .revocable_keypackage_refs()
            .expect("revocable refs should load");
        assert_eq!(refs, vec![record.keypackage_ref.as_str().to_owned()]);
        assert!(!refs.contains(&record.keypackage_id));

        store
            .mark_mls_key_package_revoked(record.keypackage_ref.as_str())
            .expect("revoke marker should persist");
        assert!(
            store
                .revocable_keypackage_refs()
                .expect("revocable refs should reload")
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn legacy_pool_cleanup_is_limited_to_the_current_agent_binding() {
        let home = temp_home("kp-revoke-agent-scope");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "legacy");
        let current_principal = "did:web:current-agent.example";
        let current_device = "ak:device:01904100-0000-7000-8000-000000000011";
        let replaced_principal = "did:web:replaced-agent.example";
        let replaced_device = "ak:device:01904100-0000-7000-8000-000000000012";
        let current = store
            .ensure_mls_key_package(current_principal, current_device, false)
            .expect("current Agent KeyPackage should be created");
        let replaced = store
            .ensure_mls_key_package(replaced_principal, replaced_device, false)
            .expect("replaced Agent KeyPackage should be created");

        let refs = store
            .revocable_keypackage_refs_for_agent(current_principal, current_device)
            .expect("binding-scoped revocable refs should load");
        assert_eq!(refs, vec![current.keypackage_ref.as_str().to_owned()]);
        assert!(!refs.contains(&replaced.keypackage_ref.as_str().to_owned()));

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
            agent_key_authorize_event_id: None,
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
            group_state_ref: Some("ak:event:01904100-0000-7000-8000-0000000000aa".to_owned()),
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
            "commit_ref": expected_binding.group_state_ref.as_deref().unwrap(),
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

        // The durable Commit must be sufficient to advance a receiver that
        // has recorded its Welcome but has not yet seen any application data.
        let commit = alice_group
            .self_update_commit()
            .expect("Alice self-update Commit should build");
        let commit_event =
            EventId::new("ak:event:01904100-0000-7000-8000-0000000000ab".to_owned()).unwrap();
        let governance_binding = arkret::MlsGovernanceBindingPayload::realm(
            RealmId::new("ak:realm:01904100-0000-7000-8000-000000000001".to_owned()).unwrap(),
            commit.group_id.clone(),
            expected_binding.epoch,
            commit.epoch,
            Hash::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
            arkret::ProfileId::MLS_GOVERNANCE_BINDING_FULL_V1,
            "arkret.reducer.v1",
        )
        .unwrap();
        let commit_payload = MlsCommitPayload::new(
            expected_binding.epoch,
            expected_binding.group_state_ref.as_deref().unwrap(),
            Vec::new(),
            &commit,
            governance_binding,
        )
        .unwrap();
        assert!(
            bob_store
                .apply_mls_commit(&commit_payload, &commit_event)
                .expect("Bob should consume Welcome and apply Commit")
        );
        assert!(
            !bob_store
                .apply_mls_commit(&commit_payload, &commit_event)
                .expect("accepted Commit replay should be idempotent")
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

    #[test]
    fn durable_mls_welcome_payload_is_converted_and_persisted() {
        use std::num::NonZeroU64;

        use arkret::{
            Base64UrlString, EventId, MlsClaimTrustBinding, MlsGovernanceBindingPayload,
            MlsGroupId, MlsRequesterTrustBinding, MlsWelcomeCarrier, MlsWelcomeClaimEnvelope,
            MlsWelcomePayloadClaimRef, NonEmptyString, RealmId,
        };

        let home = temp_home("durable-welcome-payload");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let principal =
            Did::new("did:webvh:z6mkfixture:agent.example".to_owned()).expect("principal");
        let device = DeviceId::new("ak:device:01904100-0000-7000-8000-00000000000f".to_owned())
            .expect("device");
        let key_package = store
            .ensure_mls_key_package(principal.as_str(), device.as_str(), false)
            .expect("KeyPackage");

        let owner = ArkretMlsIdentity::new_basic(
            Did::new("did:webvh:z6mkfixture:owner.example".to_owned()).expect("owner"),
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006".to_owned())
                .expect("owner device"),
        )
        .expect("owner identity");
        let realm_id = RealmId::new("ak:realm:01904100-0000-7000-8000-000000000001".to_owned())
            .expect("realm");
        let mut group = owner
            .create_group(realm_id.as_str().as_bytes())
            .expect("group");
        let add = group.add_member(&key_package).expect("add member");
        let hash = |marker: char| {
            Hash::new(format!("sha256:{}", marker.to_string().repeat(64))).expect("hash")
        };
        let governance_binding = MlsGovernanceBindingPayload::realm(
            realm_id.clone(),
            add.welcome.group_id.clone(),
            0,
            add.welcome.epoch,
            hash('c'),
            arkret::ProfileId::MLS_GOVERNANCE_BINDING_FULL_V1,
            "arkret.reducer.v1",
        )
        .expect("governance binding");
        let claim_id = NonEmptyString::new("claim-agent-welcome-001").expect("claim id");
        let authorize_event = NonEmptyString::new("ak:event:01904100-0000-7000-8000-000000000011")
            .expect("authorize event");
        let claim_ref = MlsWelcomePayloadClaimRef {
            claim_id: claim_id.clone(),
            keypackage_ref: key_package.keypackage_ref.as_str().to_owned(),
            keypackage_digest: key_package.keypackage_ref.clone(),
            capabilities_digest: hash('d'),
            trust_binding: MlsClaimTrustBinding::AgentKeyAuthorizeEventId(authorize_event),
        };
        let claim_envelope = MlsWelcomeClaimEnvelope {
            keypackage_ref: key_package.keypackage_ref.as_str().to_owned(),
            keypackage_digest: key_package.keypackage_ref.clone(),
            intended_realm_id: realm_id,
            claim_id: claim_id.clone(),
            requester_did: Did::new("did:webvh:z6mkfixture:owner.example".to_owned())
                .expect("requester"),
            trust_binding: MlsRequesterTrustBinding::SskGeneration(
                NonZeroU64::new(1).expect("generation"),
            ),
            nonce: NonEmptyString::new("durable-welcome-nonce").expect("nonce"),
            welcome_digest: add.welcome.welcome_hash.clone(),
            created_at: Utc::now(),
            signature: KeyOperationSignature {
                kid: NonEmptyString::new("did:webvh:z6mkfixture:owner.example#ssk-1").expect("kid"),
                alg: Some(NonEmptyString::new("EdDSA").expect("algorithm")),
                sig: Base64UrlString::new("AQ").expect("signature"),
            },
        };
        let payload = MlsWelcomePayload {
            mls_group_id: MlsGroupId::new(add.welcome.group_id.clone()).expect("group id"),
            epoch: add.welcome.epoch,
            recipient_principal_id: principal.clone(),
            recipient_device_id: device.clone(),
            sender_device_id: None,
            keypackage_ref: key_package.keypackage_ref.as_str().to_owned(),
            keypackage_digest: key_package.keypackage_ref.clone(),
            claim_id,
            claim_ref,
            claim_envelope,
            peer_claim_receipt: None,
            carrier: MlsWelcomeCarrier::new(
                None,
                None,
                Some(NonEmptyString::new(add.welcome.welcome.clone()).expect("ciphertext")),
            )
            .expect("carrier"),
            commit_ref: None,
            governance_binding,
            expires_at: Utc::now() + chrono::Duration::hours(1),
        };

        let recorded = store
            .record_mls_welcome_from_value(
                &serde_json::to_value(&payload).expect("serialize durable payload"),
            )
            .expect("record durable payload")
            .expect("extract durable Welcome");
        let mut expected = add.welcome;
        expected.ratchet_tree = None;
        assert_eq!(recorded, expected);
        let state = store.load().expect("state");
        let mls_store = state.mls_store().expect("MLS store");
        let welcomes = mls_store.welcomes_for_device(&principal, &device);
        assert_eq!(welcomes.len(), 1);
        assert_eq!(welcomes[0], &recorded);

        let repair_realm_id = payload.claim_envelope.intended_realm_id.clone();
        let repair_actor = Did::new("did:webvh:z6mkfixture:owner.example".to_owned()).unwrap();
        let repair_scope = ScopeRef::Realm {
            realm_id: repair_realm_id.clone(),
        };
        let strand_id =
            arkret::StrandId::new("ak:strand:01904100-0000-7000-8000-000000000020".to_owned())
                .unwrap();
        let strand_payload = StrandCreatePayload {
            object: arkret::Strand::new(
                strand_id.clone(),
                repair_realm_id.clone(),
                "Direct",
                repair_actor.clone(),
            ),
            initial_relations: None,
        };
        let strand_event = arkret::Event::new_with_id_at(
            EventId::new("ak:event:01904100-0000-7000-8000-000000000021".to_owned()).unwrap(),
            "ak.strand.create",
            repair_scope.clone(),
            repair_actor.clone(),
            1,
            arkret::Hlc::new("01970e589d21-0004-a13f9c2e").unwrap(),
            serde_json::to_value(strand_payload).unwrap(),
            Utc::now(),
        )
        .unwrap();
        let welcome_event_id =
            EventId::new("ak:event:01904100-0000-7000-8000-000000000022".to_owned()).unwrap();
        let welcome_event = arkret::Event::new_with_id_at(
            welcome_event_id.clone(),
            "ak.mls.welcome",
            repair_scope,
            repair_actor,
            2,
            arkret::Hlc::new("01970e589d22-0000-a13f9c2e").unwrap(),
            serde_json::to_value(&payload).unwrap(),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(
            store
                .repair_pending_direct_conversation_bindings_from_accepted_events(&[
                    strand_event,
                    welcome_event,
                ])
                .expect("accepted history should repair pending consume context"),
            1
        );
        let repaired = store
            .pending_mls_welcome_consume_bindings()
            .expect("repaired pending bindings should load");
        assert_eq!(repaired.len(), 1);
        assert_eq!(
            repaired[0].welcome_ref.as_deref(),
            Some(welcome_event_id.as_str())
        );
        assert_eq!(repaired[0].strand_id.as_deref(), Some(strand_id.as_str()));
        let _ = std::fs::remove_dir_all(&home);
    }
}
