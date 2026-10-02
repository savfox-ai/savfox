//! Local Arkret crypto state for Savfox channel adapters.
//!
//! The SDK owns protocol records and `ArkretMlsGroup`. This module gives
//! Savfox a file-backed wrapper
//! so account-mode and applet-mode can persist decryption failures, MLS group
//! snapshots, recovery plans and realm encryption policy under `SAVFOX_HOME`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context;
use arkret::mls::{ArkretMlsGroup, ArkretMlsIdentity, ArkretMlsSigner};
use arkret::{
    AccountId, ActorId, ContentBlock, DeviceId, DidCoreId, DirectConversationBoundPayload,
    EncryptedPayload, EncryptedPayloadScheme, EventContentPreEncryptionHeader,
    EventContentRoutingContext, EventId, MessageMetadata, MlsCommitPayload, MlsEncryptedPayload,
    MlsEndpointIdentity, MlsKeyPackageRecord, MlsKeyPackageState, MlsPayloadType,
    MlsWelcomeDelivery, PresencePlaintext, PresenceState, RealmId, ScopeRef, SignalSequenceDomain,
    SignalSequenceEndpoint, TypedCurrentResult, seal_signal_plaintext,
};
use arkret_models_crypto::MlsGroupStateRecord as CurrentMlsGroupStateRecord;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{DateTime, Utc};
use parking_lot::ReentrantMutex;
#[cfg(not(test))]
use savfox_keyring_store::KeyringStore as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::signer::{ArkretKeyRef, load_ed25519_signing_key};

const STATE_VERSION: &str = "savfox.arkret.crypto_state.v1";
const WRAPPED_STATE_VERSION: &str = "savfox.arkret.crypto_state.wrapped.v1";
#[cfg(not(test))]
const WRAPPING_KEY_SERVICE: &str = "savfox-arkret-crypto-state";

