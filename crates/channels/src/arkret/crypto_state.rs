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
    EventContentRoutingContext, EventId, MessageMetadata, MlsCommitPayload, MlsCommitSource,
    MlsEncryptedPayload, MlsEndpointIdentity, MlsKeyPackageRecord, MlsKeyPackageState,
    MlsPayloadType, MlsWelcomeEnvelope, MlsWelcomePayload, MlsWelcomeRecipient, PresencePlaintext,
    PresenceState, RealmId, ScopeRef, SealId, SignalSequenceDomain, SignalSequenceEndpoint,
    StrandCreatePayload, StrandId, seal_signal_plaintext,
};
use arkret_models_crypto::MlsGroupStateRecord as CurrentMlsGroupStateRecord;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{DateTime, Utc};
use garth::{CryptoStore, MemoryCryptoStore, MlsGroupStateRecord, MlsWelcomeState};
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

/// The closed local subject a verified `ak.mls.welcome` may be admitted for.
///
/// Recipient selection and the authorization anchors that must accompany it are
/// the only part of Welcome admission that differs between an owned Agent
/// runtime and an Applet Bot `Principal` runtime. Both edges reach
/// [`FileArkretCryptoStore::admit_verified_welcomes`] with this subject, and the
/// governance closure verification that produces the checkpoint has exactly one
/// implementation, so the two edges can never drift apart on what "verified"
/// means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MlsWelcomeAdmissionSubject {
    /// Owned Agent runtime: the recipient is the Agent endpoint branch bound to
    /// its current `ak.agent.key.authorize` Event.
    OwnedAgent {
        agent_id: DidCoreId,
        agent_key_authorize_event_id: String,
    },
    /// Applet Bot `Principal` runtime. A Bot is an independent long-lived
    /// principal with its own Account and a runtime device, so it publishes and
    /// is welcomed through the ordinary closed endpoint
    /// (`recipient_principal_id` + `recipient_device_id`), never the Agent
    /// branch. `applet-integration.md` §12 additionally requires a separate
    /// E2EE join authorization carrying Applet provenance; `applet_id` and
    /// `install_authorization_ref` are the anchors that authorization must cite.
    AppletBot {
        bot_account_id: AccountId,
        device_id: DeviceId,
        applet_id: String,
        install_authorization_ref: String,
    },
}

impl MlsWelcomeAdmissionSubject {
    #[must_use]
    pub const fn recipient_principal_id(&self) -> &DidCoreId {
        match self {
            Self::OwnedAgent { agent_id, .. } => agent_id,
            Self::AppletBot { bot_account_id, .. } => &bot_account_id.principal_id,
        }
    }

    /// Exact closed recipient endpoint match. A Welcome addressed to any other
    /// endpoint branch, principal or device is not this subject's Welcome.
    #[must_use]
    pub fn matches_welcome_recipient(&self, payload: &MlsWelcomePayload) -> bool {
        if payload
            .recipient_principal_id
            .as_ref()
            .is_none_or(|recipient| recipient != self.recipient_principal_id())
        {
            return false;
        }
        match (self, &payload.recipient) {
            (
                Self::OwnedAgent { agent_id, .. },
                MlsWelcomeRecipient::Agent {
                    recipient_agent_id, ..
                },
            ) => recipient_agent_id == agent_id,
            (
                Self::AppletBot { device_id, .. },
                MlsWelcomeRecipient::Device {
                    recipient_device_id,
                },
            ) => recipient_device_id == device_id,
            _ => false,
        }
    }