/// Classification retained by Savfox for encrypted Events that cannot yet be
/// opened. Arkret no longer owns this client-local rendering queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnableToDecryptReason {
    NoSession,
    UnknownSender,
    UnknownDevice,
    MissingMessageKey,
    BadCiphertext,
    Withheld,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnableToDecryptRecord {
    pub event_id: EventId,
    pub realm_id: RealmId,
    pub sender: DidCoreId,
    pub reason: UnableToDecryptReason,
    pub encrypted_content: EncryptedPayload,
    pub first_seen_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MlsRecoveryAction {
    UseLocalState,
    ConsumeWelcome,
    RequestEpochRecovery { missing_from_epoch: u64 },
}

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
    pub fn group_id_for_realm(&self) -> anyhow::Result<String> {
        let scope = ScopeRef::Realm {
            realm_id: RealmId::new(self.realm_id.clone())?,
        };
        let canonical = scope.canonical_mls_group_id()?.to_string();
        anyhow::ensure!(
            self.mls_group_id
                .as_deref()
                .is_none_or(|declared| declared == canonical.as_str()),
            "Realm policy MLS group id differs from its canonical security scope"
        );
        Ok(canonical)
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
    pub actor_id: ActorId,
    pub endpoint: MlsEndpointIdentity,
    pub private_state: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keypackage_id: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for ArkretMlsIdentityStateRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArkretMlsIdentityStateRecord")
            .field("actor_id", &self.actor_id)
            .field("endpoint", &self.endpoint)
            .field(
                "private_state",
                &format_args!("<redacted {} bytes>", self.private_state.len()),
            )
            .field("keypackage_id", &self.keypackage_id)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_durable_receipt: Option<arkret::RecipientMlsDurableReceipt>,
    /// Complete member attribution for the epoch this Welcome joins, as
    /// derived once by the single verified admission path.
    ///
    /// The SDK refuses to export a group state record until every occupied
    /// leaf is bound ("MLS member attribution is unavailable until accepted
    /// transition bindings are installed"), and a staged Welcome replayed
    /// later - by the Commit recovery path or by the first inbound ciphertext
    /// - has no other way to recover that authority. Re-installing these
    /// re-validates each entry against the actual RFC 9420 leaf, so the
    /// record is a cache of an authority decision, never a substitute for one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified_leaf_bindings: Vec<arkret::mls::MlsVerifiedLeafBinding>,
}

impl PartialEq for ArkretMlsWelcomeConsumeBinding {
    fn eq(&self, other: &Self) -> bool {
        self.keypackage_ref == other.keypackage_ref
            && self.claim_id == other.claim_id
            && self.welcome_ref == other.welcome_ref
            && self.realm_id == other.realm_id
            && self.strand_id == other.strand_id
            && self.mls_group_id == other.mls_group_id
            && self.epoch == other.epoch
            && self.group_state_ref == other.group_state_ref
    }
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

/// Coordinates endorsed by an accepted `ak.direct_conversation.bound`.
///
/// The payload names no Welcome Event and no permanent MLS group — the binding
/// is written once for the pair and participant authority reads the *current*
/// active generation — so the Realm is what a pending Welcome consume is
/// matched on. A record written by an older build carries `welcome_ref` /
/// `mls_group_id`; those keys are simply dropped on read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArkretDirectConversationWelcomeBinding {
    pub realm_id: String,
    pub strand_id: String,
    /// Accepted `ak.direct_conversation.bound` Event that authorizes messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_event_ref: Option<String>,
    /// `generation 1` activation Event: the first exact-pair MLS generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_exact_pair_generation_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArkretCryptoStateFile {
    pub version: String,
    pub scope_id: String,
    #[serde(default)]
    pub generation: u64,
    pub unable_to_decrypt: BTreeMap<EventId, UnableToDecryptRecord>,
    /// Provider-opaque group snapshots installed from accepted Commit or
    /// recipient Welcome delivery. Keys are canonical MLS group ids.
    #[serde(default)]
    pub mls_group_states: BTreeMap<String, CurrentMlsGroupStateRecord>,
    /// Account Station current results retained at their exact MLS epochs.
    #[serde(default)]
    pub mls_station_currents: BTreeMap<String, BTreeMap<u64, arkret::MlsGroupCurrent>>,
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
    /// Last authority decision set observed on an accepted Event for each
    /// Realm. This lets ordinary authoring proceed from verified local state.
    #[serde(default)]
    pub realm_authority_refs: BTreeMap<String, Vec<EventId>>,
    #[serde(default)]
    pub bootstrap: BTreeMap<String, ArkretBootstrapRecord>,
    /// Next verified sender-endpoint sequence per Signal scope. The value is
    /// advanced and persisted before each submit so a failed HTTP request can
    /// skip but never reuse a sequence or the MLS Signal nonce consumed with it.
    #[serde(default)]
    pub signal_sequences: BTreeMap<String, u64>,
    #[serde(default)]
    pub key_backup: ArkretKeyBackupState,
}

impl ArkretCryptoStateFile {
    fn new(scope_id: String) -> anyhow::Result<Self> {
        Ok(Self {
            version: STATE_VERSION.to_owned(),
            scope_id,
            generation: 0,
            unable_to_decrypt: BTreeMap::new(),
            mls_group_states: BTreeMap::new(),
            mls_station_currents: BTreeMap::new(),
            mls_identities: BTreeMap::new(),
            mls_key_packages: BTreeMap::new(),
            mls_welcome_consume_bindings: BTreeMap::new(),
            direct_conversation_welcome_bindings: BTreeMap::new(),
            realm_policies: BTreeMap::new(),
            realm_authority_refs: BTreeMap::new(),
            bootstrap: BTreeMap::new(),
            signal_sequences: BTreeMap::new(),
            key_backup: ArkretKeyBackupState::default(),
        })
    }
}

impl arkret::MlsGroupStateSink for ArkretCryptoStateFile {
    fn put_mls_group_state(
        &mut self,
        record: CurrentMlsGroupStateRecord,
    ) -> Result<(), arkret::WireError> {
        let key = record.group_id.as_str().to_owned();
        if let Some(previous) = self.mls_group_states.get(&key)
            && (record.epoch < previous.epoch
                || record.actor_id != previous.actor_id
                || record.endpoint != previous.endpoint)
        {
            return Err(arkret::WireError::Protocol(
                "MLS group snapshot rolls back or changes its local endpoint".to_owned(),
            ));
        }
        self.mls_group_states.insert(key, record);
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

    #[cfg(not(test))]
    fn wrapping_key_account(&self) -> String {
        use sha2::Digest as _;
        format!(
            "scope-{}",
            hex::encode(sha2::Sha256::digest(self.scope_id.as_bytes()))
        )
    }

    fn wrapping_key(&self) -> anyhow::Result<[u8; 32]> {
        #[cfg(test)]
        {
            use sha2::Digest as _;
            let mut digest = sha2::Sha256::new();
            digest.update(b"savfox-arkret-unit-test-wrapping-key");
            digest.update(self.scope_id.as_bytes());
            return Ok(digest.finalize().into());
        }
        #[cfg(not(test))]
        {
            let account = self.wrapping_key_account();
            let store = super::key_store::ArkretKeyringStore;
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
        policy.group_id_for_realm()?;
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

    pub fn record_realm_authority_refs(
        &self,
        realm_id: &str,
        authority_refs: &[EventId],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !authority_refs.is_empty()
                && authority_refs.len() <= 64
                && authority_refs.windows(2).all(|pair| pair[0] < pair[1]),
            "Realm authority references must be non-empty, sorted, and unique"
        );
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        state
            .realm_authority_refs
            .insert(realm_id.to_owned(), authority_refs.to_vec());
        self.save(&mut state)
    }

    pub fn realm_authority_refs(&self, realm_id: &str) -> anyhow::Result<Option<Vec<EventId>>> {
        Ok(self.load()?.realm_authority_refs.get(realm_id).cloned())
    }

    /// Realm ids for which the account has both an E2EE policy and mutable MLS
    /// state accepted at a known group-state reference. Only these scopes can
    /// safely advertise encrypted v1 presence.
    pub fn presence_ready_realm_ids(&self) -> anyhow::Result<Vec<String>> {
        let state = self.load()?;
        let mut realms = Vec::new();
        for policy in state
            .realm_policies
            .values()
            .filter(|policy| policy.requires_e2ee())
        {
            let group_id = policy.group_id_for_realm()?;
            let Some(record) = state.mls_group_states.get(&group_id) else {
                continue;
            };
            if group_state_ref_for_epoch(&state, &group_id, record.epoch).is_some() {
                realms.push(policy.realm_id.clone());
            }
        }
        realms.sort();
        realms.dedup();
        Ok(realms)
    }

    /// Encrypt and sign one Realm-scoped `ak.presence` Signal.
    ///
    /// The post-seal MLS state and strictly increasing payload sequence are
    /// persisted before the caller performs HTTP submit. This intentionally
    /// burns both values on an uncertain request and prevents replay/nonce
    /// reuse after a crash.
    #[allow(clippy::too_many_arguments)]
    pub fn seal_online_presence_signal(
        &self,
        realm_id: &str,
        actor_account_id: &arkret::AccountId,
        verification_method: &str,
        key_ref: &ArkretKeyRef,
        authority_head: &arkret::CommitStreamHead,
        sent_at: DateTime<Utc>,
    ) -> anyhow::Result<arkret_wire::SignalEnvelope> {
        let realm_id = RealmId::new(realm_id.to_owned())?;
        let scope_ref = ScopeRef::Realm {
            realm_id: realm_id.clone(),
        };
        anyhow::ensure!(
            authority_head.stream_ref
                == arkret::CommitStreamRef::Realm {
                    realm_id: realm_id.clone()
                },
            "presence authority head belongs to another independent stream"
        );
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let policy = state
            .realm_policies
            .get(realm_id.as_str())
            .filter(|policy| policy.requires_e2ee())
            .cloned()
            .with_context(|| format!("Realm '{realm_id}' has no E2EE Signal policy"))?;
        let group_id = policy.group_id_for_realm()?;
        let record = state
            .mls_group_states
            .get(&group_id)
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
        actor_account_id.validate()?;
        let actor_id = ActorId::account(actor_account_id.clone());
        let expires_at = sent_at + chrono::Duration::seconds(30);
        let signing_key = load_ed25519_signing_key(key_ref)?;
        let public_key_digest = arkret_signatures::PublicKeyMaterial::Ed25519Raw {
            bytes: signing_key.verifying_key().to_bytes().to_vec(),
        }
        .raw_ed25519_digest()?;
        let sequence_key = SignalSequenceDomain {
            sender_actor_id: actor_id.clone(),
            endpoint: SignalSequenceEndpoint::AgentKey { public_key_digest },
            scope_ref: scope_ref.clone(),
        }
        .canonical_key()?;
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
            group_state_ref: group_state_ref.clone(),
        };
        let mut group = ArkretMlsGroup::restore_from_state_record(&record)
            .map_err(|error| anyhow::anyhow!("restore Arkret MLS group: {error}"))?;
        let binding = arkret_wire::SignalAeadBinding {
            realm_id: &realm_id,
            scope_ref: &scope_ref,
            sender_actor_id: &actor_id,
            sender_device_id: None,
            authority_commit_id: &authority_head.commit_id,
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
            .encrypt_signal_payload(&binding, &plaintext)
            .map_err(|error| anyhow::anyhow!("seal Arkret presence Signal: {error}"))?;

        let updated = group
            .persist_state(&mut state)
            .map_err(|error| anyhow::anyhow!("persist post-Signal MLS group: {error}"))?;
        let next_sequence = payload_sequence
            .checked_add(1)
            .context("Arkret presence payload sequence exhausted")?;
        state.signal_sequences.insert(sequence_key, next_sequence);
        state.bootstrap.insert(
            updated.group_id.to_string(),
            ArkretBootstrapRecord {
                group_id: updated.group_id.to_string(),
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
            sender_device_id: None,
            authority_commit_id: authority_head.commit_id.clone(),
            signal_class: arkret_wire::SignalClass::Session,
            sent_at,
            expires_at,
            encrypted_payload: sealed.encrypted_payload,
            proof: arkret_wire::SignalProof {
                kind: arkret::proof_kind::DETACHED_JWS.to_owned(),
                verification_method,
                envelope_digest: arkret::Hash::new(format!("sha256:{}", "0".repeat(64)))?,
                domain: None,
                audience: None,
                jws: String::new(),
            },
        };
        envelope.proof.envelope_digest = envelope.envelope_digest()?;
        let proof_bytes = envelope.proof_binding_bytes()?;
        envelope.proof.jws =
            arkret_signatures::sign_ed25519_detached_jws(&signing_key, &proof_bytes)?;
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
                policy.group_id_for_realm()?;
                state.realm_policies.insert(policy.realm_id.clone(), policy);
                updated += 1;
            }
        }
        if updated > 0 {
            self.save(&mut state)?;
        }
        Ok(updated)
    }

    /// Author one ordinary (single-use) KeyPackage. The Arkret SDK no longer
    /// authors last-resort KeyPackages, so this endpoint has no variant for
    /// them; `MlsKeyPackageRecord::last_resort` still carries the wire flag of
    /// packages received from a peer.
    pub fn ensure_mls_key_package(
        &self,
        account_id: &AccountId,
        device_id: &str,
    ) -> anyhow::Result<MlsKeyPackageRecord> {
        account_id.validate()?;
        let endpoint = MlsEndpointIdentity::human_device(
            account_id.principal_id.clone(),
            DeviceId::new(device_id.to_owned())?,
        );
        self.ensure_mls_key_package_inner(ActorId::account(account_id.clone()), endpoint, None)
    }

    pub fn ensure_agent_mls_key_package(
        &self,
        account_id: &AccountId,
        key_ref: &super::signer::ArkretKeyRef,
        verification_method: &str,
        authorized_event_ref: &str,
    ) -> anyhow::Result<MlsKeyPackageRecord> {
        account_id.validate()?;
        let signing_seed = super::signer::load_seed_array(key_ref)?;
        let endpoint = agent_mls_endpoint(
            account_id.principal_id.as_str(),
            verification_method,
            authorized_event_ref,
        )?;
        self.ensure_mls_key_package_inner(
            ActorId::account(account_id.clone()),
            endpoint,
            Some(signing_seed),
        )
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
        account_id: &AccountId,
        count: usize,
        key_ref: &super::signer::ArkretKeyRef,
        verification_method: &str,
        authorized_event_ref: &str,
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
        account_id.validate()?;
        let actor_id = ActorId::account(account_id.clone());
        let expected_endpoint = agent_mls_endpoint(
            account_id.principal_id.as_str(),
            verification_method,
            authorized_event_ref,
        )?;
        let identity_key = mls_identity_key(&actor_id, &expected_endpoint)?;
        let identity_record = state.mls_identities.get(&identity_key).ok_or_else(|| {
            anyhow::anyhow!("Agent MLS identity must be initialized before pool replenishment")
        })?;
        let identity = restore_mls_identity(identity_record)?;
        anyhow::ensure!(
            mls_identity_signature_public_key(&identity)? == expected_signature_key,
            "Agent MLS identity does not match the currently authorized runtime key"
        );
        anyhow::ensure!(
            identity.endpoint_identity() == expected_endpoint,
            "Agent MLS identity does not match the currently authorized runtime endpoint"
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
                actor_id,
                endpoint: expected_endpoint,
                private_state,
                keypackage_id: records.last().map(|record| record.keypackage_id.clone()),
                updated_at: Utc::now(),
            },
        );
        self.save(&mut state)?;
        Ok(records)
    }

    fn ensure_mls_key_package_inner(
        &self,
        actor_id: ActorId,
        endpoint: MlsEndpointIdentity,
        mut signing_seed: Option<[u8; 32]>,
    ) -> anyhow::Result<MlsKeyPackageRecord> {
        use zeroize::Zeroize as _;

        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        actor_id.validate()?;
        endpoint.validate()?;
        anyhow::ensure!(
            actor_id.signing_principal_id() == endpoint.principal_id(),
            "MLS ActorId differs from its endpoint principal"
        );
        let identity_key = mls_identity_key(&actor_id, &endpoint)?;
        let cache_key = format!("{identity_key}#single_use");

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
            identity.endpoint_identity() == endpoint
                && expected_signature_key.as_ref().is_none_or(|expected| {
                    mls_identity_signature_public_key(identity)
                        .is_ok_and(|actual| actual.as_slice() == expected.as_slice())
                })
        });

        if restored_matches_authorization
            && let Some(record) = state.mls_key_packages.get(&cache_key).cloned()
            && local_key_package_can_be_published(&record)
        {
            if let Some(seed) = signing_seed.as_mut() {
                seed.zeroize();
            }
            return Ok(record);
        }

        let identity = if let Some(identity) = restored.filter(|identity| {
            identity.endpoint_identity() == endpoint
                && expected_signature_key.as_ref().is_none_or(|expected| {
                    mls_identity_signature_public_key(identity)
                        .is_ok_and(|actual| actual.as_slice() == expected.as_slice())
                })
        }) {
            if let Some(seed) = signing_seed.as_mut() {
                seed.zeroize();
            }
            identity
        } else {
            new_mls_identity(actor_id.clone(), endpoint.clone(), signing_seed.take())
                .map_err(|err| anyhow::anyhow!("create Arkret MLS identity: {err}"))?
        };
        let record = identity
            .key_package_record()
            .map_err(|err| anyhow::anyhow!("create Arkret MLS KeyPackage: {err}"))?;
        let private_state = identity
            .export_private_state()
            .map_err(|err| anyhow::anyhow!("export Arkret MLS identity state: {err}"))?;

        state.mls_key_packages.insert(cache_key, record.clone());
        state.mls_identities.insert(
            identity_key,
            ArkretMlsIdentityStateRecord {
                actor_id,
                endpoint,
                private_state,
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

    /// Estimate the refill needed from Savfox's client-private inventory.
    /// Upload responses intentionally no longer expose server inventory.
    pub fn mls_key_package_maintenance_deficit(
        &self,
        endpoint: &MlsEndpointIdentity,
        low_water: usize,
    ) -> anyhow::Result<usize> {
        let state = self.load()?;
        let now = Utc::now();
        let usable = state
            .mls_key_packages
            .values()
            .filter(|record| {
                &record.endpoint == endpoint
                    && !record.last_resort
                    && record.state == MlsKeyPackageState::Published
                    && record.expires_at.is_none_or(|expires_at| expires_at > now)
            })
            .count();
        Ok(low_water.saturating_sub(usable))
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
    /// Agent MUST revoke only its own endpoint rows; attempting to
    /// revoke another Agent's rows both fails authorization and leaves the
    /// current runtime exposed to claims for private material it no longer
    /// opens.
    pub fn revocable_keypackage_refs_for_agent(
        &self,
        principal_id: &str,
        _device_id: &str,
    ) -> anyhow::Result<Vec<String>> {
        let principal = DidCoreId::new(principal_id.to_owned())
            .with_context(|| format!("invalid Arkret principal DID '{principal_id}'"))?;
        let state = self.load()?;
        Ok(Self::revocable_keypackage_refs_matching(
            &state,
            Some(&principal),
            None,
        ))
    }

    fn revocable_keypackage_refs_matching(
        state: &ArkretCryptoStateFile,
        principal_id: Option<&DidCoreId>,
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
            if principal_id
                .is_some_and(|principal| record.actor_id.signing_principal_id() != principal)
                || device_id.is_some_and(|device| {
                    endpoint_human_device_id(&record.endpoint) != Some(device)
                })
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

    /// Whether this accepted Commit advances the durable local group and needs
    /// the Station's accepted post-transition leaf authority. An already
    /// installed epoch needs no new attribution.
    pub fn mls_commit_needs_accepted_leaf_authority(
        &self,
        payload: &MlsCommitPayload,
    ) -> anyhow::Result<bool> {
        let state = self.load()?;
        let group_id = payload.mls_group_id()?;
        Ok(state
            .mls_group_states
            .get(group_id.as_str())
            .is_none_or(|record| record.epoch < payload.next_epoch()))
    }

    /// Retain the Account Station's typed MLS current values at their exact
    /// epochs. A later accepted Commit may use only the entry for its base
    /// epoch; a newer current result is never substituted for that base.
    pub fn record_station_mls_currents(
        &self,
        current: &arkret::AccountCurrentResult,
    ) -> anyhow::Result<usize> {
        current.validate()?;
        let mut accepted = Vec::new();
        for entry in &current.entries {
            let TypedCurrentResult::Value {
                selector: arkret::CurrentSelector::MlsGroup { scope_ref },
                source_stream_ref,
                value,
                ..
            } = entry
            else {
                continue;
            };
            anyhow::ensure!(
                scope_ref.realm_id_opt() == Some(&current.realm_id)
                    && source_stream_ref == &arkret::CommitStreamRef::from_scope(scope_ref, None)?,
                "Station MLS current result has a mismatched Realm or stream"
            );
            let group: arkret::MlsGroupCurrent = serde_json::from_value(value.clone())?;
            anyhow::ensure!(
                group.effective_scope == *scope_ref,
                "Station MLS current value differs from its selector"
            );
            let group_id = scope_ref.canonical_mls_group_id()?;
            accepted.push((group_id.as_str().to_owned(), group));
        }
        if accepted.is_empty() {
            return Ok(0);
        }
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        for (group_id, group) in &accepted {
            state
                .mls_station_currents
                .entry(group_id.clone())
                .or_default()
                .insert(group.epoch, group.clone());
        }
        self.save(&mut state)?;
        Ok(accepted.len())
    }

    pub fn station_mls_current_for_scope_epoch(
        &self,
        scope: &ScopeRef,
        epoch: u64,
    ) -> anyhow::Result<Option<arkret::MlsGroupCurrent>> {
        let group_id = scope.canonical_mls_group_id()?;
        Ok(self
            .load()?
            .mls_station_currents
            .get(group_id.as_str())
            .and_then(|epochs| epochs.get(&epoch))
            .cloned())
    }

    /// Install a Commit only from its full accepted view on the scope's own
    /// stream. `station_base` is the pinned current result for the installed
    /// base epoch; `new_authority` comes from checked claims for newly occupied
    /// leaves, while unchanged leaves retain their prior verified bindings.
    pub fn install_accepted_mls_commit(
        &self,
        accepted: &arkret::CommittedEventFullView,
        station_base: &arkret::MlsGroupCurrent,
        new_authority: &[super::mls_leaf_authority::VerifiedMlsLeafAuthority],
    ) -> anyhow::Result<bool> {
        accepted.validate_shape()?;
        anyhow::ensure!(
            accepted.event.kind == arkret::EventKind::MlsCommit,
            "accepted MLS transition is not a Commit"
        );
        let payload: MlsCommitPayload = serde_json::from_value(serde_json::Value::Object(
            accepted.event.payload.clone().into_iter().collect(),
        ))?;
        payload.validate()?;
        let group_id = payload.mls_group_id()?;
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let record = state
            .mls_group_states
            .get(group_id.as_str())
            .with_context(|| {
                format!("accepted MLS Commit for group '{group_id}' has no local base state")
            })?;
        if record.epoch >= payload.next_epoch() {
            return Ok(false);
        }
        anyhow::ensure!(
            record.epoch == payload.base_epoch() && record.epoch == station_base.epoch,
            "accepted MLS Commit base epoch differs from local and Station state"
        );
        let mut group = ArkretMlsGroup::restore_from_state_record(record)?;
        let previous = group.verified_leaf_bindings()?;
        let applied_epoch = group.install_accepted_commit(accepted, station_base)?;
        anyhow::ensure!(
            applied_epoch == payload.next_epoch(),
            "accepted MLS Commit produced an unexpected epoch"
        );
        super::mls_leaf_authority::install_verified_leaf_authority(
            &mut group,
            &previous,
            new_authority,
        )?;
        let updated = group.persist_state(&mut state)?;
        state.bootstrap.insert(
            updated.group_id.as_str().to_owned(),
            ArkretBootstrapRecord {
                group_id: updated.group_id.as_str().to_owned(),
                required_epoch: updated.epoch,
                local_epoch: Some(updated.epoch),
                group_state_ref: Some(accepted.event.event_id.to_string()),
                action: MlsRecoveryAction::UseLocalState,
                updated_at: Utc::now(),
            },
        );
        self.save(&mut state)?;
        Ok(true)
    }

    /// Join from a recipient delivery whose producer proof was verified by
    /// the governance Station at enqueue, after the host independently
    /// verified the own-Station claim receipt. It checks the claim against the
    /// exact accepted Commit before decrypting the Welcome, then persists the
    /// joined group and complete roster,
    /// then signs the recipient receipt only across that durable barrier.
    #[allow(clippy::too_many_arguments)]
    pub fn install_accepted_mls_welcome(
        &self,
        delivery: &MlsWelcomeDelivery,
        accepted_commit: &arkret::CommittedEventFullView,
        claim_outcome: &arkret::KeyPackagesClaimOutcome,
        local_station: &DidCoreId,
        endpoint_authorization: &EventId,
        recipient: arkret::RecipientMlsDurableSigner,
        other_new_authority: &[super::mls_leaf_authority::VerifiedMlsLeafAuthority],
    ) -> anyhow::Result<bool> {
        let claim = claim_outcome
            .claims
            .iter()
            .find(|claim| claim.claim_id == delivery.keypackage_claim_ref.as_str())
            .context("MLS Welcome has no own-Station KeyPackage claim")?;
        let claimed_record = mls_key_package_record_from_claim(claim)?;
        let recipient_matches = match (&claimed_record.endpoint, &recipient) {
            (
                MlsEndpointIdentity::HumanDevice {
                    principal_id,
                    device_id,
                },
                arkret::RecipientMlsDurableSigner::Device {
                    recipient_account_id,
                    recipient_device_id,
                    ..
                },
            ) => {
                &recipient_account_id.principal_id == principal_id
                    && &recipient_account_id.station_id == local_station
                    && recipient_device_id == device_id
            }
            (
                MlsEndpointIdentity::AgentRuntime {
                    agent_id,
                    verification_method,
                    agent_key_authorize_event_id,
                },
                arkret::RecipientMlsDurableSigner::Agent {
                    recipient_agent_id,
                    recipient_agent_verification_method,
                    agent_key_authorize_event_id: receipt_authorization,
                },
            ) => {
                recipient_agent_id == agent_id
                    && recipient_agent_verification_method == verification_method
                    && receipt_authorization == agent_key_authorize_event_id
            }
            _ => false,
        };
        anyhow::ensure!(
            recipient_matches,
            "MLS Welcome durable signer differs from the claimed local endpoint"
        );
        let recipient_authority = super::mls_leaf_authority::verified_welcome_leaf_authority(
            delivery,
            accepted_commit,
            &claimed_record.endpoint,
            claim_outcome,
            local_station,
            endpoint_authorization,
        )?;
        let payload: MlsCommitPayload = serde_json::from_value(serde_json::Value::Object(
            accepted_commit.event.payload.clone().into_iter().collect(),
        ))?;
        payload.validate()?;
        let group_id = payload.mls_group_id()?;
        let identity_key = mls_identity_key(&claimed_record.actor_id, &claimed_record.endpoint)?;
        let binding_key = format!(
            "{}#{}#{}#{}",
            group_id,
            payload.next_epoch(),
            claim.keypackage_ref,
            delivery.keypackage_claim_ref
        );

        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let local_package_key =
            find_mls_key_package_cache_key(&state.mls_key_packages, &claim.keypackage_ref)
                .context("MLS Welcome claimed KeyPackage is not held locally")?;
        let local_package = &state.mls_key_packages[&local_package_key];
        anyhow::ensure!(
            local_package.keypackage == claimed_record.keypackage
                && local_package.actor_id == claimed_record.actor_id
                && local_package.endpoint == claimed_record.endpoint,
            "MLS Welcome claim differs from the locally held KeyPackage"
        );
        if let Some(record) = state.mls_group_states.get(group_id.as_str()) {
            anyhow::ensure!(
                record.epoch >= payload.next_epoch(),
                "MLS Welcome cannot replace an installed earlier epoch; apply accepted Commits"
            );
        }
        let identity_record = state
            .mls_identities
            .get(&identity_key)
            .context("MLS Welcome recipient private identity is unavailable")?;
        let identity = restore_mls_identity(identity_record)?;
        let already_joined = state
            .mls_group_states
            .get(group_id.as_str())
            .is_some_and(|record| record.epoch >= payload.next_epoch());
        if !already_joined {
            let mut group = ArkretMlsGroup::join_from_verified_welcome_delivery(
                identity,
                delivery,
                accepted_commit,
            )?;
            let mut authority = other_new_authority.to_vec();
            authority.push(recipient_authority);
            super::mls_leaf_authority::install_verified_leaf_authority(
                &mut group,
                &[],
                &authority,
            )?;
            let updated = group.persist_state(&mut state)?;
            let mut binding = ArkretMlsWelcomeConsumeBinding {
                keypackage_ref: claim.keypackage_ref.clone(),
                claim_id: delivery.keypackage_claim_ref.to_string(),
                welcome_ref: Some(delivery.welcome_id.to_string()),
                realm_id: Some(delivery.realm_id.to_string()),
                strand_id: None,
                mls_group_id: updated.group_id.as_str().to_owned(),
                epoch: updated.epoch,
                group_state_ref: Some(accepted_commit.event.event_id.to_string()),
                recipient_durable_receipt: None,
                verified_leaf_bindings: group.verified_leaf_bindings()?,
            };
            enrich_mls_welcome_consume_binding(
                &mut binding,
                &state.direct_conversation_welcome_bindings,
            );
            state
                .mls_welcome_consume_bindings
                .insert(binding_key.clone(), binding);
            let local_package = state
                .mls_key_packages
                .get_mut(&local_package_key)
                .expect("local MLS KeyPackage checked above");
            if !matches!(
                local_package.state,
                MlsKeyPackageState::Consumed | MlsKeyPackageState::Revoked
            ) {
                local_package.state = MlsKeyPackageState::Claimed;
                local_package.claim_id = Some(delivery.keypackage_claim_ref.to_string());
            }
            state.bootstrap.insert(
                updated.group_id.as_str().to_owned(),
                ArkretBootstrapRecord {
                    group_id: updated.group_id.as_str().to_owned(),
                    required_epoch: updated.epoch,
                    local_epoch: Some(updated.epoch),
                    group_state_ref: Some(accepted_commit.event.event_id.to_string()),
                    action: MlsRecoveryAction::ConsumeWelcome,
                    updated_at: Utc::now(),
                },
            );
            self.save(&mut state)?;
        }

        // A previous attempt may have crossed the group-state barrier and
        // failed while saving its receipt. Replaying the same delivery repairs
        // that pending receipt without joining the group a second time.
        let mut durable = self.load()?;
        let binding = durable
            .mls_welcome_consume_bindings
            .get(&binding_key)
            .context("MLS Welcome has no durable consume binding")?;
        anyhow::ensure!(
            binding.welcome_ref.as_deref() == Some(delivery.welcome_id.as_str())
                && binding.group_state_ref.as_deref()
                    == Some(accepted_commit.event.event_id.as_str()),
            "MLS Welcome durable binding names another delivery or Commit"
        );
        if binding.recipient_durable_receipt.is_some() {
            return Ok(!already_joined);
        }
        anyhow::ensure!(
            durable
                .mls_group_states
                .get(group_id.as_str())
                .is_some_and(|record| record.epoch >= payload.next_epoch()),
            "MLS Welcome group state is not durable"
        );
        let identity = restore_mls_identity(
            durable
                .mls_identities
                .get(&identity_key)
                .context("durable MLS Welcome recipient identity is unavailable")?,
        )?;
        let verification_method = match &recipient {
            arkret::RecipientMlsDurableSigner::Agent {
                recipient_agent_verification_method,
                ..
            } => recipient_agent_verification_method,
            arkret::RecipientMlsDurableSigner::Device {
                device_verification_method,
                ..
            } => device_verification_method,
            arkret::RecipientMlsDurableSigner::MinimalMetadataPairwise { .. } => {
                anyhow::bail!("pairwise endpoint cannot consume an ordinary MLS Welcome")
            }
        }
        .clone();
        let receipt = arkret::RecipientMlsDurableReceipt {
            domain: arkret::NonEmptyString::new("ak.mls.recipient_durable_receipt.v1")
                .map_err(anyhow::Error::msg)?,
            claim_request_id: claim_outcome.claim_request_id.clone(),
            key_package_ref: arkret::NonEmptyString::new(&claim.keypackage_ref)
                .map_err(anyhow::Error::msg)?,
            recipient,
            recipient_id: local_station.clone(),
            realm_id: delivery.realm_id.clone(),
            mls_group_id: group_id,
            mls_epoch: payload.next_epoch(),
            welcome_ref: delivery.welcome_id.clone(),
            welcome_digest: delivery.durable_receipt_digest()?,
            durable_at: Utc::now(),
            signature: arkret::KeyOperationSignature {
                kid: arkret::NonEmptyString::new(verification_method.as_str())
                    .map_err(anyhow::Error::msg)?,
                signature_algorithm: None,
                sig: arkret::Base64UrlString::new("AA").map_err(anyhow::Error::msg)?,
            },
        };
        let receipt = identity.sign_recipient_mls_durable_receipt(receipt)?;
        durable
            .mls_welcome_consume_bindings
            .get_mut(&binding_key)
            .expect("durable MLS Welcome binding checked above")
            .recipient_durable_receipt = Some(receipt);
        self.save(&mut durable)?;
        Ok(!already_joined)
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
        for (payload, binding_event_ref) in &payloads {
            let binding = ArkretDirectConversationWelcomeBinding {
                realm_id: payload.realm_id.to_string(),
                strand_id: payload.main_strand_id.to_string(),
                binding_event_ref: binding_event_ref.clone(),
                initial_exact_pair_generation_ref: Some(
                    payload.initial_exact_pair_group_state_ref.to_string(),
                ),
            };
            // One binding per Realm: drop any record an older build keyed by the
            // Welcome Event so the same pair is never described twice.
            state
                .direct_conversation_welcome_bindings
                .retain(|_, existing| existing.realm_id != binding.realm_id);
            state
                .direct_conversation_welcome_bindings
                .insert(binding.realm_id.clone(), binding);
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

    pub fn direct_conversation_binding_event_ref(
        &self,
        realm_id: &str,
    ) -> anyhow::Result<Option<EventId>> {
        self.load()?
            .direct_conversation_welcome_bindings
            .get(realm_id)
            .and_then(|binding| binding.binding_event_ref.as_deref())
            .map(|reference| {
                EventId::new(reference.to_owned()).map_err(|error| {
                    anyhow::anyhow!("stored Direct Conversation binding is invalid: {error}")
                })
            })
            .transpose()
    }

    pub fn realm_is_direct_conversation(&self, realm_id: &str) -> anyhow::Result<bool> {
        Ok(self
            .load()?
            .realm_policies
            .get(realm_id)
            .is_some_and(|policy| policy.source == "account_subscribe_direct_conversation"))
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

    pub fn plan_bootstrap_for_payload(
        &self,
        principal_id: &str,
        device_id: &str,
        payload: &EncryptedPayload,
    ) -> anyhow::Result<ArkretBootstrapRecord> {
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let _principal = DidCoreId::new(principal_id.to_owned())
            .with_context(|| format!("invalid Arkret principal DID '{principal_id}'"))?;
        let _device = DeviceId::new(device_id.to_owned())
            .with_context(|| format!("invalid Arkret device id '{device_id}'"))?;
        let local_epoch = state
            .mls_group_states
            .get(payload.group_id.as_str())
            .map(|record| record.epoch);
        let action = match local_epoch {
            Some(epoch) if epoch >= payload.epoch => MlsRecoveryAction::UseLocalState,
            Some(epoch) => MlsRecoveryAction::RequestEpochRecovery {
                missing_from_epoch: epoch.saturating_add(1),
            },
            None => MlsRecoveryAction::RequestEpochRecovery {
                missing_from_epoch: payload.epoch,
            },
        };
        let group_state_ref =
            group_state_ref_for_epoch(&state, payload.group_id.as_str(), payload.epoch);
        let record = ArkretBootstrapRecord {
            group_id: payload.group_id.to_string(),
            required_epoch: payload.epoch,
            local_epoch,
            group_state_ref,
            action,
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
            sender: DidCoreId::new(sender.to_owned())
                .with_context(|| format!("invalid Arkret sender DID '{sender}'"))?,
            reason,
            encrypted_content,
            first_seen_at: Utc::now(),
        };
        state
            .unable_to_decrypt
            .insert(record.event_id.clone(), record);
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
        let Some(record) = state
            .mls_group_states
            .get(payload.group_id.as_str())
            .cloned()
        else {
            return Ok(ArkretDecryptDetailedOutcome::MissingGroupState);
        };
        let mut group = ArkretMlsGroup::restore_from_state_record(&record)
            .map_err(|err| anyhow::anyhow!("restore Arkret MLS group: {err}"))?;
        let plaintext = group
            .decrypt_payload(payload)
            .map_err(|err| anyhow::anyhow!("decrypt Arkret MLS payload: {err}"))?;
        let content = serde_json::from_slice(&plaintext)
            .with_context(|| "decrypted Arkret content block is not JSON")?;
        let updated = group
            .persist_state(&mut state)
            .map_err(|err| anyhow::anyhow!("persist Arkret MLS group: {err}"))?;
        let group_state_ref =
            group_state_ref_for_epoch(&state, payload.group_id.as_str(), updated.epoch);
        state.bootstrap.insert(
            updated.group_id.to_string(),
            ArkretBootstrapRecord {
                group_id: updated.group_id.to_string(),
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
                binding.mls_group_id == payload.group_id.as_str() && binding.epoch <= updated.epoch
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
        let group_id = policy.group_id_for_realm()?;
        let Some(record) = state.mls_group_states.get(&group_id).cloned() else {
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
        let effective_scope = ScopeRef::Realm {
            realm_id: RealmId::new(realm_id.to_owned())
                .with_context(|| format!("invalid Arkret realm id '{realm_id}'"))?,
        };
        let group_state_ref = EventId::new(group_state_ref).map_err(|error| {
            anyhow::anyhow!("invalid Arkret MLS group_state_ref for encryption: {error}")
        })?;
        let sender_domain = group
            .local_content_sender_domain()
            .map_err(|err| anyhow::anyhow!("resolve Arkret MLS sender domain: {err}"))?;
        let header = EventContentPreEncryptionHeader::reconstruct(
            "1.0",
            T::MLS_CONTENT_TYPE,
            EncryptedPayloadScheme::MlsRfc9420,
            effective_scope,
            "ak.message.create",
            record.epoch,
            group_state_ref.clone(),
            sender_domain,
            EventContentRoutingContext::None,
        )
        .map_err(|err| anyhow::anyhow!("build Arkret pre-encryption header: {err}"))?;
        let plaintext = serde_json::to_vec(plaintext_value)?;
        let payload = group
            .encrypt_payload(header, &plaintext)
            .map_err(|err| anyhow::anyhow!("encrypt Arkret MLS payload: {err}"))?;
        let envelope = arkret::mls::encrypted_envelope_from_payload(&payload)
            .map_err(|err| anyhow::anyhow!("build Arkret encrypted envelope: {err}"))?;
        let envelope = MlsEncryptedPayload::<T>::new(envelope)
            .map_err(|err| anyhow::anyhow!("type Arkret MLS payload: {err}"))?;
        let updated = group
            .persist_state(&mut state)
            .map_err(|err| anyhow::anyhow!("persist Arkret MLS group: {err}"))?;
        state.bootstrap.insert(
            updated.group_id.to_string(),
            ArkretBootstrapRecord {
                group_id: updated.group_id.to_string(),
                required_epoch: updated.epoch,
                local_epoch: Some(updated.epoch),
                group_state_ref: Some(group_state_ref.to_string()),
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

fn agent_mls_endpoint(
    principal_id: &str,
    verification_method: &str,
    authorized_event_ref: &str,
) -> anyhow::Result<MlsEndpointIdentity> {
    MlsEndpointIdentity::agent_runtime(
        DidCoreId::new(principal_id.to_owned())
            .with_context(|| format!("invalid Arkret Agent DID '{principal_id}'"))?,
        arkret::DidUrl::new(verification_method.to_owned()).map_err(|error| {
            anyhow::anyhow!(
                "invalid Arkret Agent verification method '{verification_method}': {error}"
            )
        })?,
        EventId::new(authorized_event_ref.to_owned()).with_context(|| {
            format!("invalid Arkret Agent authorize Event id '{authorized_event_ref}'")
        })?,
    )
    .map_err(anyhow::Error::from)
}

fn new_mls_identity(
    actor_id: ActorId,
    endpoint: MlsEndpointIdentity,
    signing_seed: Option<[u8; 32]>,
) -> anyhow::Result<ArkretMlsIdentity> {
    use zeroize::Zeroize as _;

    let mut signing_seed = signing_seed.unwrap_or_else(rand::random);
    let signer = ArkretMlsSigner::from_ed25519_signing_key(ed25519_dalek::SigningKey::from_bytes(
        &signing_seed,
    ));
    signing_seed.zeroize();

    actor_id.validate()?;
    endpoint.validate()?;
    anyhow::ensure!(
        actor_id.signing_principal_id() == endpoint.principal_id(),
        "MLS ActorId differs from its endpoint principal"
    );
    match endpoint {
        MlsEndpointIdentity::HumanDevice { device_id, .. } => {
            ArkretMlsIdentity::new_human_device(actor_id, device_id, signer)
        }
        MlsEndpointIdentity::AgentRuntime {
            verification_method,
            agent_key_authorize_event_id,
            ..
        } => ArkretMlsIdentity::new_agent(
            actor_id,
            verification_method,
            agent_key_authorize_event_id,
            signer,
        ),
        MlsEndpointIdentity::MinimalMetadataPairwise {
            verification_method,
            ..
        } => {
            ArkretMlsIdentity::new_minimal_metadata_pairwise(actor_id, verification_method, signer)
        }
    }
    .map_err(anyhow::Error::from)
}

#[cfg(test)]
fn test_account(principal_id: &str) -> AccountId {
    AccountId::new(
        DidCoreId::new(principal_id.to_owned()).unwrap(),
        DidCoreId::new("ak:did_core:web:station.example").unwrap(),
    )
}

#[cfg(test)]
fn new_human_mls_identity(
    principal_id: DidCoreId,
    device_id: DeviceId,
) -> anyhow::Result<ArkretMlsIdentity> {
    new_mls_identity(
        ActorId::account(test_account(principal_id.as_str())),
        MlsEndpointIdentity::human_device(principal_id, device_id),
        None,
    )
}

pub fn mls_key_package_record_from_claim(
    claim: &arkret::KeyPackageClaimRecord,
) -> anyhow::Result<MlsKeyPackageRecord> {
    claim.actor_id.validate()?;
    anyhow::ensure!(
        claim.actor_id.signing_principal_id() == &claim.principal_id,
        "KeyPackage claim ActorId does not bind its principal"
    );
    let endpoint = match (
        &claim.device_id,
        &claim.agent_id,
        &claim.agent_verification_method,
        &claim.agent_key_authorize_event_id,
    ) {
        (Some(device_id), None, None, None) => {
            MlsEndpointIdentity::human_device(claim.principal_id.clone(), device_id.clone())
        }
        (None, Some(agent_id), Some(method), Some(authorization_ref)) => {
            if agent_id != &claim.principal_id {
                anyhow::bail!("Agent KeyPackage claim endpoint binding mismatch");
            }
            MlsEndpointIdentity::agent_runtime(
                agent_id.clone(),
                method.clone(),
                authorization_ref.clone(),
            )?
        }
        (None, None, None, None) => MlsEndpointIdentity::minimal_metadata_pairwise(
            claim.principal_id.clone(),
            claim
                .pairwise_verification_method
                .clone()
                .ok_or_else(|| anyhow::anyhow!("pairwise KeyPackage claim omits its method"))?,
        )?,
        _ => anyhow::bail!("KeyPackage claim has an incomplete or mixed endpoint identity"),
    };
    let keypackage = arkret::base64url_decode(claim.keypackage.as_bytes())?;
    let keypackage_ref = arkret::Hash::new(arkret::canonical::sha256_digest(&keypackage))?;
    let cipher_suite = arkret::mls::keypackage_ciphersuite_canonical_id(&keypackage)?;
    Ok(MlsKeyPackageRecord {
        keypackage_id: claim.keypackage_ref.clone(),
        actor_id: claim.actor_id.clone(),
        endpoint,
        keypackage: claim.keypackage.clone(),
        keypackage_ref,
        cipher_suites: vec![cipher_suite.to_owned()],
        capabilities: claim.capabilities.clone(),
        state: MlsKeyPackageState::Claimed,
        claim_id: Some(claim.claim_id.clone()),
        created_at: Utc::now(),
        expires_at: Some(claim.expires_at),
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
    event: &arkret::Event,
) -> Option<EncryptedPayload> {
    let envelope = serde_json::from_value(event.payload.get("encrypted_content")?.clone()).ok()?;
    encrypted_payload_for_event(event, &envelope)
}

/// Extract the `encrypted_metadata` carrier of a message payload, if any.
///
/// The envelope shape is identical to `encrypted_content`
/// (`encrypted-envelope.schema.json`); only the payload plaintext differs —
/// `message_metadata` JSON instead of a content block. Decryption goes
/// through the same MLS group as the content carrier.
#[must_use]
pub fn extract_encrypted_metadata_payload_from_message_content(
    event: &arkret::Event,
) -> Option<EncryptedPayload> {
    let envelope = serde_json::from_value(event.payload.get("encrypted_metadata")?.clone()).ok()?;
    encrypted_payload_for_event(event, &envelope)
}

pub(crate) fn encrypted_payload_for_event(
    event: &arkret::Event,
    envelope: &arkret::EncryptedEnvelope,
) -> Option<EncryptedPayload> {
    let sender_domain = event
        .producer_proof
        .as_ref()?
        .verification_method
        .as_str()
        .rsplit_once('#')?
        .1;
    let header = envelope
        .reconstruct_pre_encryption_header(
            EncryptedPayloadScheme::MlsRfc9420,
            event.scope_ref.clone(),
            event.kind.as_str(),
            sender_domain,
            None,
        )
        .ok()?;
    arkret::mls::encrypted_envelope_to_payload_with_verified_header(envelope, header).ok()
}

#[must_use]
pub fn message_content_has_encrypted_carrier(content: &BTreeMap<String, Value>) -> bool {
    content.get("encrypted_content").is_some()
}

fn collect_direct_conversation_bound_payloads(
    value: &Value,
    remaining_depth: usize,
    payloads: &mut Vec<(DirectConversationBoundPayload, Option<String>)>,
) {
    if let Value::Object(object) = value
        && object.get("kind").and_then(Value::as_str) == Some("ak.direct_conversation.bound")
        && let Some(payload) = object.get("payload")
        && let Ok(payload) =
            serde_json::from_value::<DirectConversationBoundPayload>(payload.clone())
    {
        let event_ref = object
            .get("event_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        payloads.push((payload, event_ref));
        return;
    }
    if let Ok(payload) = serde_json::from_value::<DirectConversationBoundPayload>(value.clone()) {
        payloads.push((payload, None));
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

fn enrich_mls_welcome_consume_binding(
    binding: &mut ArkretMlsWelcomeConsumeBinding,
    direct_bindings: &BTreeMap<String, ArkretDirectConversationWelcomeBinding>,
) {
    let Some(direct) = binding.realm_id.as_deref().and_then(|realm_id| {
        direct_bindings
            .values()
            .find(|candidate| candidate.realm_id == realm_id)
    }) else {
        return;
    };
    binding.realm_id = Some(direct.realm_id.clone());
    binding.strand_id = Some(direct.strand_id.clone());
}

fn restore_mls_identity(
    record: &ArkretMlsIdentityStateRecord,
) -> anyhow::Result<ArkretMlsIdentity> {
    ArkretMlsIdentity::restore_from_private_state(
        record.actor_id.clone(),
        record.endpoint.clone(),
        &record.private_state,
    )
    .map_err(|err| anyhow::anyhow!("restore Arkret MLS identity state: {err}"))
}

fn mls_identity_signature_public_key(identity: &ArkretMlsIdentity) -> anyhow::Result<Vec<u8>> {
    let snapshot = identity
        .export_private_state()
        .map_err(|err| anyhow::anyhow!("export Arkret MLS identity state: {err}"))?;
    let snapshot: Value =
        serde_json::from_slice(&snapshot).context("decode Arkret MLS identity state snapshot")?;
    let encoded = snapshot
        .get("signer_public_key")
        .and_then(Value::as_str)
        .context("Arkret MLS identity state is missing signer_public_key")?;
    URL_SAFE_NO_PAD
        .decode(encoded)
        .context("decode Arkret MLS signer public key")
}

fn endpoint_human_device_id(endpoint: &MlsEndpointIdentity) -> Option<&DeviceId> {
    match endpoint {
        MlsEndpointIdentity::HumanDevice { device_id, .. } => Some(device_id),
        MlsEndpointIdentity::AgentRuntime { .. }
        | MlsEndpointIdentity::MinimalMetadataPairwise { .. } => None,
    }
}

fn mls_identity_key(actor_id: &ActorId, endpoint: &MlsEndpointIdentity) -> anyhow::Result<String> {
    Ok(arkret::canonical::canonical_json_string(&(
        actor_id, endpoint,
    ))?)
}

fn mls_fresh_key_package_cache_key(identity_key: &str, keypackage_id: &str) -> String {
    format!("{identity_key}#single_use#{keypackage_id}")
}

fn local_key_package_can_be_published(record: &MlsKeyPackageRecord) -> bool {
    !record.last_resort && record.is_usable() && record.state == MlsKeyPackageState::Published
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
            // The only valid group id is the SDK derivation of this Realm
            // scope; the projection does not need to restate it.
            mls_group_id: None,
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
    use std::sync::LazyLock;

    use arkret::{EncryptedPayloadScheme, Hash, KeyPackageClaimRecord};
    use serde_json::json;

    use super::*;

    fn fixture_event_id(seed: u8) -> EventId {
        EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [seed; 32])
    }

    static FIXTURE_REALM: LazyLock<RealmId> =
        LazyLock::new(|| RealmId::from_event_id(&fixture_event_id(0x01)));
    static FIXTURE_REALM_B: LazyLock<RealmId> =
        LazyLock::new(|| RealmId::from_event_id(&fixture_event_id(0x02)));
    static FIXTURE_CLAIM_REALM: LazyLock<RealmId> =
        LazyLock::new(|| RealmId::from_event_id(&fixture_event_id(0x03)));
    static FIXTURE_STRAND: LazyLock<arkret::StrandId> =
        LazyLock::new(|| arkret::StrandId::from_event_id(&fixture_event_id(0x04)));
    static FIXTURE_EVENT_1: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x51));
    static FIXTURE_EVENT_2: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x52));
    static FIXTURE_EVENT_6: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x56));
    static FIXTURE_EVENT_9: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x59));
    static FIXTURE_EVENT_13: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x5d));
    static FIXTURE_EVENT_14: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x5e));

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

    fn direct_conversation_bound_payload(realm_id: &str, strand_id: &str) -> Value {
        json!({
            "pair_key": format!("sha256:{}", "aa".repeat(32)),
            "unordered_participant_ids": [
                {"kind": "account", "account_id": {
                    "principal_id": "ak:did_core:webvh:z6mkfixturealice",
                    "station_id": "ak:did_core:webvh:z6mkfixturestation"
                }},
                {"kind": "account", "account_id": {
                    "principal_id": "ak:did_core:webvh:z6mkfixtureagent",
                    "station_id": "ak:did_core:webvh:z6mkfixturestation"
                }}
            ],
            "realm_id": realm_id,
            "main_strand_id": strand_id,
            "founding_unit_digest": format!("sha256:{}", "bb".repeat(32)),
            "authorization_basis": {
                "kind": "accepted_contact",
                "event_refs": [
                    FIXTURE_EVENT_1.as_str(),
                    FIXTURE_EVENT_2.as_str()
                ]
            },
            "initial_exact_pair_group_state_ref": FIXTURE_EVENT_6.as_str(),
            "created_at": "2026-08-01T00:00:00.000Z"
        })
    }

    #[test]
    fn direct_conversation_binding_enriches_pending_welcome_consume_in_either_order() {
        let home = temp_home("direct-welcome-binding-order");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let realm_id = FIXTURE_REALM_B.as_str();
        let strand_id = FIXTURE_STRAND.as_str();
        let group_id = "AZZBmwAAAACAAAAAAAAAAQ";
        let welcome_ref = FIXTURE_EVENT_13.as_str();
        let pending = ArkretMlsWelcomeConsumeBinding {
            keypackage_ref: "ak:mls:keypackage:pending".to_owned(),
            claim_id: "ak:claim:pending".to_owned(),
            welcome_ref: Some(welcome_ref.to_owned()),
            realm_id: Some(realm_id.to_owned()),
            strand_id: None,
            mls_group_id: group_id.to_owned(),
            epoch: 1,
            group_state_ref: None,
            recipient_durable_receipt: None,
            verified_leaf_bindings: Vec::new(),
        };
        let mut state = store.load().expect("state should load");
        state
            .mls_welcome_consume_bindings
            .insert(pending.cache_key(), pending.clone());
        store
            .save(&mut state)
            .expect("pending binding should persist");

        let bound = direct_conversation_bound_payload(realm_id, strand_id);
        let binding_event_ref = FIXTURE_EVENT_14.as_str();
        assert_eq!(
            store
                .record_direct_conversation_binding_from_value(&json!({
                    "event_id": binding_event_ref,
                    "kind": "ak.direct_conversation.bound",
                    "payload": bound
                }))
                .expect("typed Direct Conversation binding should persist"),
            1
        );
        assert_eq!(
            store
                .direct_conversation_binding_event_ref(realm_id)
                .expect("binding lookup should succeed")
                .as_ref()
                .map(EventId::as_str),
            Some(binding_event_ref)
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
            recipient_durable_receipt: None,
            verified_leaf_bindings: Vec::new(),
        };
        enrich_mls_welcome_consume_binding(&mut later, &state.direct_conversation_welcome_bindings);
        assert_eq!(later.realm_id.as_deref(), Some(realm_id));
        assert_eq!(later.strand_id.as_deref(), Some(strand_id));
        // The binding endorses coordinates only, so it can no longer name the
        // Welcome Event a consume is still missing.
        assert_eq!(later.welcome_ref, None);

        // A missing Realm never borrows an unrelated stored binding.
        let mut orphan = ArkretMlsWelcomeConsumeBinding {
            keypackage_ref: "ak:mls:keypackage:orphan".to_owned(),
            claim_id: "ak:claim:orphan".to_owned(),
            welcome_ref: None,
            realm_id: None,
            strand_id: None,
            mls_group_id: group_id.to_owned(),
            epoch: 1,
            group_state_ref: None,
            recipient_durable_receipt: None,
            verified_leaf_bindings: Vec::new(),
        };
        enrich_mls_welcome_consume_binding(
            &mut orphan,
            &state.direct_conversation_welcome_bindings,
        );
        assert_eq!(orphan.realm_id, None);
        assert_eq!(orphan.strand_id, None);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn agent_mls_cache_binds_station_and_authorization_and_restore_rejects_tampering() {
        let home = temp_home("agent-mls-full-actor");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let account = test_account("ak:did_core:web:agent.example");
        let other_station = AccountId::new(
            account.principal_id.clone(),
            DidCoreId::new("ak:did_core:web:other-station.example").unwrap(),
        );
        let key_ref = ArkretKeyRef::InlineSeedBase64 {
            value: base64::engine::general_purpose::STANDARD_NO_PAD.encode([42; 32]),
        };
        let method = "did:web:agent.example#runtime-1";
        let authorization = EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [73; 32]);
        let later_authorization =
            EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [74; 32]);
        let first = store
            .ensure_agent_mls_key_package(&account, &key_ref, method, authorization.as_str())
            .unwrap();
        let other = store
            .ensure_agent_mls_key_package(&other_station, &key_ref, method, authorization.as_str())
            .unwrap();
        let later = store
            .ensure_agent_mls_key_package(&account, &key_ref, method, later_authorization.as_str())
            .unwrap();
        assert_eq!(first.actor_id, ActorId::account(account.clone()));
        assert_eq!(other.actor_id, ActorId::account(other_station.clone()));
        assert_ne!(first.keypackage_ref, other.keypackage_ref);
        assert_ne!(first.keypackage_ref, later.keypackage_ref);
        let state = store.load().unwrap();
        assert_eq!(state.mls_identities.len(), 3);
        let first_endpoint = agent_mls_endpoint(
            account.principal_id.as_str(),
            method,
            authorization.as_str(),
        )
        .unwrap();
        let record = state
            .mls_identities
            .get(&mls_identity_key(&first.actor_id, &first_endpoint).unwrap())
            .unwrap();
        let identity = restore_mls_identity(record).unwrap();
        let group = identity
            .create_group(&ScopeRef::Realm {
                realm_id: RealmId::from_event_id(&authorization),
            })
            .unwrap();
        let leaves = group.active_author_leaves();
        let arkret::AuthorLeafCredential::Basic { identity } = &leaves[0].credential else {
            panic!("BasicCredential required")
        };
        assert_eq!(
            identity,
            &arkret::canonical::canonical_json_bytes(&first.actor_id).unwrap()
        );
        let mut tampered = record.clone();
        tampered.actor_id = ActorId::account(other_station);
        assert!(restore_mls_identity(&tampered).is_err());
        tampered = record.clone();
        tampered.endpoint = agent_mls_endpoint(
            account.principal_id.as_str(),
            method,
            later_authorization.as_str(),
        )
        .unwrap();
        assert!(restore_mls_identity(&tampered).is_err());
        let wire = serde_json::to_value(record).unwrap();
        assert!(wire.get("device_id").is_none());
        assert!(wire.get("principal_id").is_none());
        assert!(wire.get("last_resort_key_package").is_none());
    }

    #[test]
    fn agent_mls_identity_reuses_authorized_runtime_key() {
        let home = temp_home("agent-runtime-mls-key");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let seed = [9_u8; 32];
        let key_ref = crate::arkret::ArkretKeyRef::InlineSeedBase64 {
            value: base64::engine::general_purpose::STANDARD_NO_PAD.encode(seed),
        };
        let principal = "ak:did_core:web:agent.example";
        let verification_method = "did:web:agent.example#runtime-1";
        let authorized_event_ref = "ak:event:ARELvWOpF6BRrks3DlbQy-9XIE6aAQQumDQp7fA4ApeM";
        let first = store
            .ensure_agent_mls_key_package(
                &test_account(principal),
                &key_ref,
                &verification_method,
                authorized_event_ref,
            )
            .unwrap();
        let expected_endpoint =
            agent_mls_endpoint(principal, verification_method, authorized_event_ref).unwrap();
        assert_eq!(first.endpoint, expected_endpoint);
        assert_eq!(
            store
                .mls_key_package_maintenance_deficit(&first.endpoint, 8)
                .unwrap(),
            7
        );

        let rotated_seed = [10_u8; 32];
        let rotated_key_ref = crate::arkret::ArkretKeyRef::InlineSeedBase64 {
            value: base64::engine::general_purpose::STANDARD_NO_PAD.encode(rotated_seed),
        };
        let rotated = store
            .ensure_agent_mls_key_package(
                &test_account(principal),
                &rotated_key_ref,
                &verification_method,
                authorized_event_ref,
            )
            .unwrap();

        let state = store.load().unwrap();
        let identity = restore_mls_identity(state.mls_identities.values().next().unwrap()).unwrap();
        assert_eq!(identity.endpoint_identity(), expected_endpoint);
        let expected = ed25519_dalek::SigningKey::from_bytes(&rotated_seed)
            .verifying_key()
            .to_bytes();
        assert_eq!(
            mls_identity_signature_public_key(&identity).unwrap(),
            expected
        );
        assert_ne!(first.keypackage_ref, rotated.keypackage_ref);

        drop(store);
        let reopened = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let after_restart = reopened
            .ensure_agent_mls_key_package(
                &test_account(principal),
                &rotated_key_ref,
                verification_method,
                authorized_event_ref,
            )
            .unwrap();
        assert_eq!(after_restart.keypackage_ref, rotated.keypackage_ref);
        assert_eq!(after_restart.endpoint, expected_endpoint);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn agent_keypackage_rejects_mismatched_authorization_binding() {
        let home = temp_home("agent-runtime-authorization-binding");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let seed = [12_u8; 32];
        let key_ref = crate::arkret::ArkretKeyRef::InlineSeedBase64 {
            value: base64::engine::general_purpose::STANDARD_NO_PAD.encode(seed),
        };
        let principal = "ak:did_core:web:agent.example";
        let verification_method = "did:web:agent.example#runtime-1";
        let authorized_event_ref = "ak:event:ARELvWOpF6BRrks3DlbQy-9XIE6aAQQumDQp7fA4ApeM";
        store
            .ensure_agent_mls_key_package(
                &test_account(principal),
                &key_ref,
                verification_method,
                authorized_event_ref,
            )
            .unwrap();

        let wrong_method = "did:web:agent.example#runtime-2";
        let method_error = store
            .create_fresh_agent_mls_key_packages(
                &test_account(principal),
                1,
                &key_ref,
                wrong_method,
                authorized_event_ref,
            )
            .unwrap_err();
        assert!(
            method_error
                .to_string()
                .contains("identity must be initialized before pool replenishment")
        );

        let wrong_authorization = store
            .create_fresh_agent_mls_key_packages(
                &test_account(principal),
                1,
                &key_ref,
                verification_method,
                "ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6",
            )
            .unwrap_err();
        assert!(
            wrong_authorization
                .to_string()
                .contains("identity must be initialized before pool replenishment")
        );

        let wrong_principal = "ak:did_core:web:other-agent.example";
        let wrong_principal_method = "did:web:other-agent.example#runtime-1";
        let principal_error = store
            .create_fresh_agent_mls_key_packages(
                &test_account(wrong_principal),
                1,
                &key_ref,
                wrong_principal_method,
                authorized_event_ref,
            )
            .unwrap_err();
        assert!(
            principal_error
                .to_string()
                .contains("must be initialized before pool replenishment")
        );
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
        let principal = "ak:did_core:web:agent.example";
        let verification_method = "did:web:agent.example#runtime-1";
        let authorized_event_ref = "ak:event:ARELvWOpF6BRrks3DlbQy-9XIE6aAQQumDQp7fA4ApeM";
        let initial = store
            .ensure_agent_mls_key_package(
                &test_account(principal),
                &key_ref,
                verification_method,
                authorized_event_ref,
            )
            .unwrap();
        let fresh = store
            .create_fresh_agent_mls_key_packages(
                &test_account(principal),
                8,
                &key_ref,
                verification_method,
                authorized_event_ref,
            )
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
        assert_eq!(
            mls_identity_signature_public_key(&identity).unwrap(),
            expected
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    fn encrypted_payload() -> EncryptedPayload {
        let realm_id = RealmId::new(FIXTURE_REALM.as_str()).unwrap();
        let effective_scope = ScopeRef::Realm { realm_id };
        let group_id = effective_scope.canonical_mls_group_id().unwrap();
        EncryptedPayload {
            scheme: EncryptedPayloadScheme::MlsRfc9420,
            group_id: group_id.clone(),
            epoch: 3,
            content_type: "application/vnd.arkret.content+json".to_owned(),
            ciphertext: "abc".to_owned(),
            pre_encryption_header: EventContentPreEncryptionHeader::reconstruct(
                "1.0",
                "application/vnd.arkret.content+json",
                EncryptedPayloadScheme::MlsRfc9420,
                effective_scope,
                "ak.message.create",
                3,
                EventId::new(FIXTURE_EVENT_2.as_str()).unwrap(),
                "ak:device:01904100-0000-7000-8000-000000000003",
                EventContentRoutingContext::None,
            )
            .unwrap(),
            payload_digest: Hash::new(
                "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            )
            .expect("test digest should parse"),
        }
    }

    #[test]
    fn state_file_persists_unable_to_decrypt() {
        let home = temp_home("utd");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        store.ensure_created().expect("create");
        store
            .record_unable_to_decrypt(
                FIXTURE_EVENT_1.as_str(),
                FIXTURE_REALM.as_str(),
                "ak:did_core:webvh:z6mkfixturealice",
                encrypted_payload(),
                UnableToDecryptReason::NoSession,
            )
            .expect("record");
        let state = store.load().expect("load");
        assert_eq!(state.unable_to_decrypt.len(), 1);
        let wire = serde_json::to_value(&state).unwrap();
        assert!(wire.get("binding").is_none());
        let reopened = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        let retained = reopened.load().unwrap();
        assert_eq!(retained.unable_to_decrypt, state.unable_to_decrypt);
        let mut obsolete = wire;
        obsolete["binding"] = serde_json::json!({"device_keys": {}});
        assert!(serde_json::from_value::<ArkretCryptoStateFile>(obsolete).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn extracts_encrypted_payload_carriers() {
        let payload = encrypted_payload();
        let envelope = arkret::mls::encrypted_envelope_from_payload(&payload).expect("envelope");
        let content = BTreeMap::from([(
            "encrypted_content".to_owned(),
            serde_json::to_value(&envelope).expect("envelope should serialize"),
        )]);
        assert!(message_content_has_encrypted_carrier(&content));
        let parsed = arkret::mls::encrypted_envelope_to_payload_with_verified_header(
            &envelope,
            payload.pre_encryption_header.clone(),
        )
        .expect("payload");
        assert_eq!(parsed.group_id, payload.group_id);
    }

    #[test]
    fn sync_realm_policy_is_persisted() {
        let home = temp_home("policy");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        let canonical_group_id = ScopeRef::Realm {
            realm_id: FIXTURE_REALM.clone(),
        }
        .canonical_mls_group_id()
        .unwrap()
        .to_string();
        let realms = json!({
            FIXTURE_REALM.as_str(): {
                "state": {
                    "realm": {
                        "content_encryption_floor": "e2ee_required",
                        "encryption_profile": "mls",
                        "mls_group_id": canonical_group_id.clone()
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
            .get(FIXTURE_REALM.as_str())
            .expect("policy should be present");
        assert!(policy.requires_e2ee());
        assert_eq!(policy.group_id_for_realm().unwrap(), canonical_group_id);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn direct_conversation_sync_projection_persists_required_e2ee_policy() {
        let home = temp_home("direct-conversation-policy");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        let realm_id = FIXTURE_REALM.as_str();
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
            policy.group_id_for_realm().unwrap(),
            ScopeRef::Realm {
                realm_id: FIXTURE_REALM.clone()
            }
            .canonical_mls_group_id()
            .unwrap()
            .to_string()
        );
        assert_eq!(policy.encryption_profile.as_deref(), Some("mls_rfc9420"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn direct_conversation_policy_derives_canonical_group_id() {
        let realm_id = FIXTURE_REALM.as_str();
        let policy = ArkretRealmCryptoPolicy {
            realm_id: realm_id.to_owned(),
            content_encryption_floor: ArkretContentEncryptionFloor::E2eeRequired,
            encryption_profile: Some("mls_rfc9420".to_owned()),
            mls_group_id: None,
            source: "account_subscribe_direct_conversation".to_owned(),
            updated_at: Utc::now(),
        };
        assert_eq!(
            policy.group_id_for_realm().unwrap(),
            ScopeRef::Realm {
                realm_id: FIXTURE_REALM.clone()
            }
            .canonical_mls_group_id()
            .unwrap()
            .to_string()
        );
    }

    #[test]
    fn realm_policy_rejects_noncanonical_group_id() {
        let home = temp_home("noncanonical-group-id");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        let policy = ArkretRealmCryptoPolicy {
            realm_id: FIXTURE_REALM.as_str().to_owned(),
            content_encryption_floor: ArkretContentEncryptionFloor::E2eeRequired,
            encryption_profile: Some("mls_rfc9420".to_owned()),
            mls_group_id: Some("group1".to_owned()),
            source: "test".to_owned(),
            updated_at: Utc::now(),
        };
        assert!(policy.group_id_for_realm().is_err());
        assert!(store.upsert_realm_policy(policy).is_err());
        assert!(
            !store
                .load()
                .unwrap()
                .realm_policies
                .contains_key(FIXTURE_REALM.as_str())
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn missing_required_group_state_blocks_encryption() {
        let home = temp_home("encrypt-missing");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        store
            .upsert_realm_policy(ArkretRealmCryptoPolicy {
                realm_id: FIXTURE_REALM.as_str().to_owned(),
                content_encryption_floor: ArkretContentEncryptionFloor::E2eeRequired,
                encryption_profile: Some("mls".to_owned()),
                mls_group_id: None,
                source: "test".to_owned(),
                updated_at: Utc::now(),
            })
            .expect("policy should persist");
        let outcome = store
            .encrypt_content_block_for_realm(
                FIXTURE_REALM.as_str(),
                &json!({"kind":"ak.content.text","body":"secret"}),
            )
            .expect("encryption decision should complete");
        assert_eq!(
            outcome,
            ArkretEncryptOutcome::MissingRequiredGroupState {
                realm_id: FIXTURE_REALM.as_str().to_owned(),
                group_id: ScopeRef::Realm {
                    realm_id: FIXTURE_REALM.clone()
                }
                .canonical_mls_group_id()
                .unwrap()
                .to_string()
            }
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn presence_rejects_another_stream_before_mutating_crypto_state() {
        let home = temp_home("presence-wrong-stream");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let event = EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [61; 32]);
        let realm_id = RealmId::from_event_id(&event);
        let other_realm = RealmId::from_event_id(&EventId::from_digest(
            arkret::canonical::DigestSuite::Sha256,
            [62; 32],
        ));
        let account = AccountId::new(
            DidCoreId::new("ak:did_core:web:agent.example").unwrap(),
            DidCoreId::new("ak:did_core:web:station.example").unwrap(),
        );
        let key_ref = ArkretKeyRef::InlineSeedBase64 {
            value: base64::engine::general_purpose::STANDARD_NO_PAD.encode([42; 32]),
        };
        for stream_ref in [
            arkret::CommitStreamRef::Realm {
                realm_id: other_realm,
            },
            arkret::CommitStreamRef::Sidecar {
                realm_id: realm_id.clone(),
                sidecar_id: arkret::SidecarId::from_event_id(&event),
            },
        ] {
            let head = arkret::CommitStreamHead {
                stream_ref,
                stream_position: 3,
                commit_id: arkret::RealmCommitId::from_digest([45; 32]),
            };
            let error = store
                .seal_online_presence_signal(
                    realm_id.as_str(),
                    &account,
                    "did:web:agent.example#runtime-1",
                    &key_ref,
                    &head,
                    Utc::now(),
                )
                .unwrap_err();
            assert!(error.to_string().contains("another independent stream"));
            assert!(
                !store.path().exists(),
                "invalid head must not create or mutate persisted state"
            );
        }
    }

    #[test]
    fn bootstrap_plan_marks_key_backup_restore_needed() {
        let home = temp_home("bootstrap");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "a1");
        let record = store
            .plan_bootstrap_for_payload(
                "ak:did_core:webvh:z6mkfixturealice",
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
            state.key_backup.last_needed_for_group_id,
            Some(encrypted_payload().group_id.to_string())
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn single_use_key_package_claim_rotates_local_cache() {
        let home = temp_home("kp-single-use");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "bob");
        let principal = "ak:did_core:webvh:z6mkfixturebob";
        let device = "ak:device:01904100-0000-7000-8000-00000000000e";
        let first = store
            .ensure_mls_key_package(&test_account(principal), device)
            .expect("single-use KeyPackage should be created");

        let claimed = store
            .mark_mls_key_package_claimed(first.keypackage_ref.as_str(), "ak:claim:test")
            .expect("claim marker should persist")
            .expect("KeyPackage should be found");
        assert_eq!(claimed.state, MlsKeyPackageState::Claimed);
        assert_eq!(claimed.claim_id.as_deref(), Some("ak:claim:test"));

        let rotated = store
            .ensure_mls_key_package(&test_account(principal), device)
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
        let principal = "ak:did_core:web:agent.example";
        let device = "ak:device:01904100-0000-7000-8000-000000000001";
        let record = store
            .ensure_mls_key_package(&test_account(principal), device)
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
    fn pool_cleanup_is_limited_to_the_current_agent_binding() {
        let home = temp_home("kp-revoke-agent-scope");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "legacy");
        let current_principal = "ak:did_core:web:current-agent.example";
        let current_device = "ak:device:01904100-0000-7000-8000-000000000011";
        let replaced_principal = "ak:did_core:web:replaced-agent.example";
        let replaced_device = "ak:device:01904100-0000-7000-8000-000000000012";
        let current = store
            .ensure_mls_key_package(&test_account(current_principal), current_device)
            .expect("current Agent KeyPackage should be created");
        let replaced = store
            .ensure_mls_key_package(&test_account(replaced_principal), replaced_device)
            .expect("replaced Agent KeyPackage should be created");

        let refs = store
            .revocable_keypackage_refs_for_agent(current_principal, current_device)
            .expect("binding-scoped revocable refs should load");
        assert_eq!(refs, vec![current.keypackage_ref.as_str().to_owned()]);
        assert!(!refs.contains(&replaced.keypackage_ref.as_str().to_owned()));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn claimed_key_package_record_can_feed_group_add_member() {
        let home = temp_home("kp-claim-record");
        let bob_store = FileArkretCryptoStore::for_account(&home, "c1", "bob");
        let bob_principal = "ak:did_core:webvh:z6mkfixturebob";
        let bob_device = "ak:device:01904100-0000-7000-8000-00000000000e";
        let bob_key_package = bob_store
            .ensure_mls_key_package(&test_account(bob_principal), bob_device)
            .expect("Bob KeyPackage should be created");
        let claim = KeyPackageClaimRecord {
            claim_id: "ak:keypackage_claim:01904100-0000-7000-8000-000000000019".to_owned(),
            keypackage_ref: bob_key_package.keypackage_ref.to_string(),
            actor_id: bob_key_package.actor_id.clone(),
            principal_id: DidCoreId::new(bob_principal.to_owned()).unwrap(),
            device_id: Some(DeviceId::new(bob_device.to_owned()).unwrap()),
            agent_id: None,
            agent_verification_method: None,
            pairwise_verification_method: None,
            keypackage: bob_key_package.keypackage.clone(),
            capabilities: bob_key_package.capabilities.clone(),
            device_authorize_event_id: Some(
                EventId::new(FIXTURE_EVENT_9.as_str().to_owned()).unwrap(),
            ),
            agent_key_authorize_event_id: None,
            expires_at: Utc::now() + chrono::Duration::days(1),
            revocation_status: Some("active".to_owned()),
            last_resort: Some(false),
        };

        let claimed = mls_key_package_record_from_claim(&claim)
            .expect("claim record should project to local MLS record");
        assert_eq!(claimed.state, MlsKeyPackageState::Claimed);
        assert_eq!(claimed.claim_id.as_deref(), Some(claim.claim_id.as_str()));
        assert_eq!(claimed.keypackage_ref, bob_key_package.keypackage_ref);
        assert_eq!(claimed.cipher_suites, bob_key_package.cipher_suites);
        assert_eq!(claimed.actor_id, claim.actor_id);
        let mut wrong_principal = claim.clone();
        wrong_principal.principal_id = DidCoreId::new("ak:did_core:web:other.example").unwrap();
        assert!(mls_key_package_record_from_claim(&wrong_principal).is_err());
        let mut invalid_keypackage = claim.clone();
        invalid_keypackage.keypackage = "AQ".to_owned();
        assert!(mls_key_package_record_from_claim(&invalid_keypackage).is_err());

        let alice = new_human_mls_identity(
            DidCoreId::new("ak:did_core:webvh:z6mkfixturealice".to_owned()).unwrap(),
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006".to_owned()).unwrap(),
        )
        .unwrap();
        let mut alice_group = alice
            .create_group(&ScopeRef::Realm {
                realm_id: FIXTURE_CLAIM_REALM.clone(),
            })
            .unwrap();
        let add = alice_group
            .add_member(&claimed)
            .expect("claimed KeyPackage should add to MLS group");
        assert!(matches!(
            &add.welcome.recipient,
            MlsEndpointIdentity::HumanDevice { principal_id, .. }
                if principal_id.as_str() == bob_principal
        ));
        assert_eq!(
            endpoint_human_device_id(&add.welcome.recipient).map(DeviceId::as_str),
            Some(bob_device)
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