    /// An Applet Bot joins ordinary multi-member Realm groups, so every
    /// occupied leaf needs its accepted historical authority before the joined
    /// snapshot can be persisted at all.
    #[must_use]
    pub const fn needs_accepted_leaf_authority(&self) -> bool {
        matches!(self, Self::AppletBot { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MlsRecoveryAction {
    UseLocalState,
    ApplyCommits { from_epoch: u64, to_epoch: u64 },
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
    pub mls_store_json: String,
    /// Provider-opaque group snapshots installed from accepted Commit or
    /// recipient Welcome delivery. Keys are canonical MLS group ids.
    #[serde(default)]
    pub mls_group_states: BTreeMap<String, CurrentMlsGroupStateRecord>,
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
    pub realm_authority_refs: BTreeMap<String, Vec<SealId>>,
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
        let store = MemoryCryptoStore::new();
        Ok(Self {
            version: STATE_VERSION.to_owned(),
            scope_id,
            generation: 0,
            unable_to_decrypt: BTreeMap::new(),
            mls_store_json: serde_json::to_string(&store)
                .map_err(|err| anyhow::anyhow!("arkret crypto store export: {err}"))?,
            mls_group_states: BTreeMap::new(),
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

    fn mls_store(&self) -> anyhow::Result<MemoryCryptoStore> {
        serde_json::from_str(&self.mls_store_json)
            .map_err(|err| anyhow::anyhow!("arkret crypto store import: {err}"))
    }

    fn set_mls_store(&mut self, store: &MemoryCryptoStore) -> anyhow::Result<()> {
        self.mls_store_json = serde_json::to_string(store)
            .map_err(|err| anyhow::anyhow!("arkret crypto store export: {err}"))?;
        Ok(())
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
        authority_refs: &[SealId],
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

    pub fn realm_authority_refs(&self, realm_id: &str) -> anyhow::Result<Option<Vec<SealId>>> {
        Ok(self.load()?.realm_authority_refs.get(realm_id).cloned())
    }

    /// Realm ids for which the account has both an E2EE policy and mutable MLS
    /// state accepted at a known group-state reference. Only these scopes can
    /// safely advertise encrypted v1 presence.
    pub fn presence_ready_realm_ids(&self) -> anyhow::Result<Vec<String>> {
        let state = self.load()?;
        let store = state.mls_store()?;
        let mut realms = Vec::new();
        for policy in state
            .realm_policies
            .values()
            .filter(|policy| policy.requires_e2ee())
        {
            let group_id = policy.group_id_for_realm()?;
            let Some(record) = store.mls_group_state(&group_id) else {
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
        let mut store = state.mls_store()?;
        let group_id = policy.group_id_for_realm()?;
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
            .persist_state(&mut store)
            .map_err(|error| anyhow::anyhow!("persist post-Signal MLS group: {error}"))?;
        state.set_mls_store(&store)?;
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

    /// Admit a Welcome only from a completely verified accepted governance
    /// closure, binding every active leaf before the T3 save. The subject is the
    /// only thing that decides which recipient endpoint and which authorization
    /// anchors are acceptable; every other admission requirement is shared, and
    /// the closure verification that produced `checkpoint` has one caller.
    ///
    /// `accepted_leaf_authority` carries, keyed by Welcome Event id, the
    /// Station's accepted historical leaf authority. It is only consulted for
    /// subjects that join ordinary multi-member groups, and it is checked
    /// against the joined group's own governance binding and its RFC 9420 leaf
    /// credentials and keys before anything is persisted.
    pub fn admit_verified_welcomes(
        &self,
        checkpoint: &arkret::MlsGovernanceVerificationCheckpoint,
        subject: &MlsWelcomeAdmissionSubject,
        accepted_leaf_authority: &BTreeMap<EventId, arkret::MlsAcceptedArtifactOutcome>,
    ) -> anyhow::Result<usize> {
        let _guard = self.mutation_lock.lock();
        let mut state = self.load()?;
        let mut store = state.mls_store()?;
        let mut admitted = 0;
        for event in &checkpoint.accepted_events {
            if event.kind != arkret::EventKind::MlsWelcome {
                continue;
            }
            let payload: MlsWelcomePayload =
                serde_json::from_value(serde_json::to_value(&event.payload)?)?;
            if !subject.matches_welcome_recipient(&payload)
                || store.mls_group_state(payload.mls_group_id()).is_some()
            {
                continue;
            }
            self.validate_local_mls_welcome_payload(&payload, subject)?;
            anyhow::ensure!(
                payload.claim_envelope.intended_realm_id == checkpoint.realm_id,
                "Welcome checkpoint Realm mismatch"
            );
            let commit_event = checkpoint
                .accepted_events
                .iter()
                .find(|accepted| accepted.event_id == payload.commit_ref)
                .context("Welcome Commit is absent from the verified checkpoint")?;
            anyhow::ensure!(
                commit_event.kind == arkret::EventKind::MlsCommit,
                "Welcome transition is not a Commit"
            );
            let commit: MlsCommitPayload =
                serde_json::from_value(serde_json::to_value(&commit_event.payload)?)?;
            anyhow::ensure!(
                commit.governance_binding() == &payload.governance_binding,
                "Welcome governance binding differs from accepted Commit"
            );
            let genesis = accepted_mls_genesis(checkpoint, &payload)?;
            let envelope = mls_welcome_envelope(&payload)?;
            let identity = state
                .mls_identities
                .values()
                .find(|identity| match subject {
                    MlsWelcomeAdmissionSubject::OwnedAgent { agent_id, .. } => {
                        identity.actor_id.signing_principal_id() == agent_id
                    }
                    MlsWelcomeAdmissionSubject::AppletBot {
                        bot_account_id,
                        device_id,
                        ..
                    } => {
                        identity.actor_id == ActorId::account(bot_account_id.clone())
                            && endpoint_human_device_id(&identity.endpoint) == Some(device_id)
                    }
                })
                .context("Welcome private KeyPackage is unavailable")?;
            let mut group =
                ArkretMlsGroup::join_from_welcome(restore_mls_identity(identity)?, &envelope)?;
            anyhow::ensure!(
                group.current_governance_binding()?.as_ref() == Some(&payload.governance_binding),
                "joined MLS group differs from verified accepted binding"
            );
            match subject {
                MlsWelcomeAdmissionSubject::OwnedAgent { agent_id, .. } => {
                    install_owned_agent_leaf_bindings(
                        &mut group, checkpoint, event, &payload, &envelope, genesis, agent_id,
                    )?;
                }
                MlsWelcomeAdmissionSubject::AppletBot {
                    bot_account_id,
                    device_id,
                    applet_id,
                    install_authorization_ref,
                } => {
                    install_applet_bot_leaf_bindings(
                        &mut group,
                        checkpoint,
                        event,
                        &payload,
                        bot_account_id,
                        device_id,
                        applet_id,
                        install_authorization_ref,
                        accepted_leaf_authority,
                    )?;
                }
            }
            stage_verified_welcome(
                &mut state,
                &mut store,
                &payload,
                &event.event_id,
                group.verified_leaf_bindings()?,
            )?;
            let record = group.persist_state(&mut store)?;
            state.bootstrap.insert(
                record.group_id.clone(),
                ArkretBootstrapRecord {
                    group_id: record.group_id,
                    required_epoch: record.epoch,
                    local_epoch: Some(record.epoch),
                    group_state_ref: Some(payload.commit_ref.to_string()),
                    action: MlsRecoveryAction::ConsumeWelcome,
                    updated_at: Utc::now(),
                },
            );
            admitted += 1;
        }
        if admitted > 0 {
            state.set_mls_store(&store)?;
            self.save(&mut state)?;
        }
        // The recipient durable receipt is signed by the recipient's own MLS
        // endpoint key. Only the Agent branch has a runtime verification method
        // to sign one with; an Applet Bot's device verification method is not
        // part of its runtime identity custody, so no receipt is produced and
        // its consume binding stays pending until the Bot device signs.
        let MlsWelcomeAdmissionSubject::OwnedAgent {
            agent_id,
            agent_key_authorize_event_id: authorization_ref,
        } = subject
        else {
            return Ok(admitted);
        };
        let agent_id = agent_id.as_str();
        // Sign only after reading the joined state back across the durable barrier.
        let mut durable = self.load()?;
        let durable_store = durable.mls_store()?;
        let mut receipts_changed = false;
        for event in &checkpoint.accepted_events {
            if event.kind != arkret::EventKind::MlsWelcome {
                continue;
            }
            let payload: MlsWelcomePayload =
                serde_json::from_value(serde_json::to_value(&event.payload)?)?;
            if payload
                .recipient_principal_id
                .as_ref()
                .is_none_or(|id| id.as_str() != agent_id)
            {
                continue;
            }
            self.validate_agent_mls_welcome_payload(&payload, agent_id, authorization_ref)?;
            let Some(record) = durable_store.mls_group_state(payload.mls_group_id()) else {
                continue;
            };
            anyhow::ensure!(
                record.epoch == payload.epoch(),
                "durable Welcome epoch mismatch"
            );
            ArkretMlsGroup::restore_from_state_record(&record)?.verified_leaf_bindings()?;
            let identity = durable
                .mls_identities
                .values()
                .find(|identity| identity.actor_id.signing_principal_id().as_str() == agent_id)
                .context("durable recipient identity is unavailable")?;
            let MlsWelcomeRecipient::Agent {
                recipient_agent_id,
                recipient_agent_verification_method,
                agent_key_authorize_event_id,
            } = &payload.recipient
            else {
                continue;
            };
            let receipt = arkret::RecipientMlsDurableReceipt {
                domain: arkret::NonEmptyString::new("ak.mls.recipient_durable_receipt.v1")
                    .map_err(anyhow::Error::msg)?,
                claim_request_id: payload.claim_receipt.claim_request_id.clone(),
                key_package_ref: arkret::NonEmptyString::new(&payload.keypackage_ref)
                    .map_err(anyhow::Error::msg)?,
                recipient: arkret::RecipientMlsDurableSigner::Agent {
                    recipient_agent_id: recipient_agent_id.clone(),
                    recipient_agent_verification_method: recipient_agent_verification_method
                        .clone(),
                    agent_key_authorize_event_id: agent_key_authorize_event_id.clone(),
                },
                recipient_id: payload.claim_receipt.destination_id.clone(),
                realm_id: checkpoint.realm_id.clone(),
                mls_group_id: arkret::NonEmptyString::new(payload.mls_group_id())
                    .map_err(anyhow::Error::msg)?,
                mls_epoch: payload.epoch(),
                welcome_ref: event.event_id.clone(),
                welcome_digest: payload.claim_envelope.welcome_digest.clone(),
                durable_at: Utc::now(),
                signature: arkret::KeyOperationSignature {
                    kid: arkret::NonEmptyString::new(recipient_agent_verification_method.as_str())
                        .map_err(anyhow::Error::msg)?,
                    signature_algorithm: Some(
                        arkret::NonEmptyString::new("Ed25519").map_err(anyhow::Error::msg)?,
                    ),
                    sig: arkret::Base64UrlString::new("AA").map_err(anyhow::Error::msg)?,
                },
            };
            let receipt =
                restore_mls_identity(identity)?.sign_recipient_mls_durable_receipt(receipt)?;
            for binding in durable.mls_welcome_consume_bindings.values_mut() {
                if binding.keypackage_ref == payload.keypackage_ref
                    && binding.claim_id == payload.claim_id.as_str()
                    && binding.welcome_ref.as_deref() == Some(event.event_id.as_str())
                    && binding.group_state_ref.as_deref() == Some(payload.commit_ref.as_str())
                    && binding.recipient_durable_receipt.is_none()
                {
                    binding.recipient_durable_receipt = Some(receipt.clone());
                    receipts_changed = true;
                }
            }
        }
        if receipts_changed {
            self.save(&mut durable)?;
        }
        Ok(admitted)
    }

    /// Whether this accepted Commit still has local work to do, and therefore
    /// whether the caller has to read the Station's accepted leaf authority for
    /// it. `apply_commit` drops the whole binding map — a transition may add,
    /// remove or replace any leaf — so a Commit that advances local state
    /// cannot be persisted without the post-transition attribution, while a
    /// replay of an already-applied Commit needs nothing.
    pub fn mls_commit_needs_accepted_leaf_authority(
        &self,
        payload: &MlsCommitPayload,
    ) -> anyhow::Result<bool> {
        let state = self.load()?;
        let store = state.mls_store()?;
        Ok(store
            .mls_group_state(payload.mls_group_id())
            .is_none_or(|record| record.epoch < payload.next_epoch()))
    }

    /// Apply one accepted durable `ak.mls.commit` to the local MLS group and
    /// persist the post-Commit snapshot before any later encrypted ordinary Event is
    /// handled. Replaying the same accepted Commit is idempotent; an epoch gap
    /// fails closed instead of fabricating ratchet state.
    ///
    /// `accepted_leaf_authority` is the Station's accepted historical authority
    /// for exactly this transition. It is required whenever the Commit advances
    /// local state — see
    /// [`Self::mls_commit_needs_accepted_leaf_authority`] — and the SDK
    /// re-checks it against the post-Commit governance binding and the real RFC
    /// 9420 leaves. Reaching the install point without it is a hard error: an
    /// unattributed roster must never be persisted.
    pub fn apply_mls_commit(
        &self,
        payload: &MlsCommitPayload,
        accepted_event_ref: &EventId,
        accepted_leaf_authority: Option<&arkret::MlsAcceptedArtifactOutcome>,
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
                payload.next_epoch(),
                accepted_event_ref,
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
        let accepted = accepted_leaf_authority.with_context(|| {
            format!(
                "Arkret MLS Commit for group '{}' has no accepted leaf authority for epoch {applied_epoch}",
                payload.mls_group_id()
            )
        })?;
        anyhow::ensure!(
            accepted.transition_head.transition_ref == *accepted_event_ref
                && &accepted.governance_binding == payload.governance_binding(),
            "accepted MLS leaf authority names another transition"
        );
        group
            .install_accepted_leaf_bindings(accepted)
            .map_err(|err| anyhow::anyhow!("install accepted MLS leaf authority: {err}"))?;
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

    /// Dispatch the subject-specific half of Welcome validation: which closed
    /// recipient endpoint is acceptable, which authorization anchors it must
    /// cite, and which locally held KeyPackage it must consume.
    fn validate_local_mls_welcome_payload(
        &self,
        payload: &MlsWelcomePayload,
        subject: &MlsWelcomeAdmissionSubject,
    ) -> anyhow::Result<()> {
        match subject {
            MlsWelcomeAdmissionSubject::OwnedAgent {
                agent_id,
                agent_key_authorize_event_id,
            } => self.validate_agent_mls_welcome_payload(
                payload,
                agent_id.as_str(),
                agent_key_authorize_event_id,
            ),
            MlsWelcomeAdmissionSubject::AppletBot {
                bot_account_id,
                device_id,
                ..
            } => self.validate_applet_bot_mls_welcome_payload(payload, bot_account_id, device_id),
        }
    }

    /// The Applet Bot consumes the ordinary closed endpoint branch: the Welcome
    /// names its exact `Principal` and runtime device, the accepted claim
    /// receipt targets that exact Account and device, and the KeyPackage it
    /// consumes is one this runtime actually holds for that endpoint.
    fn validate_applet_bot_mls_welcome_payload(
        &self,
        payload: &MlsWelcomePayload,
        bot_account_id: &AccountId,
        device_id: &DeviceId,
    ) -> anyhow::Result<()> {
        let MlsWelcomeRecipient::Device {
            recipient_device_id,
        } = &payload.recipient
        else {
            anyhow::bail!("MLS Welcome does not select an Applet Bot device endpoint");
        };
        if payload
            .recipient_principal_id
            .as_ref()
            .is_none_or(|recipient| recipient != &bot_account_id.principal_id)
            || recipient_device_id != device_id
        {
            anyhow::bail!("MLS Welcome recipient does not match this Applet Bot runtime");
        }
        let request = &payload.claim_receipt.request;
        if request.target_account_id.as_ref() != Some(bot_account_id)
            || !request.target_device_ids.contains(device_id)
        {
            anyhow::bail!("MLS Welcome claim receipt targets another Account or device");
        }
        if !matches!(
            &payload.claim_ref.trust_binding,
            arkret::MlsClaimTrustBinding::DeviceAuthorizeEventId(_)
        ) {
            anyhow::bail!("MLS Welcome claim_ref is not bound to an ordinary device authorization");
        }
        let state = self.load()?;
        let local_keypackage = state
            .mls_key_packages
            .values()
            .find(|record| {
                record.keypackage_ref.as_str() == payload.keypackage_ref.as_str()
                    || record.keypackage_id == payload.keypackage_ref.as_str()
            })
            .context("MLS Welcome references no locally held Applet Bot KeyPackage")?;
        match &local_keypackage.endpoint {
            MlsEndpointIdentity::HumanDevice {
                principal_id,
                device_id: local_device_id,
            } if principal_id == &bot_account_id.principal_id && local_device_id == device_id => {}
            _ => anyhow::bail!("local KeyPackage does not belong to this Applet Bot runtime"),
        }
        if local_keypackage.keypackage_ref != payload.claim_envelope.keypackage_digest
            || payload.claim_ref.keypackage_digest != payload.claim_envelope.keypackage_digest
            || payload.claim_ref.keypackage_ref != payload.keypackage_ref
        {
            anyhow::bail!("MLS Welcome KeyPackage claim binding does not match local state");
        }
        Ok(())
    }

    fn validate_agent_mls_welcome_payload(
        &self,
        payload: &MlsWelcomePayload,
        principal_id: &str,
        authorized_event_ref: &str,
    ) -> anyhow::Result<()> {
        if payload
            .recipient_principal_id
            .as_ref()
            .is_none_or(|recipient| recipient.as_str() != principal_id)
        {
            anyhow::bail!("MLS Welcome recipient does not match this Agent runtime");
        }
        let MlsWelcomeRecipient::Agent {
            recipient_agent_id,
            agent_key_authorize_event_id,
            ..
        } = &payload.recipient
        else {
            anyhow::bail!("MLS Welcome selects a Human Device, not this Agent runtime");
        };
        if recipient_agent_id.as_str() != principal_id
            || agent_key_authorize_event_id.as_str() != authorized_event_ref
        {
            anyhow::bail!("MLS Welcome Agent endpoint binding is stale or mismatched");
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
        match &local_keypackage.endpoint {
            MlsEndpointIdentity::AgentRuntime {
                agent_id,
                agent_key_authorize_event_id,
                ..
            } if agent_id.as_str() == principal_id
                && agent_key_authorize_event_id.as_str() == authorized_event_ref => {}
            _ => anyhow::bail!("local KeyPackage does not belong to this Agent runtime"),
        }
        if local_keypackage.keypackage_ref != payload.claim_envelope.keypackage_digest
            || payload.claim_ref.keypackage_digest != payload.claim_envelope.keypackage_digest
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
                // A Strand id is event-derived: the create payload carries no id
                // of its own, so it is retyped off the accepted Event.
                serde_json::to_value(&event.payload)
                    .ok()
                    .and_then(|value| serde_json::from_value::<StrandCreatePayload>(value).ok())
                    .filter(|payload| payload.object.realm_id == event.realm_id)
                    .map(|_| StrandId::from_event_id(&event.event_id).to_string())
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
                    && binding.mls_group_id == payload.mls_group_id().to_owned()
                    && binding.epoch == payload.epoch()
                    && binding.welcome_ref.as_deref() == Some(event.event_id.as_str())
                    && binding.group_state_ref.as_deref() == Some(payload.commit_ref.as_str())
                    && binding.realm_id.as_deref()
                        == Some(payload.claim_envelope.intended_realm_id.as_str())
                {
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
        let principal = DidCoreId::new(principal_id.to_owned())
            .with_context(|| format!("invalid Arkret principal DID '{principal_id}'"))?;
        let device = DeviceId::new(device_id.to_owned())
            .with_context(|| format!("invalid Arkret device id '{device_id}'"))?;
        let local_epoch = store
            .mls_group_state(&payload.group_id)
            .map(|record| record.epoch);
        let action = plan_mls_recovery(
            &store,
            &payload.group_id,
            local_epoch,
            payload.epoch,
            &principal,
            &device,
        );
        let group_state_ref = group_state_ref_for_epoch(&state, &payload.group_id, payload.epoch);
        let record = ArkretBootstrapRecord {
            group_id: payload.group_id.clone(),
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
        let plaintext = group
            .decrypt_payload(payload)
            .map_err(|err| anyhow::anyhow!("decrypt Arkret MLS payload: {err}"))?;
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
        let group_id = policy.group_id_for_realm()?;
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
            .persist_state(&mut store)
            .map_err(|err| anyhow::anyhow!("persist Arkret MLS group: {err}"))?;
        state.set_mls_store(&store)?;
        state.bootstrap.insert(
            updated.group_id.clone(),
            ArkretBootstrapRecord {
                group_id: updated.group_id,
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

fn plan_mls_recovery(
    store: &MemoryCryptoStore,
    group_id: &str,
    local_epoch: Option<u64>,
    required_epoch: u64,
    principal_id: &DidCoreId,
    device_id: &DeviceId,
) -> MlsRecoveryAction {
    match local_epoch {
        Some(epoch) if epoch >= required_epoch => MlsRecoveryAction::UseLocalState,
        Some(epoch)
            if (epoch.saturating_add(1)..=required_epoch).all(|next_epoch| {
                store
                    .commits_for_group(group_id)
                    .iter()
                    .any(|commit| commit.epoch == next_epoch)
            }) =>
        {
            MlsRecoveryAction::ApplyCommits {
                from_epoch: epoch.saturating_add(1),
                to_epoch: required_epoch,
            }
        }
        _ if store
            .welcomes_for_device(principal_id, device_id)
            .into_iter()
            .any(|welcome| {
                welcome.group_id == group_id
                    && welcome.epoch == required_epoch
                    && store.welcome_state(&welcome.welcome_hash) == Some(MlsWelcomeState::Accepted)
            }) =>
        {
            MlsRecoveryAction::ConsumeWelcome
        }
        Some(epoch) => MlsRecoveryAction::RequestEpochRecovery {
            missing_from_epoch: epoch.saturating_add(1),
        },
        None => MlsRecoveryAction::RequestEpochRecovery {
            missing_from_epoch: required_epoch,
        },
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
    Ok(MlsKeyPackageRecord {
        keypackage_id: claim.keypackage_ref.clone(),
        actor_id: claim.actor_id.clone(),
        endpoint,
        keypackage: claim.keypackage.clone(),
        keypackage_ref,
        cipher_suites: Vec::new(),
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

/// The accepted `ak.mls.genesis` that created the Welcome's group. Shared by
/// every subject: a Welcome into a group with no accepted Genesis in the
/// verified closure is never admissible.
fn accepted_mls_genesis<'a>(
    checkpoint: &'a arkret::MlsGovernanceVerificationCheckpoint,
    payload: &MlsWelcomePayload,
) -> anyhow::Result<&'a arkret::Event> {
    checkpoint
        .accepted_events
        .iter()
        .find(|accepted| {
            accepted.kind == arkret::EventKind::MlsGenesis
                && serde_json::to_value(&accepted.payload)
                    .ok()
                    .and_then(|value| {
                        serde_json::from_value::<arkret::MlsGenesisPayload>(value).ok()
                    })
                    .is_some_and(|genesis| genesis.mls_group_id() == payload.mls_group_id())
        })
        .context("Welcome group has no accepted Genesis")
}

/// Bind the exact two participant leaves of an accepted controller/owned-Agent
/// founding unit. The controller leaf is only accepted after its own claim
/// envelope signature verifies under the actual RFC 9420 leaf key.
fn install_owned_agent_leaf_bindings(
    group: &mut ArkretMlsGroup,
    checkpoint: &arkret::MlsGovernanceVerificationCheckpoint,
    welcome_event: &arkret::Event,
    payload: &MlsWelcomePayload,
    envelope: &MlsWelcomeEnvelope,
    genesis: &arkret::Event,
    agent_id: &DidCoreId,
) -> anyhow::Result<()> {
    let agent_id = agent_id.as_str();
    let mut founding = checkpoint
        .accepted_events
        .iter()
        .filter(|accepted| {
            accepted.realm_id == checkpoint.realm_id
                && accepted.actor_id == payload.claim_envelope.requester_actor_id
                && accepted.actor_seq < 4
        })
        .collect::<Vec<_>>();
    founding.sort_by_key(|accepted| accepted.actor_seq);
    let unit: [&arkret::Event; 4] = founding
        .try_into()
        .map_err(|_| anyhow::anyhow!("Welcome lacks the complete accepted founding unit"))?;
    let plan = arkret::DirectConversationFoundingPlan::from_events(unit)?;
    let peer: arkret::MembershipPayload =
        serde_json::from_value(serde_json::to_value(&unit[2].payload)?)?;
    let controller_binding = peer
        .agent_controller_binding
        .as_ref()
        .context("Agent founding membership lacks its explicit controller binding")?;
    anyhow::ensure!(
        plan.realm_id == checkpoint.realm_id
            && peer.member_id.signing_principal_id().as_str() == agent_id
            && peer.member_id.route_service_id() == &payload.claim_receipt.destination_id
            && unit[0].actor_id == payload.claim_envelope.requester_actor_id
            && unit[0].actor_id.as_account_id() == Some(&controller_binding.controller_account_id),
        "Welcome does not bind the accepted controller-Agent founding pair"
    );
    anyhow::ensure!(
        genesis.actor_id == payload.claim_envelope.requester_actor_id
            && welcome_event.actor_id == genesis.actor_id,
        "owned-Agent Welcome must be authored by the original controller"
    );
    let arkret::MlsRequesterTrustBinding::RequesterDevice {
        requester_device_id,
        requester_device_authorize_event_id,
    } = &payload.claim_envelope.trust_binding
    else {
        anyhow::bail!("owned-Agent Welcome requires its controller device authority");
    };
    let leaves = group.active_author_leaves();
    anyhow::ensure!(
        leaves.len() == 2,
        "owned-Agent admission requires the exact two participant leaves"
    );
    let agent_endpoint = envelope.recipient.clone();
    let agent_actor = ActorId::account(AccountId::new(
        DidCoreId::new(agent_id)?,
        payload.claim_receipt.destination_id.clone(),
    ));
    let mut bindings = Vec::new();
    for leaf in leaves {
        let arkret::AuthorLeafCredential::Basic { identity } = leaf.credential else {
            anyhow::bail!("MLS leaf is not BasicCredential");
        };
        let (actor_id, endpoint, device_authorize_event_id) = if identity == agent_id.as_bytes() {
            (agent_actor.clone(), agent_endpoint.clone(), None)
        } else {
            anyhow::ensure!(
                identity == requester_device_id.as_str().as_bytes(),
                "MLS leaf does not match the controller device"
            );
            let key: [u8; 32] = leaf
                .signature_key
                .as_slice()
                .try_into()
                .context("controller leaf key is not Ed25519")?;
            let signature = ed25519_dalek::Signature::from_slice(&arkret::base64url_decode(
                payload.claim_envelope.signature.sig.as_str().as_bytes(),
            )?)?;
            ed25519_dalek::VerifyingKey::from_bytes(&key)?.verify_strict(
                &payload
                    .claim_envelope
                    .canonical_signing_bytes(&payload.claim_receipt)?,
                &signature,
            )?;
            (
                genesis.actor_id.clone(),
                MlsEndpointIdentity::human_device(
                    genesis.actor_id.signing_principal_id().clone(),
                    requester_device_id.clone(),
                ),
                Some(requester_device_authorize_event_id.clone()),
            )
        };
        bindings.push(arkret::mls::MlsVerifiedLeafBinding {
            leaf_index: leaf.leaf_index,
            actor_id,
            endpoint,
            credential_ref: arkret::NonEmptyString::new(String::from_utf8(identity)?)
                .map_err(anyhow::Error::msg)?,
            signature_key: arkret::Base64UrlString::new(arkret::base64url_encode(
                &leaf.signature_key,
            ))
            .map_err(anyhow::Error::msg)?,
            device_authorize_event_id,
        });
    }
    anyhow::ensure!(
        bindings
            .iter()
            .filter(|binding| binding.actor_id == agent_actor)
            .count()
            == 1,
        "owned-Agent admission requires exactly one Agent leaf"
    );
    group.install_verified_leaf_bindings(bindings)?;
    Ok(())
}

/// Bind every occupied leaf of the ordinary Realm group an Applet Bot has just
/// joined. `applet-integration.md` §12 forbids joining an E2EE group on the
/// strength of the Welcome alone, so the separate accepted E2EE join
/// authorization is required first, and the Bot's own leaf must be the exact
/// endpoint this runtime holds.
#[allow(clippy::too_many_arguments)]
fn install_applet_bot_leaf_bindings(
    group: &mut ArkretMlsGroup,
    checkpoint: &arkret::MlsGovernanceVerificationCheckpoint,
    welcome_event: &arkret::Event,
    payload: &MlsWelcomePayload,
    bot_account_id: &AccountId,
    device_id: &DeviceId,
    applet_id: &str,
    install_authorization_ref: &str,
    accepted_leaf_authority: &BTreeMap<EventId, arkret::MlsAcceptedArtifactOutcome>,
) -> anyhow::Result<()> {
    let bot_actor = ActorId::account(bot_account_id.clone());
    anyhow::ensure!(
        welcome_event.actor_id == payload.claim_envelope.requester_actor_id,
        "Applet Bot Welcome is not authored by the accepted KeyPackage claim requester"
    );
    require_applet_e2ee_join_authorization(
        checkpoint,
        &bot_actor,
        applet_id,
        install_authorization_ref,
    )?;
    let accepted = accepted_leaf_authority
        .get(&welcome_event.event_id)
        .context("Applet Bot Welcome has no accepted MLS leaf authority")?;
    anyhow::ensure!(
        accepted.transition_head.transition_ref == payload.commit_ref
            && accepted.governance_binding == payload.governance_binding,
        "accepted MLS leaf authority names another transition"
    );
    group.install_accepted_leaf_bindings(accepted)?;
    let bot_endpoint =
        MlsEndpointIdentity::human_device(bot_account_id.principal_id.clone(), device_id.clone());
    anyhow::ensure!(
        group
            .verified_leaf_bindings()?
            .iter()
            .filter(|binding| binding.actor_id == bot_actor && binding.endpoint == bot_endpoint)
            .count()
            == 1,
        "Applet Bot admission requires exactly one accepted Bot leaf"
    );
    Ok(())
}

/// `applet-integration.md` §12: an Applet-managed member joins an E2EE group
/// only through a separate, auditable join authorization carrying Applet
/// provenance. The verified closure must therefore contain an accepted
/// `ak.member.state` join for the exact Bot Actor that cites this
/// installation's `applet_id` and grant, and the Bot must not have issued it.
fn require_applet_e2ee_join_authorization(
    checkpoint: &arkret::MlsGovernanceVerificationCheckpoint,
    bot_actor: &ActorId,
    applet_id: &str,
    install_authorization_ref: &str,
) -> anyhow::Result<()> {
    let authorized = checkpoint.accepted_events.iter().any(|accepted| {
        if accepted.kind != arkret::EventKind::MemberState
            || accepted.realm_id != checkpoint.realm_id
            || &accepted.actor_id == bot_actor
            || accepted
                .applet_id
                .as_ref()
                .is_none_or(|id| id.as_str() != applet_id)
            || accepted
                .authorization_ref
                .as_ref()
                .is_none_or(|grant| grant.as_str() != install_authorization_ref)
        {
            return false;
        }
        serde_json::to_value(&accepted.payload)
            .ok()
            .and_then(|value| serde_json::from_value::<arkret::MembershipPayload>(value).ok())
            .is_some_and(|membership| {
                membership.membership == arkret::MembershipPayloadState::Join
                    && &membership.member_id == bot_actor
            })
    });
    anyhow::ensure!(
        authorized,
        "Applet Bot MLS admission requires its separate accepted E2EE join authorization"
    );
    Ok(())
}

fn mls_welcome_envelope(payload: &MlsWelcomePayload) -> anyhow::Result<MlsWelcomeEnvelope> {
    // The shared codec validates the closed carrier and its byte digest.
    let payload: MlsWelcomePayload = serde_json::from_value(serde_json::to_value(payload)?)?;
    let recipient = match &payload.recipient {
        MlsWelcomeRecipient::Device {
            recipient_device_id,
        } => MlsEndpointIdentity::human_device(
            payload
                .recipient_principal_id
                .clone()
                .context("Welcome principal is missing")?,
            recipient_device_id.clone(),
        ),
        MlsWelcomeRecipient::Agent {
            recipient_agent_id,
            recipient_agent_verification_method,
            agent_key_authorize_event_id,
        } => MlsEndpointIdentity::agent_runtime(
            recipient_agent_id.clone(),
            recipient_agent_verification_method.clone(),
            agent_key_authorize_event_id.clone(),
        )?,
        MlsWelcomeRecipient::MinimalMetadataPairwise {
            recipient_pairwise_actor_id,
            recipient_pairwise_verification_method,
        } => MlsEndpointIdentity::minimal_metadata_pairwise(
            recipient_pairwise_actor_id.clone(),
            recipient_pairwise_verification_method.clone(),
        )?,
    };
    Ok(MlsWelcomeEnvelope {
        group_id: payload.mls_group_id().to_owned(),
        epoch: payload.epoch(),
        recipient,
        welcome: payload.carrier.ciphertext(),
        welcome_hash: payload.claim_envelope.welcome_digest.clone(),
        ratchet_tree: None,
    })
}

/// Stage only after the caller has verified the exact accepted Event and MLS leaves.
/// The caller commits this cache together with the verified group snapshot.
/// Persist a verified Welcome together with the member attribution that was
/// derived for it, so a later replay of the staged Welcome can restore the
/// same authority instead of persisting an unattributed group.
fn stage_verified_welcome(
    state: &mut ArkretCryptoStateFile,
    store: &mut MemoryCryptoStore,
    payload: &MlsWelcomePayload,
    welcome_ref: &EventId,
    verified_leaf_bindings: Vec<arkret::mls::MlsVerifiedLeafBinding>,
) -> anyhow::Result<MlsWelcomeEnvelope> {
    let welcome = mls_welcome_envelope(payload)?;
    let mut binding = ArkretMlsWelcomeConsumeBinding {
        keypackage_ref: payload.keypackage_ref.clone(),
        claim_id: payload.claim_id.to_string(),
        welcome_ref: Some(welcome_ref.to_string()),
        realm_id: Some(payload.claim_envelope.intended_realm_id.to_string()),
        strand_id: None,
        mls_group_id: payload.mls_group_id().to_owned(),
        epoch: payload.epoch(),
        group_state_ref: Some(payload.commit_ref.to_string()),
        recipient_durable_receipt: None,
        verified_leaf_bindings,
    };
    enrich_mls_welcome_consume_binding(&mut binding, &state.direct_conversation_welcome_bindings);
    store
        .put_welcome(welcome.clone())
        .map_err(anyhow::Error::msg)?;
    state.bootstrap.insert(
        welcome.group_id.clone(),
        ArkretBootstrapRecord {
            group_id: welcome.group_id.clone(),
            required_epoch: welcome.epoch,
            local_epoch: store
                .mls_group_state(&welcome.group_id)
                .map(|record| record.epoch),
            group_state_ref: binding.group_state_ref.clone(),
            action: MlsRecoveryAction::ConsumeWelcome,
            updated_at: Utc::now(),
        },
    );
    if let Some(key) =
        find_mls_key_package_cache_key(&state.mls_key_packages, &binding.keypackage_ref)
        && let Some(record) = state.mls_key_packages.get_mut(&key)
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
    Ok(welcome)
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

/// Restore the member attribution recorded when this Welcome was staged.
///
/// A staged Welcome without it cannot be replayed: the SDK fails closed on
/// `export_state_record`, and inventing bindings here would put an
/// unauthorized roster behind every later decrypt and Commit.
fn staged_welcome_leaf_bindings(
    state: &ArkretCryptoStateFile,
    welcome: &MlsWelcomeEnvelope,
) -> anyhow::Result<Vec<arkret::mls::MlsVerifiedLeafBinding>> {
    let bindings = state
        .mls_welcome_consume_bindings
        .values()
        .find(|binding| {
            binding.mls_group_id == welcome.group_id
                && binding.epoch == welcome.epoch
                && !binding.verified_leaf_bindings.is_empty()
        })
        .map(|binding| binding.verified_leaf_bindings.clone())
        .with_context(|| {
            format!(
                "staged Arkret MLS Welcome for group '{}' at epoch {} carries no verified leaf authority",
                welcome.group_id, welcome.epoch
            )
        })?;
    Ok(bindings)
}

fn try_consume_stored_welcome_for_payload(
    state: &ArkretCryptoStateFile,
    store: &mut MemoryCryptoStore,
    payload: &EncryptedPayload,
) -> anyhow::Result<Option<(MlsGroupStateRecord, MlsWelcomeEnvelope)>> {
    let mut candidates = Vec::new();
    for identity_record in state.mls_identities.values() {
        let endpoint = restore_mls_identity(identity_record)?.endpoint_identity();
        candidates.extend(
            store
                .welcomes_for_endpoint(&endpoint)
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
            Ok(mut group) => {
                let installed =
                    staged_welcome_leaf_bindings(state, &welcome).and_then(|bindings| {
                        group
                            .install_verified_leaf_bindings(bindings)
                            .map_err(|err| {
                                anyhow::anyhow!("install staged MLS leaf authority: {err}")
                            })
                    });
                if let Err(err) = installed {
                    last_error = Some(err);
                    continue;
                }
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
    next_epoch: u64,
    accepted_event_ref: &EventId,
) -> anyhow::Result<()> {
    let mut candidates = Vec::new();
    for identity_record in state.mls_identities.values() {
        let endpoint = restore_mls_identity(identity_record)?.endpoint_identity();
        candidates.extend(
            store
                .welcomes_for_endpoint(&endpoint)
                .into_iter()
                .filter(|welcome| {
                    welcome.group_id == group_id
                        && (welcome.epoch <= base_epoch
                            || (welcome.epoch == next_epoch
                                && state.mls_welcome_consume_bindings.values().any(|binding| {
                                    binding.mls_group_id == group_id
                                        && binding.epoch == next_epoch
                                        && binding.group_state_ref.as_deref()
                                            == Some(accepted_event_ref.as_str())
                                })))
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
            Ok(mut group) if group.epoch() == base_epoch || group.epoch() == next_epoch => {
                let installed =
                    staged_welcome_leaf_bindings(state, &welcome).and_then(|bindings| {
                        group
                            .install_verified_leaf_bindings(bindings)
                            .map_err(|err| {
                                anyhow::anyhow!("install staged MLS leaf authority: {err}")
                            })
                    });
                if let Err(err) = installed {
                    last_error = Some(err);
                    continue;
                }
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

    use arkret::{EncryptedPayloadScheme, Hash, KeyOperationSignature, KeyPackageClaimRecord};
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
    static FIXTURE_STRAND: LazyLock<StrandId> =
        LazyLock::new(|| StrandId::from_event_id(&fixture_event_id(0x04)));
    static FIXTURE_EVENT_1: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x51));
    static FIXTURE_EVENT_2: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x52));
    static FIXTURE_EVENT_6: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x56));
    static FIXTURE_EVENT_9: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x59));
    static FIXTURE_EVENT_13: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x5d));
    static FIXTURE_EVENT_14: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x5e));
    static FIXTURE_EVENT_31: LazyLock<EventId> = LazyLock::new(|| fixture_event_id(0x71));

    fn content_header(
        group: &ArkretMlsGroup,
        realm_id: &str,
        group_state_ref: &str,
        content_type: &str,
    ) -> EventContentPreEncryptionHeader {
        EventContentPreEncryptionHeader::reconstruct(
            "1.0",
            content_type,
            EncryptedPayloadScheme::MlsRfc9420,
            ScopeRef::Realm {
                realm_id: RealmId::new(realm_id.to_owned()).unwrap(),
            },
            "ak.message.create",
            group.epoch(),
            EventId::new(group_state_ref.to_owned()).unwrap(),
            group.local_content_sender_domain().unwrap(),
            None,
            EventContentRoutingContext::None,
        )
        .unwrap()
    }

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
        let device = "ak:device:01904100-0000-7000-8000-00000000000f";
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
        let device = "ak:device:01904100-0000-7000-8000-000000000012";
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
                .contains("authorized runtime endpoint")
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
                .contains("authorized runtime endpoint")
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
        let device = "ak:device:01904100-0000-7000-8000-000000000010";
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
            content_type: CONTENT_BLOCK_JSON.to_owned(),
            ciphertext: "abc".to_owned(),
            counter: None,
            pre_encryption_header: EventContentPreEncryptionHeader::reconstruct(
                "1.0",
                CONTENT_BLOCK_JSON,
                EncryptedPayloadScheme::MlsRfc9420,
                effective_scope,
                "ak.message.create",
                3,
                EventId::new(FIXTURE_EVENT_2.as_str()).unwrap(),
                "ak:device:01904100-0000-7000-8000-000000000003",
                None,
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

    /// Outbound Sidecar reply metadata: the binding plaintext built by
    /// `build_user_facing_response_metadata` must round-trip through the same
    /// MLS group used for `encrypted_content`, and the resulting envelope is a
    /// ciphertext carrier whose plaintext parses back into a valid
    /// `user_facing_response` binding.
    /// Known red: `consume_stored_welcome_for_commit` joins from a staged
    /// Welcome and calls `persist_state` without first installing the accepted
    /// transition leaf bindings, so the SDK refuses to export the state record
    /// ("MLS member attribution is unavailable until accepted transition
    /// bindings are installed"). Only `admit_verified_welcomes` installs them.
    /// Do not weaken this assertion; the recovery path needs the accepted leaf
    /// authority plumbed through `apply_mls_commit`.
    #[test]
    fn sidecar_reply_metadata_encrypts_and_round_trips_through_group() {
        use super::super::sidecar::{
            SidecarExchangeContext, build_user_facing_response_metadata,
            sidecar_binding_from_metadata_plaintext,
        };

        let home = temp_home("sidecar-metadata-encrypt");
        let realm_id = FIXTURE_REALM.as_str();
        let bob_store = FileArkretCryptoStore::for_account(&home, "c1", "bob");
        let bob_key_package = bob_store
            .ensure_mls_key_package(
                &test_account("ak:did_core:webvh:z6mkfixturebob"),
                "ak:device:01904100-0000-7000-8000-00000000000e",
            )
            .expect("Bob KeyPackage should be stored");

        let alice = new_human_mls_identity(
            DidCoreId::new("ak:did_core:webvh:z6mkfixturealice").unwrap(),
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006").unwrap(),
        )
        .unwrap();
        let mut alice_group = alice.create_group(realm_id.as_bytes()).unwrap();
        let add = alice_group.add_member(&bob_key_package).unwrap();
        let group_state_ref = "ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6";
        let payload = test_welcome_payload(
            &bob_key_package,
            &add.welcome,
            realm_id,
            group_state_ref,
            None,
        );
        persist_test_accepted_welcome(
            &bob_store,
            &payload,
            "ak:event:ARELvWOpF6BRrks3DlbQy-9XIE6aAQQumDQp7fA4ApeM",
            fixture_leaf_bindings(
                &alice_group,
                &[
                    MlsEndpointIdentity::human_device(
                        DidCoreId::new("ak:did_core:webvh:z6mkfixturealice").unwrap(),
                        DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006").unwrap(),
                    ),
                    bob_key_package.endpoint.clone(),
                ],
                "ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6",
            ),
        );

        // Bob joins by decrypting one inbound payload, which also seeds the
        // bootstrap record (group_state_ref) that outbound encryption needs.
        let inbound = alice_group
            .encrypt_payload(
                content_header(&alice_group, realm_id, group_state_ref, CONTENT_BLOCK_JSON),
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
                "ak:did_core:webvh:z6mkfixturebob",
                "ak:device:01904100-0000-7000-8000-00000000000e",
                &inbound,
            )
            .expect("planning with local state should preserve the verified commit ref");
        // The verified commit ref is the one the ciphertext header names, not a
        // separate fixture constant: comparing against `group_state_ref` keeps
        // the assertion from drifting away from the payload it is about.
        assert_eq!(bootstrap.group_state_ref.as_deref(), Some(group_state_ref));
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
            request_event_id: FIXTURE_EVENT_31.as_str().to_owned(),
            coordinator_assignment_event_id: Some(FIXTURE_EVENT_31.as_str().to_owned()),
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
        let envelope: arkret::EncryptedEnvelope =
            serde_json::from_value(envelope_value).expect("envelope shape");
        let header = envelope
            .reconstruct_pre_encryption_header(
                EncryptedPayloadScheme::MlsRfc9420,
                ScopeRef::Realm {
                    realm_id: RealmId::new(realm_id.to_owned()).unwrap(),
                },
                "ak.message.create",
                "ak:device:01904100-0000-7000-8000-00000000000e",
                None,
            )
            .expect("pre-encryption header");
        let payload =
            arkret::mls::encrypted_envelope_to_payload_with_verified_header(&envelope, header)
                .expect("payload conversion");
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
        let realm_id = "ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI";
        let agent_id = "ak:did_core:web:agent.example";
        let station_id = "ak:did_core:web:station.example";
        let agent_device = "ak:device:01904100-0000-7000-8000-00000000000e";
        let verification_method = "did:web:agent.example#runtime-1";
        let agent_key_authorize_event_id =
            EventId::new("ak:event:ARELvWOpF6BRrks3DlbQy-9XIE6aAQQumDQp7fA4ApeM").unwrap();
        let seed = [42_u8; 32];
        let key_ref = crate::arkret::ArkretKeyRef::InlineSeedBase64 {
            value: base64::engine::general_purpose::STANDARD_NO_PAD.encode(seed),
        };
        let agent_store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let agent_key_package = agent_store
            .ensure_agent_mls_key_package(
                &test_account(agent_id),
                &key_ref,
                verification_method,
                agent_key_authorize_event_id.as_str(),
            )
            .expect("Agent KeyPackage should use its authorized runtime key");

        let alice_id = DidCoreId::new("ak:did_core:web:alice.example").unwrap();
        let alice_device_id =
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006").unwrap();
        let alice = new_human_mls_identity(alice_id.clone(), alice_device_id.clone()).unwrap();
        let mut alice_group = alice.create_group(realm_id.as_bytes()).unwrap();
        let add = alice_group.add_member(&agent_key_package).unwrap();
        let group_id = add.welcome.group_id.clone();
        let group_state_ref = "ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6";
        let mut state = agent_store.load().unwrap();
        let agent_identity = restore_mls_identity(state.mls_identities.values().next().unwrap())
            .expect("Agent MLS identity should restore");
        let alice_endpoint = alice_group.identity().endpoint_identity();
        let agent_endpoint = agent_identity.endpoint_identity();
        let mut agent_group = ArkretMlsGroup::join_from_welcome(agent_identity, &add.welcome)
            .expect("Agent should join the accepted Welcome");
        let alice_actor = ActorId::account(AccountId::new(
            alice_id,
            DidCoreId::new(station_id.to_owned()).unwrap(),
        ));
        let agent_actor = ActorId::account(AccountId::new(
            DidCoreId::new(agent_id.to_owned()).unwrap(),
            DidCoreId::new(station_id.to_owned()).unwrap(),
        ));
        let device_authorize_event_id =
            EventId::new("ak:event:ASeIBHNVQyeIcU4aBIt2t2BF_ikuVMH0kNru_HgO_gG1").unwrap();
        for group in [&mut alice_group, &mut agent_group] {
            let bindings = group
                .active_author_leaves()
                .into_iter()
                .map(|leaf| {
                    let arkret::AuthorLeafCredential::Basic { identity } = leaf.credential else {
                        panic!("test MLS leaf must use BasicCredential");
                    };
                    let (actor_id, endpoint, device_authorize_event_id) =
                        if identity.as_slice() == agent_id.as_bytes() {
                            (agent_actor.clone(), agent_endpoint.clone(), None)
                        } else {
                            assert_eq!(identity.as_slice(), alice_device_id.as_str().as_bytes());
                            (
                                alice_actor.clone(),
                                alice_endpoint.clone(),
                                Some(device_authorize_event_id.clone()),
                            )
                        };
                    arkret::mls::MlsVerifiedLeafBinding {
                        leaf_index: leaf.leaf_index,
                        actor_id,
                        endpoint,
                        credential_ref: arkret::NonEmptyString::new(
                            String::from_utf8(identity).unwrap(),
                        )
                        .unwrap(),
                        signature_key: arkret::Base64UrlString::new(
                            arkret::canonical::base64url_encode(&leaf.signature_key),
                        )
                        .unwrap(),
                        device_authorize_event_id,
                    }
                })
                .collect();
            group
                .install_verified_leaf_bindings(bindings)
                .expect("accepted transition should bind every occupied leaf");
        }
        let mut store = state.mls_store().unwrap();
        let accepted = agent_group.persist_state(&mut store).unwrap();
        state.set_mls_store(&store).unwrap();
        state.bootstrap.insert(
            accepted.group_id.clone(),
            ArkretBootstrapRecord {
                group_id: accepted.group_id,
                required_epoch: accepted.epoch,
                local_epoch: Some(accepted.epoch),
                group_state_ref: Some(group_state_ref.to_owned()),
                action: MlsRecoveryAction::UseLocalState,
                updated_at: Utc::now(),
            },
        );
        agent_store.save(&mut state).unwrap();
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
        let authority_head = arkret::CommitStreamHead {
            stream_ref: arkret::CommitStreamRef::Realm {
                realm_id: RealmId::new(realm_id).unwrap(),
            },
            stream_position: 3,
            commit_id: arkret::RealmCommitId::from_digest([45; 32]),
        };
        let first = agent_store
            .seal_online_presence_signal(
                realm_id,
                agent_actor.as_account_id().expect("Agent account actor"),
                verification_method,
                &key_ref,
                &authority_head,
                sent_at,
            )
            .expect("first presence heartbeat should seal");
        let second = agent_store
            .seal_online_presence_signal(
                realm_id,
                agent_actor.as_account_id().expect("Agent account actor"),
                verification_method,
                &key_ref,
                &authority_head,
                sent_at + chrono::Duration::seconds(20),
            )
            .expect("second presence heartbeat should seal");

        let verifying_key = ed25519_dalek::SigningKey::from_bytes(&seed).verifying_key();
        let public_key = arkret_signatures::PublicKeyMaterial::Ed25519Raw {
            bytes: verifying_key.to_bytes().to_vec(),
        };
        arkret_signatures::verify_ed25519_signal_proof(&first, &public_key)
            .expect("first Signal proof should verify");
        arkret_signatures::verify_ed25519_signal_proof(&second, &public_key)
            .expect("second Signal proof should verify");
        assert!(first.sender_device_id.is_none());
        assert!(second.sender_device_id.is_none());
        assert_eq!(first.authority_commit_id, authority_head.commit_id);
        let wire = serde_json::to_value(&first).unwrap();
        assert!(wire.get("seal_ref").is_none());
        assert!(wire["encrypted_payload"].get("aad_digest").is_none());
        assert!(
            wire["encrypted_payload"]["key_ref"]
                .get("algorithm")
                .is_none()
        );
        assert_ne!(
            first.encrypted_payload.nonce, second.encrypted_payload.nonce,
            "each refresh must spend a distinct MLS Signal nonce"
        );

        let mut replay = arkret::AeadNonceReplayTracker::new();
        for (expected_sequence, envelope) in [(0, &first), (1, &second)] {
            let authority = arkret::mls::SignalSenderAuthority::Agent {
                public_key: &public_key,
                verification_method: &envelope.proof.verification_method,
                agent_key_authorize_event_id: &agent_key_authorize_event_id,
            };
            let plaintext = alice_group
                .open_signal_envelope(
                    envelope,
                    authority,
                    group_state_ref,
                    &authority_head.commit_id,
                    &mut replay,
                )
                .expect("peer should decrypt encrypted presence");
            let arkret::SignalPlaintext::Presence(presence) =
                arkret::open_signal_plaintext(&plaintext).expect("presence plaintext should open")
            else {
                panic!("Signal plaintext must be ak.presence");
            };
            assert_eq!(presence.payload_sequence, expected_sequence);
            assert_eq!(presence.actor_id, agent_actor);
            assert_eq!(presence.state, PresenceState::Online);
            assert_eq!(presence.ttl_ms, 30_000);
        }
        let sequence_key = SignalSequenceDomain::from_verified_envelope(
            &first,
            Some(public_key.raw_ed25519_digest().unwrap()),
        )
        .unwrap()
        .canonical_key()
        .unwrap();
        assert_eq!(
            agent_store
                .load()
                .unwrap()
                .signal_sequences
                .get(&sequence_key),
            Some(&2)
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
            Some(encrypted_payload().group_id)
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
            claim_id: "ak:claim:test-claim-record".to_owned(),
            keypackage_ref: "ak:mls:keypackage:test-claim-record".to_owned(),
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
        assert_eq!(claimed.actor_id, claim.actor_id);
        let mut wrong_principal = claim.clone();
        wrong_principal.principal_id = DidCoreId::new("ak:did_core:web:other.example").unwrap();
        assert!(mls_key_package_record_from_claim(&wrong_principal).is_err());

        let alice = new_human_mls_identity(
            DidCoreId::new("ak:did_core:webvh:z6mkfixturealice".to_owned()).unwrap(),
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006".to_owned()).unwrap(),
        )
        .unwrap();
        let mut alice_group = alice
            .create_group(FIXTURE_CLAIM_REALM.as_str().as_bytes())
            .unwrap();
        let add = alice_group
            .add_member(&claimed)
            .expect("claimed KeyPackage should add to MLS group");
        assert_eq!(add.welcome.recipient.actor_id().as_str(), bob_principal);
        assert_eq!(
            endpoint_human_device_id(&add.welcome.recipient).map(DeviceId::as_str),
            Some(bob_device)
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A staged Welcome plus the durable Commit that follows it must be enough
    /// to advance a receiver that has seen no application data yet, and the
    /// post-Commit snapshot must stay fully attributed: `apply_commit` drops
    /// every leaf binding, so the Station's accepted leaf authority for the
    /// entered epoch is what makes the group persistable again.
    #[test]
    fn stored_welcome_admits_group_and_decrypts_content_block() {
        let home = temp_home("welcome-admit");
        let bob_store = FileArkretCryptoStore::for_account(&home, "c1", "bob");
        let bob_principal = "ak:did_core:webvh:z6mkfixturebob";
        let bob_device = "ak:device:01904100-0000-7000-8000-00000000000e";
        let bob_key_package = bob_store
            .ensure_mls_key_package(&test_account(bob_principal), bob_device)
            .expect("Bob KeyPackage should be stored with private identity state");
        let state = bob_store.load().expect("state should load");
        assert_eq!(state.mls_identities.len(), 1);
        assert_eq!(state.mls_key_packages.len(), 1);

        let alice = new_human_mls_identity(
            DidCoreId::new("ak:did_core:webvh:z6mkfixturealice").unwrap(),
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006").unwrap(),
        )
        .unwrap();
        let realm_id = "ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI";
        let binding_for = |previous: u64, next: u64| {
            arkret::MlsGovernanceBindingPayload::realm(
                RealmId::new(realm_id.to_owned()).unwrap(),
                previous,
                next,
                Hash::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
                arkret::ContentScheme::MlsRfc9420,
                None,
                arkret::ProfileId::MLS_GOVERNANCE_BINDING_FULL_V1,
                "arkret.reducer.v1",
            )
            .unwrap()
        };
        let mut alice_group = alice
            .create_group_with_governance_binding(realm_id.as_bytes(), &binding_for(0, 0))
            .unwrap();
        let add = alice_group
            .add_member_with_governance_binding(&bob_key_package, &binding_for(0, 1))
            .unwrap();
        let alice_endpoint = MlsEndpointIdentity::human_device(
            DidCoreId::new("ak:did_core:webvh:z6mkfixturealice").unwrap(),
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006").unwrap(),
        );
        let welcome_endpoints = [alice_endpoint.clone(), bob_key_package.endpoint.clone()];
        let expected_binding = ArkretMlsWelcomeConsumeBinding {
            keypackage_ref: bob_key_package.keypackage_ref.as_str().to_owned(),
            claim_id: "claim-agent-welcome-001".to_owned(),
            welcome_ref: Some("ak:event:ARELvWOpF6BRrks3DlbQy-9XIE6aAQQumDQp7fA4ApeM".to_owned()),
            realm_id: Some("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI".to_owned()),
            strand_id: None,
            mls_group_id: add.welcome.group_id.clone(),
            epoch: add.welcome.epoch,
            group_state_ref: Some(
                "ak:event:AbnHJt4q4qY18zqvLiy3Emmqy7weTAuApx42RmRgPr2h".to_owned(),
            ),
            recipient_durable_receipt: None,
            verified_leaf_bindings: Vec::new(),
        };
        let payload = test_welcome_payload(
            &bob_key_package,
            &add.welcome,
            realm_id,
            expected_binding.group_state_ref.as_deref().unwrap(),
            Some(binding_for(0, 1)),
        );
        let recorded = persist_test_accepted_welcome(
            &bob_store,
            &payload,
            expected_binding.welcome_ref.as_deref().unwrap(),
            fixture_leaf_bindings(
                &alice_group,
                &welcome_endpoints,
                "ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6",
            ),
        );
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
        let commit_binding = binding_for(expected_binding.epoch, expected_binding.epoch + 1);
        let commit = alice_group
            .update_governance_binding(&commit_binding)
            .expect("Alice governance-bound Commit should build");
        let commit_event =
            EventId::new("ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6".to_owned())
                .unwrap();
        let commit_payload = MlsCommitPayload::new(
            expected_binding.group_state_ref.as_deref().unwrap(),
            Vec::new(),
            &commit,
            commit_binding.clone(),
        )
        .unwrap();
        let commit_authority = fixture_accepted_artifact(
            &commit_event,
            commit_binding,
            &fixture_leaf_bindings(
                &alice_group,
                &welcome_endpoints,
                "ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6",
            ),
        );
        assert!(
            bob_store
                .mls_commit_needs_accepted_leaf_authority(&commit_payload)
                .expect("local MLS state should be readable")
        );
        assert!(
            bob_store
                .apply_mls_commit(&commit_payload, &commit_event, Some(&commit_authority))
                .expect("Bob should consume Welcome and apply Commit")
        );
        // An already-applied Commit needs no authority and writes no new state.
        assert!(
            !bob_store
                .mls_commit_needs_accepted_leaf_authority(&commit_payload)
                .expect("local MLS state should be readable")
        );
        assert!(
            !bob_store
                .apply_mls_commit(&commit_payload, &commit_event, None)
                .expect("accepted Commit replay should be idempotent")
        );

        let content = json!({"kind":"ak.content.text","body":"secret"});
        let payload = alice_group
            .encrypt_payload(
                content_header(
                    &alice_group,
                    realm_id,
                    commit_event.as_str(),
                    CONTENT_BLOCK_JSON,
                ),
                &serde_json::to_vec(&content).unwrap(),
            )
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
                content_header(
                    &alice_group,
                    realm_id,
                    commit_event.as_str(),
                    CONTENT_BLOCK_JSON,
                ),
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

    const APPLET_REALM_ID: &str = "ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI";
    const APPLET_BOT_PRINCIPAL: &str = "ak:did_core:web:bridge.example:bot";
    const APPLET_BOT_DEVICE: &str = "ak:device:01904100-0000-7000-8000-0000000000a1";
    const APPLET_BOT_STATION: &str = "ak:did_core:web:service.example";
    const APPLET_INVITER_PRINCIPAL: &str = "ak:did_core:web:owner.example";
    const APPLET_INVITER_STATION: &str = "ak:did_core:web:principal-server.example";
    const APPLET_INVITER_DEVICE: &str = "ak:device:01904100-0000-7000-8000-000000000006";
    const APPLET_ADMIN_PRINCIPAL: &str = "ak:did_core:web:admin.example";
    const APPLET_ID: &str = "ak:applet:21532600-0000-7000-8000-000000000000";
    const APPLET_GRANT_ID: &str = "ak:grant:AdIAmf-J5rIPxEomGXwJblJdhNg-TllVN8uRTI85EUIM";

    struct AppletWelcomeFixture {
        home: PathBuf,
        store: FileArkretCryptoStore,
        subject: MlsWelcomeAdmissionSubject,
        checkpoint: arkret::MlsGovernanceVerificationCheckpoint,
        leaf_authority: BTreeMap<EventId, arkret::MlsAcceptedArtifactOutcome>,
        inviter_group: ArkretMlsGroup,
        commit_event_id: EventId,
        mls_group_id: String,
    }

    fn applet_bot_account_id() -> AccountId {
        AccountId::new(
            DidCoreId::new(APPLET_BOT_PRINCIPAL).unwrap(),
            DidCoreId::new(APPLET_BOT_STATION).unwrap(),
        )
    }

    fn applet_checkpoint_event(
        kind: &str,
        actor: &ActorId,
        actor_seq: u64,
        payload: Value,
    ) -> arkret::Event {
        arkret_wire::test_support::raw_event_for_actor_at(
            kind,
            ScopeRef::Realm {
                realm_id: RealmId::new(APPLET_REALM_ID).unwrap(),
            },
            actor.clone(),
            actor_seq,
            arkret::Hlc::new("000000000000-0000-00000000").unwrap(),
            payload,
            Utc::now(),
        )
        .expect("checkpoint fixture Event")
    }

    /// One inviter device plus the Applet Bot, joined through an accepted
    /// Commit/Welcome pair and authorized by a separate `ak.member.state` join
    /// carrying Applet provenance.
    fn applet_welcome_fixture(label: &str) -> AppletWelcomeFixture {
        let home = temp_home(label);
        let store = FileArkretCryptoStore::for_applet(&home, "applet-1");
        let bot_key_package = store
            .ensure_mls_key_package(&test_account(APPLET_BOT_PRINCIPAL), APPLET_BOT_DEVICE)
            .expect("Applet Bot KeyPackage");
        let realm_id = RealmId::new(APPLET_REALM_ID).unwrap();
        let hash =
            |marker: char| Hash::new(format!("sha256:{}", marker.to_string().repeat(64))).unwrap();
        let genesis_binding = arkret::MlsGovernanceBindingPayload::realm(
            realm_id.clone(),
            0,
            0,
            hash('a'),
            arkret::ContentScheme::MlsRfc9420,
            None,
            arkret::ProfileId::MLS_GOVERNANCE_BINDING_FULL_V1,
            "arkret.reducer.v1",
        )
        .expect("genesis binding");
        let join_binding = arkret::MlsGovernanceBindingPayload::realm(
            realm_id.clone(),
            0,
            1,
            hash('b'),
            arkret::ContentScheme::MlsRfc9420,
            None,
            arkret::ProfileId::MLS_GOVERNANCE_BINDING_FULL_V1,
            "arkret.reducer.v1",
        )
        .expect("join binding");
        let inviter = new_human_mls_identity(
            DidCoreId::new(APPLET_INVITER_PRINCIPAL).unwrap(),
            DeviceId::new(APPLET_INVITER_DEVICE).unwrap(),
        )
        .expect("inviter identity");
        let mut inviter_group = inviter
            .create_group_with_governance_binding(APPLET_REALM_ID.as_bytes(), &genesis_binding)
            .expect("inviter group");
        let add = inviter_group
            .add_member_with_governance_binding(&bot_key_package, &join_binding)
            .expect("add Applet Bot");
        let mls_group_id = add.welcome.group_id.clone();

        let inviter_actor = ActorId::account(AccountId::new(
            DidCoreId::new(APPLET_INVITER_PRINCIPAL).unwrap(),
            DidCoreId::new(APPLET_INVITER_STATION).unwrap(),
        ));
        let commit_payload = MlsCommitPayload::new(
            "ak:event:AbnHJt4q4qY18zqvLiy3Emmqy7weTAuApx42RmRgPr2h",
            Vec::new(),
            &add.commit,
            join_binding.clone(),
        )
        .expect("Commit payload");
        let commit_event = applet_checkpoint_event(
            "ak.mls.commit",
            &inviter_actor,
            2,
            serde_json::to_value(&commit_payload).unwrap(),
        );
        let welcome_payload = test_welcome_payload(
            &bot_key_package,
            &add.welcome,
            APPLET_REALM_ID,
            commit_event.event_id.as_str(),
            Some(join_binding.clone()),
        );
        let welcome_event = applet_checkpoint_event(
            "ak.mls.welcome",
            &inviter_actor,
            3,
            serde_json::to_value(&welcome_payload).unwrap(),
        );
        let genesis_event = applet_checkpoint_event(
            "ak.mls.genesis",
            &inviter_actor,
            1,
            json!({
                "cipher_suite": "MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519",
                "group_info_ref": format!("ak:blob:sha256:{}", "1".repeat(64)),
                "ratchet_tree_ref": format!("ak:blob:sha256:{}", "2".repeat(64)),
                "governance_binding": serde_json::to_value(&genesis_binding).unwrap(),
                "created_at": "2098-12-31T23:59:30.000Z"
            }),
        );

        let bot_account_id = applet_bot_account_id();
        let bot_actor = ActorId::account(bot_account_id.clone());
        let admin_actor = ActorId::account(AccountId::new(
            DidCoreId::new(APPLET_ADMIN_PRINCIPAL).unwrap(),
            DidCoreId::new(APPLET_INVITER_STATION).unwrap(),
        ));
        let membership = arkret::MembershipPayload::join(
            realm_id.clone(),
            bot_actor.clone(),
            "applet e2ee join",
        );
        let mut join_event = applet_checkpoint_event(
            "ak.member.state",
            &admin_actor,
            1,
            serde_json::to_value(&membership).unwrap(),
        );
        join_event.applet_id = Some(arkret::AppletId::new(APPLET_ID).unwrap());
        join_event.authorization_ref =
            Some(arkret::AuthorizationRef::new(APPLET_GRANT_ID).unwrap());

        let outcome = arkret::MlsAcceptedArtifactOutcome {
            query_digest: hash('e'),
            transition_head: arkret::MlsAcceptedTransition {
                transition_ref: commit_event.event_id.clone(),
                transition_event_digest: commit_event.event_id.event_digest(),
                mls_transition_digest: hash('f'),
            },
            governance_binding: join_binding,
            mls_frontier_leaves: vec![
                arkret_wire::mls_transition::MlsSecurityFrontierLeaf {
                    leaf_index: 0,
                    actor_id: inviter_actor,
                    credential_ref: arkret::NonEmptyString::new(APPLET_INVITER_DEVICE).unwrap(),
                },
                arkret_wire::mls_transition::MlsSecurityFrontierLeaf {
                    leaf_index: 1,
                    actor_id: bot_actor,
                    credential_ref: arkret::NonEmptyString::new(APPLET_BOT_DEVICE).unwrap(),
                },
            ],
            mls_leaf_authorizations: vec![
                arkret::MlsAcceptedLeafAuthorization {
                    leaf_index: 0,
                    device_authorize_event_id: Some(
                        EventId::new("ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6")
                            .unwrap(),
                    ),
                    agent_verification_method: None,
                    agent_key_authorize_event_id: None,
                },
                arkret::MlsAcceptedLeafAuthorization {
                    leaf_index: 1,
                    device_authorize_event_id: Some(
                        EventId::new("ak:event:ARELvWOpF6BRrks3DlbQy-9XIE6aAQQumDQp7fA4ApeM")
                            .unwrap(),
                    ),
                    agent_verification_method: None,
                    agent_key_authorize_event_id: None,
                },
            ],
        };
        let mut leaf_authority = BTreeMap::new();
        leaf_authority.insert(welcome_event.event_id.clone(), outcome);

        let commit_event_id = commit_event.event_id.clone();
        let checkpoint = arkret::MlsGovernanceVerificationCheckpoint {
            realm_id,
            basis: arkret::SealBasis { leaves: Vec::new() },
            live_digest_suite: arkret::canonical::DigestSuite::Sha256,
            accepted_seals: Vec::new(),
            accepted_events: vec![genesis_event, commit_event, welcome_event, join_event],
            governance_dependencies: Vec::new(),
        };
        AppletWelcomeFixture {
            home,
            store,
            subject: MlsWelcomeAdmissionSubject::AppletBot {
                bot_account_id,
                device_id: DeviceId::new(APPLET_BOT_DEVICE).unwrap(),
                applet_id: APPLET_ID.to_owned(),
                install_authorization_ref: APPLET_GRANT_ID.to_owned(),
            },
            checkpoint,
            leaf_authority,
            inviter_group,
            commit_event_id,
            mls_group_id,
        }
    }

    fn applet_group_state_present(store: &FileArkretCryptoStore, group_id: &str) -> bool {
        store
            .load()
            .expect("state should load")
            .mls_store()
            .expect("MLS store should load")
            .mls_group_state(group_id)
            .is_some()
    }

    #[test]
    fn unverified_applet_bot_welcome_is_not_admitted() {
        let fixture = applet_welcome_fixture("applet-welcome-unverified");
        // Strip the separate Applet E2EE join authorization
        // (`applet-integration.md` §12): the Welcome alone must not admit.
        let mut checkpoint = fixture.checkpoint.clone();
        checkpoint
            .accepted_events
            .retain(|event| event.kind != arkret::EventKind::MemberState);
        let error = fixture
            .store
            .admit_verified_welcomes(&checkpoint, &fixture.subject, &fixture.leaf_authority)
            .expect_err("Welcome without its E2EE join authorization must fail closed");
        assert!(
            error.to_string().contains("E2EE join authorization"),
            "unexpected failure: {error:#}"
        );
        assert!(!applet_group_state_present(
            &fixture.store,
            &fixture.mls_group_id
        ));

        // Same Welcome, but the join authorization cites another installation.
        let mut checkpoint = fixture.checkpoint.clone();
        for event in &mut checkpoint.accepted_events {
            if event.kind == arkret::EventKind::MemberState {
                event.applet_id = Some(
                    arkret::AppletId::new("ak:applet:21532600-0000-7000-8000-0000000000ff")
                        .unwrap(),
                );
            }
        }
        assert!(
            fixture
                .store
                .admit_verified_welcomes(&checkpoint, &fixture.subject, &fixture.leaf_authority)
                .is_err()
        );
        assert!(!applet_group_state_present(
            &fixture.store,
            &fixture.mls_group_id
        ));

        // A verified Welcome addressed to another device is not this runtime's
        // Welcome: nothing is admitted and nothing is installed.
        let other_device = MlsWelcomeAdmissionSubject::AppletBot {
            bot_account_id: applet_bot_account_id(),
            device_id: DeviceId::new("ak:device:01904100-0000-7000-8000-0000000000ff").unwrap(),
            applet_id: APPLET_ID.to_owned(),
            install_authorization_ref: APPLET_GRANT_ID.to_owned(),
        };
        assert_eq!(
            fixture
                .store
                .admit_verified_welcomes(
                    &fixture.checkpoint,
                    &other_device,
                    &fixture.leaf_authority
                )
                .expect("a Welcome for another recipient is simply not ours"),
            0
        );
        assert!(!applet_group_state_present(
            &fixture.store,
            &fixture.mls_group_id
        ));
        let _ = std::fs::remove_dir_all(&fixture.home);
    }

    #[test]
    fn verified_applet_bot_welcome_is_admitted_and_survives_restart() {
        let mut fixture = applet_welcome_fixture("applet-welcome-admit");
        assert_eq!(
            fixture
                .store
                .admit_verified_welcomes(
                    &fixture.checkpoint,
                    &fixture.subject,
                    &fixture.leaf_authority
                )
                .expect("verified Applet Bot Welcome should be admitted"),
            1
        );
        assert!(applet_group_state_present(
            &fixture.store,
            &fixture.mls_group_id
        ));

        // Repeat admission of the same accepted checkpoint is a no-op.
        assert_eq!(
            fixture
                .store
                .admit_verified_welcomes(
                    &fixture.checkpoint,
                    &fixture.subject,
                    &fixture.leaf_authority
                )
                .expect("repeat admission should be idempotent"),
            0
        );

        let content = json!({"kind":"ak.content.text","body":"applet secret"});
        let payload = fixture
            .inviter_group
            .encrypt_payload(
                content_header(
                    &fixture.inviter_group,
                    APPLET_REALM_ID,
                    fixture.commit_event_id.as_str(),
                    CONTENT_BLOCK_JSON,
                ),
                &serde_json::to_vec(&content).unwrap(),
            )
            .expect("inviter should encrypt an ordinary message");

        // Reopen the persisted file exactly as a restarted process would.
        let restarted = FileArkretCryptoStore::for_applet(&fixture.home, "applet-1");
        assert_eq!(
            restarted
                .try_decrypt_content_block(&payload)
                .expect("admitted group state should decrypt after restart"),
            ArkretDecryptOutcome::Decrypted(content)
        );
        let _ = std::fs::remove_dir_all(&fixture.home);
    }

    fn test_welcome_payload(
        key_package: &MlsKeyPackageRecord,
        welcome: &MlsWelcomeEnvelope,
        realm: &str,
        commit_ref: &str,
        binding: Option<arkret::MlsGovernanceBindingPayload>,
    ) -> MlsWelcomePayload {
        use arkret::{
            Base64UrlString, MlsClaimTrustBinding, MlsGovernanceBindingPayload,
            MlsRequesterTrustBinding, MlsWelcomeCarrier, MlsWelcomeClaimEnvelope,
            MlsWelcomePayloadClaimRef, NonEmptyString,
        };
        let MlsEndpointIdentity::HumanDevice {
            principal_id: principal,
            device_id: device,
        } = &welcome.recipient
        else {
            panic!("human recipient fixture required");
        };
        let realm_id = RealmId::new(realm).unwrap();
        let hash = |marker: char| {
            Hash::new(format!("sha256:{}", marker.to_string().repeat(64))).expect("hash")
        };
        let governance_binding = binding.unwrap_or_else(|| {
            MlsGovernanceBindingPayload::realm(
                realm_id.clone(),
                0,
                welcome.epoch,
                hash('c'),
                arkret::ContentScheme::MlsRfc9420,
                None,
                arkret::ProfileId::MLS_GOVERNANCE_BINDING_FULL_V1,
                "arkret.reducer.v1",
            )
            .expect("governance binding")
        });
        let claim_id = NonEmptyString::new("claim-agent-welcome-001").expect("claim id");
        let authorize_event =
            NonEmptyString::new("ak:event:ARELvWOpF6BRrks3DlbQy-9XIE6aAQQumDQp7fA4ApeM")
                .expect("authorize event");
        let claim_ref = MlsWelcomePayloadClaimRef {
            claim_id: claim_id.clone(),
            keypackage_ref: key_package.keypackage_ref.as_str().to_owned(),
            keypackage_digest: key_package.keypackage_ref.clone(),
            capabilities_digest: hash('d'),
            trust_binding: MlsClaimTrustBinding::DeviceAuthorizeEventId(authorize_event),
        };
        let claim_envelope = MlsWelcomeClaimEnvelope {
            keypackage_ref: key_package.keypackage_ref.as_str().to_owned(),
            keypackage_digest: key_package.keypackage_ref.clone(),
            intended_realm_id: realm_id.clone(),
            claim_id: claim_id.clone(),
            requester_actor_id: ActorId::account(AccountId::new(
                DidCoreId::new("ak:did_core:web:owner.example".to_owned()).expect("requester"),
                DidCoreId::new("ak:did_core:web:principal-server.example".to_owned())
                    .expect("requester Station"),
            )),
            trust_binding: MlsRequesterTrustBinding::RequesterDevice {
                requester_device_id: DeviceId::new(
                    "ak:device:01904100-0000-7000-8000-000000000006".to_owned(),
                )
                .expect("requester device"),
                requester_device_authorize_event_id: EventId::new(
                    "ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6".to_owned(),
                )
                .expect("requester authorization"),
            },
            welcome_digest: welcome.welcome_hash.clone(),
            created_at: Utc::now(),
            signature: KeyOperationSignature {
                kid: NonEmptyString::new("did:web:owner.example#ssk-1").expect("kid"),
                signature_algorithm: Some(NonEmptyString::new("Ed25519").expect("algorithm")),
                sig: Base64UrlString::new("AQ").expect("signature"),
            },
        };
        let claim_receipt: arkret::PeerKeyPackageClaimReceipt = serde_json::from_value(json!({
            "claim_request_id": "Y2xhaW0tcmVxdWVzdC0wMDE",
            "request_digest": format!("sha256:{}", "e".repeat(64)),
            "claims_digest": format!("sha256:{}", "f".repeat(64)),
            "source_id": "ak:did_core:web:service.example",
            "destination_id": "ak:did_core:web:service.example",
            "request": {
                "claim_request_id": "Y2xhaW0tcmVxdWVzdC0wMDE",
                "target_account_id": {
                    "principal_id": principal.as_str(),
                    "station_id": "ak:did_core:web:service.example"
                },
                "requester_account_id": {
                    "principal_id": "ak:did_core:web:owner.example",
                    "station_id": "ak:did_core:web:service.example"
                },
                "intended_realm_id": realm_id.as_str(),
                "mls_group_id": welcome.group_id.clone(),
                "claim_purpose": "realm_membership",
                "required_capabilities": ["mimi.content.v1"],
                "expires_at": "2099-01-01T00:00:00.000Z",
                "target_device_ids": [device.as_str()]
            },
            "claimed_at": "2098-12-31T23:59:30.000Z",
            "expires_at": "2099-01-01T00:00:00.000Z",
            "signature": {"kid": "service-key", "sig": "AQ"}
        }))
        .expect("peer claim receipt");
        MlsWelcomePayload {
            recipient_principal_id: Some(principal.clone()),
            recipient: MlsWelcomeRecipient::Device {
                recipient_device_id: device.clone(),
            },
            sender_device_id: None,
            keypackage_ref: key_package.keypackage_ref.as_str().to_owned(),
            claim_id,
            claim_ref,
            claim_envelope,
            claim_receipt,
            carrier: MlsWelcomeCarrier::new(
                arkret::base64url_decode(welcome.welcome.as_bytes()).expect("Welcome ciphertext"),
            )
            .expect("carrier"),
            commit_ref: EventId::new(commit_ref).expect("commit ref"),
            governance_binding,
            expires_at: Utc::now() + chrono::Duration::hours(1),
        }
    }

    /// Member attribution for the transition a fixture Welcome joins, taken
    /// from the inviter's real RFC 9420 leaves.
    ///
    /// In production this comes from the Station's accepted leaf authority; the
    /// only thing the fixture supplies is the actor-to-leaf attribution the
    /// Station would assert. Every credential and signature key is the actual
    /// one, and `install_verified_leaf_bindings` re-checks both against the
    /// occupied leaf.
    fn fixture_leaf_bindings(
        group: &ArkretMlsGroup,
        endpoints: &[MlsEndpointIdentity],
        device_authorize_event_id: &str,
    ) -> Vec<arkret::mls::MlsVerifiedLeafBinding> {
        group
            .active_author_leaves()
            .into_iter()
            .map(|leaf| {
                let arkret::AuthorLeafCredential::Basic { identity } = leaf.credential else {
                    panic!("fixture MLS leaf must be a BasicCredential");
                };
                let endpoint = endpoints
                    .iter()
                    .find(|endpoint| match endpoint {
                        MlsEndpointIdentity::HumanDevice { device_id, .. } => {
                            device_id.as_str().as_bytes() == identity
                        }
                        MlsEndpointIdentity::AgentRuntime { agent_id, .. } => {
                            agent_id.as_str().as_bytes() == identity
                        }
                        MlsEndpointIdentity::MinimalMetadataPairwise {
                            pairwise_actor_id, ..
                        } => pairwise_actor_id.as_str().as_bytes() == identity,
                    })
                    .expect("fixture endpoint set must cover every occupied leaf")
                    .clone();
                let MlsEndpointIdentity::HumanDevice { principal_id, .. } = &endpoint else {
                    panic!("fixture leaf endpoint must be a human device");
                };
                arkret::mls::MlsVerifiedLeafBinding {
                    leaf_index: leaf.leaf_index,
                    actor_id: ActorId::account(AccountId::new(
                        principal_id.clone(),
                        principal_id.clone(),
                    )),
                    endpoint,
                    credential_ref: arkret::NonEmptyString::new(
                        String::from_utf8(identity).unwrap(),
                    )
                    .unwrap(),
                    signature_key: arkret::Base64UrlString::new(
                        URL_SAFE_NO_PAD.encode(&leaf.signature_key),
                    )
                    .unwrap(),
                    device_authorize_event_id: Some(
                        EventId::new(device_authorize_event_id).unwrap(),
                    ),
                }
            })
            .collect()
    }

    /// Wrap fixture leaf bindings as the Station's accepted artifact for one
    /// transition. The frontier leaves and leaf authorizations restate exactly
    /// what the bindings already carry, and the SDK re-validates both against
    /// the entered epoch's governance binding and its real leaves.
    fn fixture_accepted_artifact(
        transition_ref: &EventId,
        governance_binding: arkret::MlsGovernanceBindingPayload,
        bindings: &[arkret::mls::MlsVerifiedLeafBinding],
    ) -> arkret::MlsAcceptedArtifactOutcome {
        arkret::MlsAcceptedArtifactOutcome {
            query_digest: Hash::new(format!("sha256:{}", "e".repeat(64))).unwrap(),
            transition_head: arkret::MlsAcceptedTransition {
                transition_ref: transition_ref.clone(),
                transition_event_digest: transition_ref.event_digest(),
                mls_transition_digest: Hash::new(format!("sha256:{}", "f".repeat(64))).unwrap(),
            },
            governance_binding,
            mls_frontier_leaves: bindings
                .iter()
                .map(
                    |binding| arkret_wire::mls_transition::MlsSecurityFrontierLeaf {
                        leaf_index: binding.leaf_index,
                        actor_id: binding.actor_id.clone(),
                        credential_ref: binding.credential_ref.clone(),
                    },
                )
                .collect(),
            mls_leaf_authorizations: bindings
                .iter()
                .map(|binding| arkret::MlsAcceptedLeafAuthorization {
                    leaf_index: binding.leaf_index,
                    device_authorize_event_id: binding.device_authorize_event_id.clone(),
                    agent_verification_method: None,
                    agent_key_authorize_event_id: None,
                })
                .collect(),
        }
    }

    fn persist_test_accepted_welcome(
        store: &FileArkretCryptoStore,
        payload: &MlsWelcomePayload,
        welcome_ref: &str,
        verified_leaf_bindings: Vec<arkret::mls::MlsVerifiedLeafBinding>,
    ) -> MlsWelcomeEnvelope {
        // Exercise the post-verification staging boundary without inventing wire proofs.
        let mut state = store.load().unwrap();
        let mut crypto = state.mls_store().unwrap();
        let welcome = stage_verified_welcome(
            &mut state,
            &mut crypto,
            payload,
            &EventId::new(welcome_ref).unwrap(),
            verified_leaf_bindings,
        )
        .unwrap();
        state.set_mls_store(&crypto).unwrap();
        store.save(&mut state).unwrap();
        welcome
    }

    #[test]
    fn durable_mls_welcome_payload_is_converted_and_persisted() {
        let home = temp_home("durable-welcome-payload");
        let store = FileArkretCryptoStore::for_account(&home, "c1", "agent");
        let principal =
            DidCoreId::new("ak:did_core:web:agent.example".to_owned()).expect("principal");
        let device = DeviceId::new("ak:device:01904100-0000-7000-8000-00000000000f".to_owned())
            .expect("device");
        let key_package = store
            .ensure_mls_key_package(&test_account(principal.as_str()), device.as_str())
            .expect("KeyPackage");

        let owner = new_human_mls_identity(
            DidCoreId::new("ak:did_core:web:owner.example".to_owned()).expect("owner"),
            DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006".to_owned())
                .expect("owner device"),
        )
        .expect("owner identity");
        let realm_id =
            RealmId::new("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI".to_owned())
                .expect("realm");
        let mut group = owner
            .create_group(realm_id.as_str().as_bytes())
            .expect("group");
        let add = group.add_member(&key_package).expect("add member");
        let payload = test_welcome_payload(
            &key_package,
            &add.welcome,
            realm_id.as_str(),
            "ak:event:AbnHJt4q4qY18zqvLiy3Emmqy7weTAuApx42RmRgPr2h",
            None,
        );

        let value = serde_json::to_value(&payload).expect("serialize durable payload");
        assert!(value.get("mls_group_id").is_none());
        assert!(value.get("epoch").is_none());
        let decoded: MlsWelcomePayload = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded.mls_group_id(), add.welcome.group_id);
        assert_eq!(decoded.epoch(), add.welcome.epoch);
        for retired in [
            serde_json::to_value(&add.welcome).unwrap(),
            json!({"content": value.clone()}),
            json!({"payload": value.clone()}),
            json!({"mls_welcome": value.clone()}),
            json!({"mlsGroupId": decoded.mls_group_id(), "epoch": decoded.epoch(), "keyPackageRef": decoded.keypackage_ref}),
        ] {
            assert!(serde_json::from_value::<MlsWelcomePayload>(retired).is_err());
        }
        for retired_field in ["mls_group_id", "mlsGroupId", "epoch"] {
            let mut retired = value.clone();
            retired[retired_field] = serde_json::json!("copied-coordinate");
            assert!(serde_json::from_value::<MlsWelcomePayload>(retired).is_err());
        }
        assert!(
            store
                .load()
                .unwrap()
                .mls_welcome_consume_bindings
                .is_empty()
        );
        let recorded = persist_test_accepted_welcome(
            &store,
            &decoded,
            "ak:event:AfOnmtYgQpP17IGXP_64dE-weM-8C_AfXXXfYpJ3ubJG",
            fixture_leaf_bindings(
                &group,
                &[
                    MlsEndpointIdentity::human_device(
                        DidCoreId::new("ak:did_core:web:owner.example".to_owned()).unwrap(),
                        DeviceId::new("ak:device:01904100-0000-7000-8000-000000000006".to_owned())
                            .unwrap(),
                    ),
                    key_package.endpoint.clone(),
                ],
                "ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6",
            ),
        );
        let mut expected = add.welcome;
        expected.ratchet_tree = None;
        assert_eq!(recorded, expected);
        let state = store.load().expect("state");
        let mls_store = state.mls_store().expect("MLS store");
        let welcomes = mls_store.welcomes_for_device(&principal, &device);
        assert_eq!(welcomes.len(), 1);
        assert_eq!(welcomes[0], &recorded);

        let repair_realm_id = payload.claim_envelope.intended_realm_id.clone();
        let repair_actor = DidCoreId::new("ak:did_core:web:owner.example".to_owned()).unwrap();
        let repair_principal_server =
            DidCoreId::new("ak:did_core:web:principal-server.example".to_owned()).unwrap();
        let repair_scope = ScopeRef::Realm {
            realm_id: repair_realm_id.clone(),
        };
        let strand_event_id =
            EventId::new("ak:event:Adoyyx1AqvJH02hYxuUtpzuC-zpV8GxwFQ8XInZLbu3s".to_owned())
                .unwrap();
        // A Strand id is event-derived: it is retyped off its create Event, not
        // chosen by the payload.
        let strand_id = arkret::StrandId::from_event_id(&strand_event_id);
        let strand_payload = StrandCreatePayload {
            object: arkret::Strand::new(
                strand_id.clone(),
                repair_realm_id.clone(),
                "Direct",
                ActorId::account(AccountId::new(
                    repair_actor.clone(),
                    repair_principal_server.clone(),
                )),
            ),
        };
        let mut strand_event = arkret_wire::test_support::raw_event_at(
            "ak.strand.create",
            repair_scope.clone(),
            repair_actor.clone(),
            repair_principal_server.clone(),
            1,
            arkret::Hlc::new("01970e589d21-0004-a13f9c2e").unwrap(),
            serde_json::to_value(strand_payload).unwrap(),
            Utc::now(),
        )
        .unwrap();
        let welcome_event_id =
            EventId::new("ak:event:AfOnmtYgQpP17IGXP_64dE-weM-8C_AfXXXfYpJ3ubJG".to_owned())
                .unwrap();
        strand_event.event_id = strand_event_id;
        let mut welcome_event = arkret_wire::test_support::raw_event_at(
            "ak.mls.welcome",
            repair_scope,
            repair_actor,
            repair_principal_server,
            2,
            arkret::Hlc::new("01970e589d22-0000-a13f9c2e").unwrap(),
            serde_json::to_value(&payload).unwrap(),
            Utc::now(),
        )
        .unwrap();
        welcome_event.event_id = welcome_event_id.clone();
        let mut other_welcome = welcome_event.clone();
        other_welcome.event_id =
            EventId::new("ak:event:ARELvWOpF6BRrks3DlbQy-9XIE6aAQQumDQp7fA4ApeM").unwrap();
        assert_eq!(
            store
                .repair_pending_direct_conversation_bindings_from_accepted_events(&[
                    strand_event.clone(),
                    other_welcome,
                ])
                .unwrap(),
            0
        );

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
