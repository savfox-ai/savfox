//! File-based persistent session store.
//!
//! Stores session metadata in rollout files (UUID v7 .jsonl files) in `{savfox_home}/sessions/`.
//! Provides TTL-based in-memory caching and automatic pruning of stale entries.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use savfox_core::rollout::list::read_session_meta_line;
use savfox_protocol::SessionId;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OnceCell, RwLock};
use tracing::{info, warn};

// ─── Session Overrides ─────────────────────────────────────────────────────

/// Per-session override settings for model, thinking, verbosity, and reasoning.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOverrides {
    /// Model override (e.g. "gpt-4o", "claude-opus-4-20250514").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Thinking budget: "off", "low", "medium", "high".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// Verbosity level: "off", "on", "full".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbose: Option<String>,
    /// Reasoning mode: "off", "on", "stream".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

impl SessionOverrides {
    /// Returns `true` if all fields are `None`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.thinking.is_none()
            && self.verbose.is_none()
            && self.reasoning.is_none()
    }

    /// Merge non-`None` fields from `other` into `self`.
    pub fn merge(&mut self, other: &Self) {
        if other.model.is_some() {
            self.model.clone_from(&other.model);
        }
        if other.thinking.is_some() {
            self.thinking.clone_from(&other.thinking);
        }
        if other.verbose.is_some() {
            self.verbose.clone_from(&other.verbose);
        }
        if other.reasoning.is_some() {
            self.reasoning.clone_from(&other.reasoning);
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMessageOrigin {
    ArkretRemote,
    LocalOperator,
    Agent,
    Tool,
    #[default]
    System,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMessageVisibility {
    RemotePublic,
    #[default]
    LocalPrivate,
    PublishCandidate,
}

/// Provenance metadata for a user-originated message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageProvenance {
    /// Source channel in `platform:channel` format.
    pub channel: String,
    /// Platform-native sender ID.
    pub user_id: String,
    /// Display name captured from the inbound event.
    pub name: String,
    /// Capture timestamp (epoch milliseconds).
    pub timestamp: u64,
    #[serde(default)]
    pub origin: SessionMessageOrigin,
    #[serde(default)]
    pub visibility: SessionMessageVisibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_config_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strand_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_kind: Option<String>,
}

/// Sender information for a session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSender {
    /// Platform-native sender ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Display name of the sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

// ─── Session Entry ─────────────────────────────────────────────────────────

/// Persistent metadata for a single session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntry {
    /// Unique session identifier (UUID v7).
    pub session_id: String,
    /// Optional routing identifier used by channel/webhook session tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_id: Option<String>,
    /// Agent this session is bound to. Set explicitly for sessions created
    /// from the web UI; channel sessions encode the agent in `routing_id`
    /// instead (see `agent_from_routing_id`). A session's agent is fixed for
    /// its lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Last update timestamp (epoch milliseconds).
    pub updated_at: u64,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: u64,
    /// Optimistic-lock version counter. Incremented on each write so
    /// concurrent readers can detect stale data.
    #[serde(default)]
    pub version: u64,
    /// Stable lifecycle identity, replaced on reset and on delete/recreation.
    #[serde(default = "new_lifecycle_generation")]
    pub lifecycle_generation: String,
    /// Revision observed when reading storage; local touch() does not change it.
    #[serde(skip)]
    pub persisted_version: Option<u64>,

    // ── Delivery context ────────────────────────────────────────────
    /// Current channel (e.g. "discord:123456").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Last channel the session was active on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_channel: Option<String>,
    /// Recipient/target identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Last recipient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_to: Option<String>,
    /// Account ID (for multi-account setups).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Core Savfox thread UUID. This is separate from any remote platform
    /// thread/strand id and is the canonical rollout-resume pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_thread_id: Option<String>,
    /// Remote platform thread identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_thread_id: Option<String>,
    /// Remote Event id to which a public response is causally related.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_event_id: Option<String>,
    /// Legacy mixed-namespace thread field. New writes use `core_thread_id`
    /// and `remote_thread_id`; it remains readable for one-time migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Parent thread ID when this session is spawned from another thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    /// Reply target/message ID used for platform-specific reply threading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_target: Option<String>,
    /// Parent message ID (for threaded replies).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<String>,

    // ── Group session info ──────────────────────────────────────────
    /// Group identifier (for channel/group chats).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// Group subject/topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Group channel name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_channel: Option<String>,
    /// Workspace/space name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    /// Sender information (last message sender).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<SessionSender>,

    // ── Agent config overrides ──────────────────────────────────────
    /// Model to use for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// LLM provider override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Canonical cross-channel identity id, if linked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Label for UI display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Whether the user pinned this session in the UI.
    #[serde(default)]
    pub pinned: bool,
    /// Whether the user archived this session in the UI.
    #[serde(default)]
    pub archived: bool,
    /// Fingerprint of the effective execution security policy last used for
    /// this logical session. A changed fingerprint invalidates the in-memory
    /// core session so stale session-scoped grants cannot survive a policy
    /// change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_policy_fingerprint: Option<String>,
    /// Effective execution mode associated with
    /// `security_policy_fingerprint`, for audit and support diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<String>,
    /// Concrete sandbox backend (or fail-closed gate) used for the last turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_enforcement: Option<String>,

    // ── Token usage metrics ─────────────────────────────────────────
    /// Cumulative input tokens.
    #[serde(default)]
    pub input_tokens: u64,
    /// Cumulative output tokens.
    #[serde(default)]
    pub output_tokens: u64,
    /// Cumulative total tokens.
    #[serde(default)]
    pub total_tokens: u64,

    // ── Session lifecycle ───────────────────────────────────────────
    /// Number of context compactions performed.
    #[serde(default)]
    pub compaction_count: u32,
    /// Number of memory flush records generated by compaction.
    #[serde(default)]
    pub memory_flush_count: u32,
    /// Total bytes written to compaction memory flush records.
    #[serde(default)]
    pub memory_flush_bytes: u64,
    /// Estimated token savings accumulated from compaction flushes.
    #[serde(default)]
    pub memory_flush_tokens_saved: u64,
    /// Path to the JSONL transcript file (relative to sessions dir).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    /// Origin platform (e.g. "discord", "telegram", "api").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Chat type: "dm", "group", "channel".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_type: Option<String>,
    /// Group activation mode: "mention", "keyword", "always", "command", or "off".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_activation: Option<String>,

    // ── Session elevation ──────────────────────────────────────────
    /// Whether the session is in elevated mode (bypass tool approval).
    #[serde(default)]
    pub elevated: bool,
    /// Elevation expiry timestamp (epoch ms); auto-reverts after this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevated_until: Option<u64>,

    // ── Per-session overrides ───────────────────────────────────────
    /// Optional per-session overrides for model, thinking, verbosity, and reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<SessionOverrides>,
    /// Inbound message provenance records for source-channel filtering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<SessionMessageProvenance>,
}

impl SessionEntry {
    /// Create a new session entry with defaults.
    #[must_use]
    pub fn new() -> Self {
        let now_ms = crate::json_store::now_ms();
        Self {
            session_id: uuid::Uuid::now_v7().to_string(),
            routing_id: None,
            updated_at: now_ms,
            created_at: now_ms,
            ..Default::default()
        }
    }

    /// Touch the `updated_at` timestamp and bump the version counter.
    pub fn touch(&mut self) {
        self.updated_at = crate::json_store::now_ms();
        self.version += 1;
    }

    /// One-time compatibility migration for metadata written before core and
    /// remote thread namespaces were separated.
    pub fn migrate_legacy_thread_namespaces(&mut self) {
        let Some(legacy) = self.thread_id.clone() else {
            return;
        };
        let is_core_uuid = uuid::Uuid::parse_str(&legacy)
            .ok()
            .is_some_and(|value| value.get_version_num() == 7);
        if is_core_uuid && self.core_thread_id.is_none() {
            self.core_thread_id = Some(legacy);
        } else if !is_core_uuid && self.remote_thread_id.is_none() {
            self.remote_thread_id = Some(legacy);
        }
        self.thread_id = None;
    }

    /// Add token usage.
    pub fn add_tokens(&mut self, input: u64, output: u64) {
        self.input_tokens += input;
        self.output_tokens += output;
        self.total_tokens += input + output;
        self.touch();
    }

    /// Patch the session overrides by merging `incoming` into the existing
    /// overrides (or replacing them if none existed).
    pub fn patch_overrides(&mut self, incoming: SessionOverrides) {
        match self.overrides.as_mut() {
            Some(existing) => existing.merge(&incoming),
            None => self.overrides = Some(incoming),
        }
        self.touch();
    }

    pub fn push_provenance(&mut self, record: SessionMessageProvenance) {
        const MAX_PROVENANCE_ENTRIES: usize = 1024;
        self.provenance.push(record);
        if self.provenance.len() > MAX_PROVENANCE_ENTRIES {
            let overflow = self.provenance.len() - MAX_PROVENANCE_ENTRIES;
            self.provenance.drain(0..overflow);
        }
    }

    /// Check if this entry is stale (older than the given duration).
    #[must_use]
    pub fn is_stale(&self, max_age: Duration) -> bool {
        let now = crate::json_store::now_ms();
        let age_ms = now.saturating_sub(self.updated_at);
        age_ms > max_age.as_millis() as u64
    }
}

impl Default for SessionEntry {
    fn default() -> Self {
        Self {
            session_id: uuid::Uuid::now_v7().to_string(),
            routing_id: None,
            agent_id: None,
            updated_at: 0,
            created_at: 0,
            version: 0,
            lifecycle_generation: new_lifecycle_generation(),
            persisted_version: None,
            channel: None,
            last_channel: None,
            to: None,
            last_to: None,
            account_id: None,
            core_thread_id: None,
            remote_thread_id: None,
            reply_to_event_id: None,
            thread_id: None,
            parent_thread_id: None,
            reply_target: None,
            parent_message_id: None,
            group_id: None,
            subject: None,
            group_channel: None,
            space: None,
            sender: None,
            model: None,
            provider: None,
            identity: None,
            label: None,
            pinned: false,
            archived: false,
            security_policy_fingerprint: None,
            execution_mode: None,
            sandbox_enforcement: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            compaction_count: 0,
            memory_flush_count: 0,
            memory_flush_bytes: 0,
            memory_flush_tokens_saved: 0,
            session_file: None,
            from: None,
            chat_type: None,
            group_activation: None,
            elevated: false,
            elevated_until: None,
            overrides: None,
            provenance: Vec::new(),
        }
    }
}

// ─── Session Routing Resolution ────────────────────────────────────────────

/// DM scoping mode: controls how direct-message sessions are routed.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DmScope {
    /// Single session per agent (default).
    #[default]
    Main,
    /// Separate session per peer.
    PerPeer,
    /// Separate session per channel + peer.
    PerChannelPeer,
    /// Separate session per account + channel + peer.
    PerAccountChannelPeer,
}

impl DmScope {
    #[must_use]
    pub fn from_str(value: Option<&str>) -> Self {
        match value.unwrap_or("main").trim().to_ascii_lowercase().as_str() {
            "main" => Self::Main,
            "per_peer" => Self::PerPeer,
            "per_channel_peer" => Self::PerChannelPeer,
            "per_account_channel_peer" => Self::PerAccountChannelPeer,
            _ => Self::Main,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::PerPeer => "per_peer",
            Self::PerChannelPeer => "per_channel_peer",
            Self::PerAccountChannelPeer => "per_account_channel_peer",
        }
    }
}

/// Build a deterministic routing id from message context components.
#[must_use]
pub fn build_routing_id(
    agent_id: &str,
    channel: Option<&str>,
    group_id: Option<&str>,
    thread_id: Option<&str>,
    peer_id: Option<&str>,
    account_id: Option<&str>,
    dm_scope: DmScope,
) -> String {
    let mut key = format!("agent:{agent_id}");

    if let Some(gid) = group_id {
        // Group session: agent:{id}:{channel}:group:{gid}
        if let Some(ch) = channel {
            key.push_str(&format!(":{ch}"));
        }
        key.push_str(&format!(":group:{gid}"));
    } else {
        // DM session, scoped by DmScope
        match dm_scope {
            DmScope::Main => key.push_str(":main"),
            DmScope::PerPeer => {
                if let Some(pid) = peer_id {
                    key.push_str(&format!(":peer:{pid}"));
                } else {
                    key.push_str(":main");
                }
            }
            DmScope::PerChannelPeer => {
                if let Some(ch) = channel {
                    key.push_str(&format!(":{ch}"));
                }
                if let Some(pid) = peer_id {
                    key.push_str(&format!(":peer:{pid}"));
                } else {
                    key.push_str(":main");
                }
            }
            DmScope::PerAccountChannelPeer => {
                if let Some(account) = account_id {
                    key.push_str(&format!(":acct:{account}"));
                }
                if let Some(ch) = channel {
                    key.push_str(&format!(":{ch}"));
                }
                if let Some(pid) = peer_id {
                    key.push_str(&format!(":peer:{pid}"));
                } else {
                    key.push_str(":main");
                }
            }
        }
    }

    // Append thread/topic suffix if present.
    if let Some(tid) = thread_id {
        key.push_str(&format!(":topic:{tid}"));
    }

    key
}

// ─── Session Store ─────────────────────────────────────────────────────────

/// Configuration for the session store.
#[derive(Debug, Clone)]
pub struct SessionStoreConfig {
    /// Base directory for session files.
    pub sessions_dir: PathBuf,
    /// Sidecar path for persisted gateway session metadata.
    pub metadata_path: PathBuf,
    /// Maximum age for entries before pruning (default: 30 days).
    pub max_age: Duration,
    /// Maximum number of entries to keep (default: 500).
    pub max_entries: usize,
    /// Cache TTL (default: 45 seconds).
    pub cache_ttl: Duration,
}

impl Default for SessionStoreConfig {
    fn default() -> Self {
        Self {
            sessions_dir: PathBuf::from("."),
            metadata_path: PathBuf::from(".gateway-session-store.json"),
            max_age: Duration::from_secs(30 * 24 * 3600), // 30 days
            max_entries: 500,
            cache_ttl: Duration::from_secs(45),
        }
    }
}

/// In-memory cache layer.
#[derive(Debug)]
struct StoreCache {
    entries: HashMap<String, SessionEntry>,
    loaded_at: Instant,
}

/// File-based persistent session store with in-memory caching.
#[derive(Debug)]
pub struct SessionStore {
    config: SessionStoreConfig,
    cache: RwLock<Option<StoreCache>>,
    /// Serializes refreshes, mutations and their durable writes. Readers can
    /// still access the cache while a mutation is being persisted.
    mutation: Mutex<()>,
    database: OnceCell<sqlx::SqlitePool>,
}

impl SessionStore {
    /// Create a new session store.
    #[must_use]
    pub fn new(config: SessionStoreConfig) -> Self {
        Self {
            config,
            cache: RwLock::new(None),
            mutation: Mutex::new(()),
            database: OnceCell::new(),
        }
    }

    /// Create a session store rooted at the given home directory.
    #[must_use]
    pub fn from_home(home: &Path) -> Self {
        let sessions_dir = home.join("sessions");
        Self::new(SessionStoreConfig {
            sessions_dir,
            metadata_path: home.join("gateway-session-store.json"),
            ..Default::default()
        })
    }

    /// Ensure the sessions directory exists.
    async fn ensure_dir(&self) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.config.sessions_dir).await
    }

    // Keep the legacy JSON snapshot as a migration artifact. SQLite is
    // authoritative after the first successful transactional import.
    async fn database(&self) -> std::io::Result<&sqlx::SqlitePool> {
        self.database
            .get_or_try_init(|| async {
                let path = self.config.metadata_path.with_extension("sqlite");
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let mut file = tokio::fs::OpenOptions::new();
                file.create(true).append(true);
                #[cfg(unix)]
                file.mode(0o600);
                file.open(&path).await?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .await?;
                }
                let options = sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                    .synchronous(sqlx::sqlite::SqliteSynchronous::Full);
                let pool = sqlx::sqlite::SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(options)
                    .await
                    .map_err(store_io_error)?;
                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS session_metadata (id TEXT PRIMARY KEY, data TEXT)",
                )
                .execute(&pool)
                .await
                .map_err(store_io_error)?;
                sqlx::query("CREATE TABLE IF NOT EXISTS session_store_meta (key TEXT PRIMARY KEY)")
                    .execute(&pool)
                    .await
                    .map_err(store_io_error)?;
                let mut transaction = pool.begin().await.map_err(store_io_error)?;
                let imported: Option<(String,)> = sqlx::query_as(
                    "SELECT key FROM session_store_meta WHERE key = 'legacy_imported'",
                )
                .fetch_optional(&mut *transaction)
                .await
                .map_err(store_io_error)?;
                if imported.is_none() {
                    let entries: HashMap<String, SessionEntry> =
                        match tokio::fs::read_to_string(&self.config.metadata_path).await {
                            Ok(content) => {
                                serde_json::from_str(&content).map_err(store_io_error)?
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                HashMap::new()
                            }
                            Err(error) => return Err(error),
                        };
                    for (id, entry) in entries {
                        if id != entry.session_id || !is_uuid_v7(&id) {
                            continue;
                        }
                        let data = serde_json::to_string(&entry).map_err(store_io_error)?;
                        sqlx::query(
                            "INSERT OR IGNORE INTO session_metadata (id, data) VALUES (?1, ?2)",
                        )
                        .bind(id)
                        .bind(data)
                        .execute(&mut *transaction)
                        .await
                        .map_err(store_io_error)?;
                    }
                    sqlx::query("INSERT INTO session_store_meta (key) VALUES ('legacy_imported')")
                        .execute(&mut *transaction)
                        .await
                        .map_err(store_io_error)?;
                }
                transaction.commit().await.map_err(store_io_error)?;
                Ok(pool)
            })
            .await
    }

    async fn overlay_persisted_entries(
        &self,
        entries: &mut HashMap<String, SessionEntry>,
    ) -> std::io::Result<()> {
        let pool = self.database().await?;
        let records =
            sqlx::query_as::<_, (String, Option<String>)>("SELECT id, data FROM session_metadata")
                .fetch_all(pool)
                .await
                .map_err(store_io_error)?;
        for (id, data) in records {
            match data {
                Some(data) => {
                    let mut entry: SessionEntry =
                        serde_json::from_str(&data).map_err(store_io_error)?;
                    if entry.session_id != id {
                        return Err(store_io_error("session ID mismatch"));
                    }
                    entry.migrate_legacy_thread_namespaces();
                    entry.persisted_version = Some(entry.version);
                    if let Some(rollout) = entries.remove(&id) {
                        entry.routing_id = entry.routing_id.or(rollout.routing_id);
                        entry.session_file = entry.session_file.or(rollout.session_file);
                        entry.model = entry.model.or(rollout.model);
                        entry.provider = entry.provider.or(rollout.provider);
                        entry.from = entry.from.or(rollout.from);
                        if entry.created_at == 0 {
                            entry.created_at = rollout.created_at;
                        }
                        if entry.updated_at == 0 {
                            entry.updated_at = rollout.updated_at;
                        }
                    }
                    entries.insert(id, entry);
                }
                None => {
                    entries.remove(&id);
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn is_tombstoned(&self, id: &str) -> std::io::Result<bool> {
        let record: Option<(Option<String>,)> =
            sqlx::query_as("SELECT data FROM session_metadata WHERE id = ?1")
                .bind(id)
                .fetch_optional(self.database().await?)
                .await
                .map_err(store_io_error)?;
        Ok(matches!(record, Some((None,))))
    }

    async fn persist_entry(&self, id: &str, entry: Option<&SessionEntry>) -> std::io::Result<()> {
        let pool = self.database().await?;
        let data = entry
            .map(serde_json::to_string)
            .transpose()
            .map_err(store_io_error)?;
        if let Some(entry) = entry {
            let expected = entry
                .persisted_version
                .map(i64::try_from)
                .transpose()
                .map_err(store_io_error)?;
            let result = sqlx::query("INSERT INTO session_metadata (id, data) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET data = excluded.data WHERE (?3 IS NULL AND session_metadata.data IS NULL) OR (session_metadata.data IS NOT NULL AND json_extract(session_metadata.data, '$.version') = ?3 AND json_extract(session_metadata.data, '$.lifecycleGeneration') = ?4)")
                .bind(id).bind(data).bind(expected).bind(&entry.lifecycle_generation).execute(pool).await.map_err(store_io_error)?;
            if result.rows_affected() == 0 {
                return Err(store_io_error("stale session replacement"));
            }
        } else {
            sqlx::query("INSERT INTO session_metadata (id, data) VALUES (?1, NULL) ON CONFLICT(id) DO UPDATE SET data = NULL")
                .bind(id).execute(pool).await.map_err(store_io_error)?;
        }
        Ok(())
    }

    async fn persist_entries(
        &self,
        entries: &HashMap<String, SessionEntry>,
        remove_missing: bool,
    ) -> std::io::Result<()> {
        let previous = self
            .cache
            .read()
            .await
            .as_ref()
            .map(|cache| cache.entries.clone())
            .unwrap_or_default();
        let pool = self.database().await?;
        let mut transaction = pool.begin().await.map_err(store_io_error)?;
        for (id, old) in &previous {
            if remove_missing && !entries.contains_key(id) {
                let result = sqlx::query("INSERT INTO session_metadata (id, data) VALUES (?1, NULL) ON CONFLICT(id) DO UPDATE SET data = NULL WHERE session_metadata.data IS NOT NULL AND json_extract(session_metadata.data, '$.version') = ?2 AND json_extract(session_metadata.data, '$.lifecycleGeneration') = ?3")
                    .bind(id).bind(old.version as i64).bind(&old.lifecycle_generation).execute(&mut *transaction).await.map_err(store_io_error)?;
                if result.rows_affected() == 0 {
                    return Err(store_io_error(
                        "session deletion changed concurrently; retry bulk operation",
                    ));
                }
            }
        }
        for (id, entry) in entries {
            if let Some(current) = previous.get(id)
                && (entry.persisted_version != current.persisted_version
                    || entry.lifecycle_generation != current.lifecycle_generation)
            {
                return Err(store_io_error("stale bulk session snapshot"));
            }
            let expected = entry.persisted_version.unwrap_or(0);
            let mut updated = entry.clone();
            updated.version = expected
                .checked_add(1)
                .ok_or_else(|| store_io_error("session version overflow"))?;
            let data = serde_json::to_string(&updated).map_err(store_io_error)?;
            let expected = i64::try_from(expected).map_err(store_io_error)?;
            let result = sqlx::query("INSERT INTO session_metadata (id, data) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET data = excluded.data WHERE session_metadata.data IS NOT NULL AND json_extract(session_metadata.data, '$.version') = ?3 AND json_extract(session_metadata.data, '$.lifecycleGeneration') = ?4")
                .bind(id).bind(data).bind(expected).bind(&entry.lifecycle_generation).execute(&mut *transaction).await.map_err(store_io_error)?;
            if result.rows_affected() == 0 {
                return Err(store_io_error(
                    "session metadata changed concurrently; retry bulk operation",
                ));
            }
        }
        transaction.commit().await.map_err(store_io_error)
    }

    /// Check if the cache is still valid.
    async fn is_cache_valid(&self) -> bool {
        let cache = self.cache.read().await;
        let Some(cache) = cache.as_ref() else {
            return false;
        };
        // Check TTL.
        if cache.loaded_at.elapsed() > self.config.cache_ttl {
            return false;
        }
        // Check if we need to refresh (simplified - always refresh on first access after TTL)
        true
    }

    /// Load entries from rollout files (bypassing cache).
    async fn load_from_disk(&self) -> std::io::Result<HashMap<String, SessionEntry>> {
        let mut entries: HashMap<String, SessionEntry> = HashMap::new();
        let mut dir = if let Ok(dir) = tokio::fs::read_dir(&self.config.sessions_dir).await {
            dir
        } else {
            self.overlay_persisted_entries(&mut entries).await?;
            return Ok(entries);
        };

        while let Some(entry) = dir.next_entry().await.ok().flatten() {
            let file_type = match entry.file_type().await.ok() {
                Some(ft) => ft,
                None => continue,
            };

            if !file_type.is_file() {
                continue;
            }

            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !name_str.ends_with(".jsonl") {
                continue;
            }

            // Extract UUID from filename
            let uuid_str = name_str.strip_suffix(".jsonl").unwrap_or(&name_str);
            if !is_uuid_v7(uuid_str) {
                continue;
            }

            let path = entry.path();
            match read_session_meta_line(&path).await {
                Ok(meta_line) => {
                    let session_id = uuid_str.to_owned();
                    let meta_id = meta_line.meta.id.to_string();
                    let created_at = meta_line
                        .meta
                        .timestamp
                        .parse::<u64>()
                        .ok()
                        .unwrap_or_else(crate::json_store::now_ms);

                    let rollout_entry = SessionEntry {
                        session_id: session_id.clone(),
                        routing_id: if meta_id != session_id {
                            Some(meta_id)
                        } else {
                            None
                        },
                        updated_at: created_at,
                        created_at,
                        session_file: Some(uuid_str.to_owned()),
                        model: meta_line.meta.model_slug().map(str::to_owned),
                        provider: meta_line.meta.model_provider_id().map(str::to_owned),
                        from: Some(format!("{:?}", meta_line.meta.source)),
                        // Add other fields as needed from SessionMeta
                        ..Default::default()
                    };
                    match entries.get_mut(&session_id) {
                        Some(existing) => {
                            if existing.routing_id.is_none() {
                                existing.routing_id = rollout_entry.routing_id;
                            }
                            if existing.session_file.is_none() {
                                existing.session_file = rollout_entry.session_file;
                            }
                            if existing.model.is_none() {
                                existing.model = rollout_entry.model;
                            }
                            if existing.provider.is_none() {
                                existing.provider = rollout_entry.provider;
                            }
                            if existing.from.is_none() {
                                existing.from = rollout_entry.from;
                            }
                            if existing.created_at == 0 {
                                existing.created_at = rollout_entry.created_at;
                            }
                            if existing.updated_at == 0 {
                                existing.updated_at = rollout_entry.updated_at;
                            }
                        }
                        None => {
                            entries.insert(session_id, rollout_entry);
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        path = %path.display(),
                        "failed to read session metadata: {e}"
                    );
                }
            }
        }

        self.overlay_persisted_entries(&mut entries).await?;
        Ok(entries)
    }

    /// Refresh the cache from disk if stale.
    async fn refresh_cache(&self) {
        if self.is_cache_valid().await {
            return;
        }

        let _mutation = self.mutation.lock().await;
        if self.is_cache_valid().await {
            return;
        }

        let entries = match self.load_from_disk().await {
            Ok(entries) => entries,
            Err(error) => {
                warn!(%error, "session refresh failed; retaining last valid cache");
                return;
            }
        };

        let mut cache = self.cache.write().await;
        *cache = Some(StoreCache {
            entries,
            loaded_at: Instant::now(),
        });
    }

    /// Get all sessions.
    pub async fn list(&self) -> Vec<SessionEntry> {
        self.refresh_cache().await;
        let cache = self.cache.read().await;
        cache
            .as_ref()
            .map(|c| c.entries.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Get a session by UUID v7 id.
    pub async fn get(&self, session_id: &str) -> Option<SessionEntry> {
        if !is_uuid_v7(session_id) {
            return None;
        }
        self.refresh_cache().await;
        let cache = self.cache.read().await;
        cache
            .as_ref()
            .and_then(|c| c.entries.get(session_id).cloned())
    }

    /// Get a session by its UUID session_id field.
    pub async fn get_by_session_id(&self, session_id: &str) -> Option<SessionEntry> {
        self.get(session_id).await
    }

    /// Find a session by routing id (non-UUID channel/webhook routing key).
    pub async fn get_by_routing_id(&self, routing_id: &str) -> Option<SessionEntry> {
        let routing_id = routing_id.trim();
        if routing_id.is_empty() {
            return None;
        }
        self.refresh_cache().await;
        let cache = self.cache.read().await;
        cache.as_ref().and_then(|c| {
            c.entries
                .values()
                .find(|entry| entry.routing_id.as_deref() == Some(routing_id))
                .cloned()
        })
    }

    /// Get or create a session entry for a UUID v7 session id.
    pub async fn get_or_create(&self, session_id: &str) -> Option<SessionEntry> {
        if !is_uuid_v7(session_id) {
            return None;
        }
        if let Some(entry) = self.get(session_id).await {
            return Some(entry);
        }
        let mut entry = SessionEntry::new();
        entry.session_id = session_id.to_owned();
        self.upsert(entry.clone()).await;
        self.get(session_id).await
    }

    /// Get or create a session entry for a routing id, generating a UUID v7 session_id.
    pub async fn get_or_create_for_routing_id(&self, routing_id: &str) -> SessionEntry {
        if let Some(entry) = self.get_by_routing_id(routing_id).await {
            return entry;
        }
        let mut entry = SessionEntry::new();
        entry.routing_id = Some(routing_id.to_owned());
        self.upsert(entry.clone()).await;
        self.get(&entry.session_id).await.unwrap_or(entry)
    }

    /// Insert or update a session entry.
    ///
    /// Rejects stale replacements, including equal-version competing writes.
    /// Use [`Self::update`] for atomic field changes and additive counters;
    /// replacing a previously read entry cannot merge concurrent changes.
    pub async fn upsert(&self, mut entry: SessionEntry) {
        self.refresh_cache().await;
        let _mutation = self.mutation.lock().await;
        entry.migrate_legacy_thread_namespaces();
        let Some(version) = entry.persisted_version.unwrap_or(0).checked_add(1) else {
            warn!("session version overflow");
            return;
        };
        entry.version = version;
        let key = entry.session_id.clone();
        if let Err(error) = self.persist_entry(&key, Some(&entry)).await {
            warn!(%error, "failed to persist session upsert");
            return;
        }
        entry.persisted_version = Some(entry.version);
        let mut cache = self.cache.write().await;
        if let Some(cache) = cache.as_mut() {
            cache.entries.insert(key, entry);
        }
    }

    /// Explicitly replace the current cached collection atomically. Missing IDs
    /// in the cache at invocation time are deletion candidates, even if they were
    /// created after a caller's earlier list(). Use apply_bulk_updates for migration.
    pub async fn replace_all(&self, entries: HashMap<String, SessionEntry>) -> std::io::Result<()> {
        let _mutation = self.mutation.lock().await;
        self.persist_entries(&entries, true).await?;
        let entries = self.load_from_disk().await?;
        *self.cache.write().await = Some(StoreCache {
            entries,
            loaded_at: Instant::now(),
        });
        Ok(())
    }

    /// Apply a snapshot-based migration without deleting sessions added while
    /// the caller computed it. Any changed supplied row aborts the transaction.
    pub async fn apply_bulk_updates(
        &self,
        entries: HashMap<String, SessionEntry>,
    ) -> std::io::Result<()> {
        let _mutation = self.mutation.lock().await;
        self.persist_entries(&entries, false).await?;
        let entries = self.load_from_disk().await?;
        *self.cache.write().await = Some(StoreCache {
            entries,
            loaded_at: Instant::now(),
        });
        Ok(())
    }

    /// Update a session entry by session_id using a closure.
    pub async fn update<F>(&self, session_id: &str, f: F) -> Option<SessionEntry>
    where
        F: FnOnce(&mut SessionEntry),
    {
        self.try_update(session_id, f).await.ok().flatten()
    }

    /// Persist one entry before publishing it in memory. A failed write leaves
    /// the cached entry unchanged and propagates to interactive callers.
    pub async fn try_update<F>(
        &self,
        session_id: &str,
        f: F,
    ) -> std::io::Result<Option<SessionEntry>>
    where
        F: FnOnce(&mut SessionEntry),
    {
        self.refresh_cache().await;
        let _mutation = self.mutation.lock().await;
        let pool = self.database().await?;
        let mut transaction = pool.begin().await.map_err(store_io_error)?;
        // Reserve the writer before reading; independent stores cannot race a
        // read/modify/write and lose additive counters.
        sqlx::query("UPDATE session_store_meta SET key = key WHERE key = 'legacy_imported'")
            .execute(&mut *transaction)
            .await
            .map_err(store_io_error)?;
        let persisted: Option<(Option<String>,)> =
            sqlx::query_as("SELECT data FROM session_metadata WHERE id = ?1")
                .bind(session_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(store_io_error)?;
        let mut entry = match persisted {
            Some((Some(data),)) => {
                serde_json::from_str::<SessionEntry>(&data).map_err(store_io_error)?
            }
            Some((None,)) => return Ok(None),
            None => {
                let cache = self.cache.read().await;
                let Some(entry) = cache
                    .as_ref()
                    .and_then(|cache| cache.entries.get(session_id))
                else {
                    return Ok(None);
                };
                entry.clone()
            }
        };
        f(&mut entry);
        entry.touch();
        let data = serde_json::to_string(&entry).map_err(store_io_error)?;
        sqlx::query("INSERT INTO session_metadata (id, data) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET data = excluded.data")
            .bind(session_id).bind(data).execute(&mut *transaction).await.map_err(store_io_error)?;
        transaction.commit().await.map_err(store_io_error)?;
        entry.persisted_version = Some(entry.version);
        if let Some(cache) = self.cache.write().await.as_mut() {
            cache.entries.insert(session_id.to_owned(), entry.clone());
        }
        Ok(Some(entry))
    }

    /// Remove a session by session_id.
    pub async fn remove(&self, session_id: &str) -> bool {
        self.try_remove(session_id).await.unwrap_or(false)
    }

    pub async fn try_remove(&self, session_id: &str) -> std::io::Result<bool> {
        if !is_uuid_v7(session_id) {
            return Ok(false);
        }
        self.refresh_cache().await;
        let _mutation = self.mutation.lock().await;
        self.persist_entry(session_id, None).await?;
        let (removed, session_file) = {
            let mut cache = self.cache.write().await;
            if let Some(cache) = cache.as_mut() {
                let entry = cache.entries.remove(session_id);
                let session_file = entry.as_ref().and_then(|e| e.session_file.clone());
                (entry.is_some(), session_file)
            } else {
                (false, None)
            }
        };

        if removed {
            // Delete the rollout file if it exists
            if let Some(session_file) = session_file {
                let path = self
                    .config
                    .sessions_dir
                    .join(format!("{session_file}.jsonl"));
                if let Err(err) = tokio::fs::remove_file(&path).await {
                    warn!(
                        path = %path.display(),
                        "failed to remove rollout file: {err}"
                    );
                }
            }
        }

        Ok(removed)
    }

    /// Delete a session by session_id.
    pub async fn delete(&self, session_id: &SessionId) -> bool {
        self.remove(&session_id.to_string()).await
    }

    /// Set a session's label.
    pub async fn set_label(&self, session_id: &str, label: String) -> bool {
        self.update(session_id, |e| e.label = Some(label.clone()))
            .await
            .is_some()
    }

    /// Set a session's pinned status.
    pub async fn set_pinned(&self, session_id: &str, pinned: bool) -> bool {
        self.try_update(session_id, |entry| entry.pinned = pinned)
            .await
            .is_ok_and(|entry| entry.is_some())
    }

    /// Set a session's archived status.
    pub async fn set_archived(&self, session_id: &str, archived: bool) -> bool {
        self.try_update(session_id, |entry| entry.archived = archived)
            .await
            .is_ok_and(|entry| entry.is_some())
    }

    /// List recent sessions.
    pub async fn list_recent(&self, limit: usize) -> Vec<SessionEntry> {
        let mut sessions = self.list().await;
        sessions.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));
        sessions.truncate(limit);
        sessions
    }

    /// Prune stale entries and cap total count.
    pub async fn prune_report(&self) -> SessionPruneResult {
        self.refresh_cache().await;
        let _mutation = self.mutation.lock().await;
        let entries = match self.load_from_disk().await {
            Ok(entries) => entries,
            Err(error) => {
                warn!(%error, "cannot prune unreadable session store");
                return SessionPruneResult::default();
            }
        };
        let mut retained: Vec<_> = entries
            .values()
            .filter(|entry| !entry.is_stale(self.config.max_age))
            .cloned()
            .collect();
        retained.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));
        retained.truncate(self.config.max_entries);
        let retained: HashMap<_, _> = retained
            .into_iter()
            .map(|entry| (entry.session_id.clone(), entry))
            .collect();
        let removed: Vec<_> = entries
            .values()
            .filter(|entry| !retained.contains_key(&entry.session_id))
            .collect();
        if removed.is_empty() {
            return SessionPruneResult::default();
        }
        let durable = async {
            let pool = self.database().await?;
            let mut transaction = pool.begin().await.map_err(store_io_error)?;
            let mut deleted = Vec::new();
            for entry in &removed {
                let result = sqlx::query("INSERT INTO session_metadata (id, data) VALUES (?1, NULL) ON CONFLICT(id) DO UPDATE SET data = NULL WHERE session_metadata.data IS NOT NULL AND json_extract(session_metadata.data, '$.version') = ?2 AND json_extract(session_metadata.data, '$.lifecycleGeneration') = ?3")
                    .bind(&entry.session_id).bind(entry.version as i64).bind(&entry.lifecycle_generation).execute(&mut *transaction).await.map_err(store_io_error)?;
                if result.rows_affected() > 0 { deleted.push(entry.session_id.clone()); }
            }
            transaction.commit().await.map_err(store_io_error)?;
            Ok::<_, std::io::Error>(deleted)
        }.await;
        let deleted = match durable {
            Ok(deleted) => deleted,
            Err(error) => {
                warn!(%error, "failed to persist pruning");
                return SessionPruneResult::default();
            }
        };
        let removed: Vec<_> = removed
            .into_iter()
            .filter(|entry| deleted.contains(&entry.session_id))
            .collect();
        if let Ok(entries) = self.load_from_disk().await
            && let Some(cache) = self.cache.write().await.as_mut()
        {
            cache.entries = entries;
        }
        // Persist tombstones before deleting files so a failed deletion cannot
        // resurrect metadata on the next cache refresh.
        let mut session_ids = Vec::new();
        let mut core_sessions = Vec::new();
        for entry in removed {
            session_ids.push(entry.session_id.clone());
            core_sessions.push((
                entry.session_id.clone(),
                entry.core_thread_id.clone().or(entry.thread_id.clone()),
            ));
            if let Some(session_file) = &entry.session_file {
                let path = self
                    .config
                    .sessions_dir
                    .join(format!("{session_file}.jsonl"));
                if let Err(error) = tokio::fs::remove_file(&path).await {
                    warn!(path = %path.display(), %error, "failed to remove stale rollout");
                }
            }
        }
        session_ids.sort();
        info!(
            pruned = session_ids.len(),
            "session store pruned stale entries"
        );
        SessionPruneResult {
            pruned: session_ids.len(),
            session_ids,
            core_sessions,
        }
    }

    /// Prune stale entries and cap total count.
    /// Returns the number of entries pruned.
    pub async fn prune(&self) -> usize {
        self.prune_report().await.pruned
    }

    /// Get summary statistics.
    pub async fn stats(&self) -> SessionStoreStats {
        self.refresh_cache().await;
        let cache = self.cache.read().await;
        let entries = cache.as_ref().map(|c| &c.entries);
        let Some(entries) = entries else {
            return SessionStoreStats {
                total_sessions: 0,
                total_tokens: 0,
                oldest_created_at: 0,
                newest_updated_at: 0,
            };
        };

        let total_tokens: u64 = entries.values().map(|e| e.total_tokens).sum();
        let oldest = entries.values().map(|e| e.created_at).min().unwrap_or(0);
        let newest = entries.values().map(|e| e.updated_at).max().unwrap_or(0);

        SessionStoreStats {
            total_sessions: entries.len(),
            total_tokens,
            oldest_created_at: oldest,
            newest_updated_at: newest,
        }
    }
}

fn new_lifecycle_generation() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn store_io_error(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

/// Summary statistics for the session store.
#[derive(Debug, Clone, Serialize)]
pub struct SessionStoreStats {
    pub total_sessions: usize,
    pub total_tokens: u64,
    pub oldest_created_at: u64,
    pub newest_updated_at: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SessionPruneResult {
    pub pruned: usize,
    pub session_ids: Vec<String>,
    /// Logical IDs and the core generation captured by the successful deletion.
    pub core_sessions: Vec<(String, Option<String>)>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tempfile::tempdir;

    use super::{SessionEntry, SessionStore};

    #[tokio::test]
    async fn independent_stores_preserve_new_rows_counters_and_updates_during_pruning() {
        let home = tempdir().unwrap();
        let a = SessionStore::from_home(home.path());
        let b = SessionStore::from_home(home.path());
        let shared = a.get_or_create_for_routing_id("shared").await;
        let mut stale = SessionEntry::new();
        stale.updated_at = 1;
        a.upsert(stale.clone()).await;
        b.list().await;
        a.update(&shared.session_id, |entry| entry.add_tokens(2, 3))
            .await
            .unwrap();
        b.update(&shared.session_id, |entry| entry.add_tokens(5, 7))
            .await
            .unwrap();
        let new = b.get_or_create_for_routing_id("new-after-a-cached").await;
        let report = a.prune_report().await;
        assert_eq!(report.session_ids, vec![stale.session_id]);
        let reopened = SessionStore::from_home(home.path());
        assert!(reopened.get(&new.session_id).await.is_some());
        let durable = reopened.get(&shared.session_id).await.unwrap();
        assert_eq!(durable.total_tokens, 17);
        assert_eq!(durable.input_tokens, 7);
        // A stale whole-entry replacement cannot overwrite B's newest fields.
        let mut obsolete = shared.clone();
        obsolete.label = Some("obsolete".into());
        a.upsert(obsolete).await;
        assert_eq!(
            SessionStore::from_home(home.path())
                .get(&shared.session_id)
                .await
                .unwrap()
                .total_tokens,
            17
        );
    }

    #[tokio::test]
    async fn bulk_write_advances_versions_and_rejects_stale_replacements_atomically() {
        let home = tempdir().unwrap();
        let a = SessionStore::from_home(home.path());
        let first = a.get_or_create_for_routing_id("before-bulk").await;
        let second = a.get_or_create_for_routing_id("unrelated").await;
        let b = SessionStore::from_home(home.path());
        let stale: std::collections::HashMap<_, _> = b
            .list()
            .await
            .into_iter()
            .map(|entry| (entry.session_id.clone(), entry))
            .collect();
        let mut changed = stale.clone();
        changed.get_mut(&first.session_id).unwrap().routing_id = Some("after-bulk".into());
        a.replace_all(changed).await.unwrap();
        let durable = SessionStore::from_home(home.path())
            .get(&first.session_id)
            .await
            .unwrap();
        assert_eq!(durable.version, first.version + 1);
        assert_eq!(durable.routing_id.as_deref(), Some("after-bulk"));
        let mut obsolete = stale[&first.session_id].clone();
        obsolete.label = Some("stale replacement".into());
        b.upsert(obsolete).await;
        assert!(b.replace_all(stale.clone()).await.is_err());
        // A stale deletion must also abort the whole bulk transaction, including
        // an attempted edit of an unrelated row.
        let mut only_other = stale[&second.session_id].clone();
        only_other.label = Some("must roll back".into());
        assert!(
            b.replace_all(std::collections::HashMap::from([(
                second.session_id.clone(),
                only_other
            )]))
            .await
            .is_err()
        );
        let reopened = SessionStore::from_home(home.path());
        let current = reopened.get(&first.session_id).await.unwrap();
        assert_eq!(current.routing_id.as_deref(), Some("after-bulk"));
        assert!(current.label.is_none());
        assert!(
            reopened
                .get(&second.session_id)
                .await
                .unwrap()
                .label
                .is_none()
        );
    }

    #[tokio::test]
    async fn touched_stale_snapshot_and_same_store_bulk_cannot_overwrite_newer_revision() {
        let home = tempdir().unwrap();
        let a = SessionStore::from_home(home.path());
        let first = a.get_or_create_for_routing_id("strict-cas").await;
        let b = SessionStore::from_home(home.path());
        let mut stale = b.get(&first.session_id).await.unwrap();
        let old_bulk: std::collections::HashMap<_, _> = a
            .list()
            .await
            .into_iter()
            .map(|entry| (entry.session_id.clone(), entry))
            .collect();
        a.try_update(&first.session_id, |entry| {
            entry.label = Some("latest".into())
        })
        .await
        .unwrap();
        stale.add_tokens(100, 200);
        stale.touch();
        b.upsert(stale.clone()).await;
        assert!(a.replace_all(old_bulk).await.is_err());
        let latest = SessionStore::from_home(home.path())
            .get(&first.session_id)
            .await
            .unwrap();
        assert_eq!(latest.label.as_deref(), Some("latest"));
        assert_eq!(latest.total_tokens, 0);
        a.try_remove(&first.session_id).await.unwrap();
        let recreated = a.get_or_create(&first.session_id).await.unwrap();
        assert_ne!(recreated.lifecycle_generation, stale.lifecycle_generation);
        b.upsert(stale).await;
        assert_eq!(
            SessionStore::from_home(home.path())
                .get(&first.session_id)
                .await
                .unwrap()
                .lifecycle_generation,
            recreated.lifecycle_generation
        );
    }

    #[tokio::test]
    async fn migration_keeps_sessions_added_after_its_snapshot() {
        let home = tempdir().unwrap();
        let store = SessionStore::from_home(home.path());
        let first = store.get_or_create_for_routing_id("old-routing").await;
        let mut snapshot: std::collections::HashMap<_, _> = store
            .list()
            .await
            .into_iter()
            .map(|entry| (entry.session_id.clone(), entry))
            .collect();
        let added = store
            .get_or_create_for_routing_id("added-during-migration")
            .await;
        snapshot.get_mut(&first.session_id).unwrap().routing_id = Some("migrated-routing".into());
        store.apply_bulk_updates(snapshot).await.unwrap();
        let reopened = SessionStore::from_home(home.path());
        assert!(reopened.get(&added.session_id).await.is_some());
        assert_eq!(
            reopened
                .get(&first.session_id)
                .await
                .unwrap()
                .routing_id
                .as_deref(),
            Some("migrated-routing")
        );
    }

    #[tokio::test]
    async fn database_read_failure_does_not_publish_empty_refresh() {
        let home = tempdir().unwrap();
        let mut store = SessionStore::from_home(home.path());
        let entry = store.get_or_create_for_routing_id("keep-cache").await;
        store.config.cache_ttl = Duration::ZERO;
        sqlx::query("DROP TABLE session_metadata")
            .execute(store.database().await.unwrap())
            .await
            .unwrap();
        assert!(store.get(&entry.session_id).await.is_some());
        assert!(
            store
                .try_update(&entry.session_id, |entry| entry.pinned = true)
                .await
                .is_err()
        );
        assert!(!store.get(&entry.session_id).await.unwrap().pinned);
    }

    #[tokio::test]
    async fn legacy_import_is_once_only_and_row_updates_leave_other_sessions_untouched() {
        let home = tempdir().unwrap();
        let mut entry = SessionEntry::new();
        entry.label = Some("legacy".into());
        let mut other = SessionEntry::new();
        other.label = Some("other".into());
        let snapshot = serde_json::to_vec(&std::collections::HashMap::from([
            (entry.session_id.clone(), entry.clone()),
            (other.session_id.clone(), other.clone()),
        ]))
        .unwrap();
        let legacy = home.path().join("gateway-session-store.json");
        tokio::fs::write(&legacy, &snapshot).await.unwrap();
        let store = SessionStore::from_home(home.path());
        assert_eq!(
            store.get(&entry.session_id).await.unwrap().label.as_deref(),
            Some("legacy")
        );
        sqlx::query("CREATE TRIGGER protect_other BEFORE UPDATE ON session_metadata WHEN json_extract(OLD.data, '$.label') = 'other' BEGIN SELECT RAISE(FAIL, 'unrelated row written'); END")
            .execute(store.database().await.unwrap()).await.unwrap();
        assert!(store.set_label(&entry.session_id, "updated".into()).await);
        assert_eq!(tokio::fs::read(&legacy).await.unwrap(), snapshot);
        let reloaded = SessionStore::from_home(home.path());
        assert_eq!(
            reloaded
                .get(&entry.session_id)
                .await
                .unwrap()
                .label
                .as_deref(),
            Some("updated")
        );
        assert_eq!(
            reloaded
                .get(&other.session_id)
                .await
                .unwrap()
                .label
                .as_deref(),
            Some("other")
        );
    }

    #[tokio::test]
    async fn failed_replacement_and_removal_preserve_durable_and_cached_state() {
        let home = tempdir().unwrap();
        let store = SessionStore::from_home(home.path());
        let entry = store.get_or_create_for_routing_id("rollback").await;
        sqlx::query("CREATE TRIGGER fail_updates BEFORE UPDATE ON session_metadata BEGIN SELECT RAISE(FAIL, 'injected failure'); END")
            .execute(store.database().await.unwrap()).await.unwrap();
        assert!(
            store
                .replace_all(std::collections::HashMap::new())
                .await
                .is_err()
        );
        assert!(store.try_remove(&entry.session_id).await.is_err());
        assert!(store.get(&entry.session_id).await.is_some());
        assert!(
            SessionStore::from_home(home.path())
                .get(&entry.session_id)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn failed_legacy_import_can_be_retried_without_marking_migration_done() {
        let home = tempdir().unwrap();
        let legacy = home.path().join("gateway-session-store.json");
        tokio::fs::write(&legacy, b"broken json").await.unwrap();
        let store = SessionStore::from_home(home.path());
        assert!(store.database().await.is_err());
        let entry = SessionEntry::new();
        tokio::fs::write(
            &legacy,
            serde_json::to_vec(&std::collections::HashMap::from([(
                entry.session_id.clone(),
                entry.clone(),
            )]))
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(store.get(&entry.session_id).await.is_some());
        assert!(store.try_remove(&entry.session_id).await.unwrap());
        // A durable tombstone wins even if a rollout scan finds the old ID.
        let mut discovered = std::collections::HashMap::from([(entry.session_id.clone(), entry)]);
        store
            .overlay_persisted_entries(&mut discovered)
            .await
            .unwrap();
        assert!(discovered.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_updates_preserve_counters_and_persist_latest_state() {
        let home = tempdir().expect("tempdir");
        let store = Arc::new(SessionStore::from_home(home.path()));
        let entry = store
            .get_or_create_for_routing_id("concurrent-updates")
            .await;
        let active = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(tokio::sync::Barrier::new(33));
        let mut updates = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let store = Arc::clone(&store);
            let active = Arc::clone(&active);
            let start = Arc::clone(&start);
            let id = entry.session_id.clone();
            updates.spawn(async move {
                start.wait().await;
                store
                    .update(&id, |entry| {
                        assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                        // Widen the window in which the old clone/modify/upsert
                        // implementation ran competing closures concurrently.
                        std::thread::sleep(Duration::from_millis(2));
                        entry.add_tokens(2, 3);
                        active.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await
                    .expect("updated entry");
            });
        }
        start.wait().await;
        while let Some(result) = updates.join_next().await {
            result.expect("update task");
        }
        let current = store.get(&entry.session_id).await.expect("cached entry");
        assert_eq!(current.input_tokens, 64);
        assert_eq!(current.output_tokens, 96);
        assert_eq!(current.total_tokens, 160);
        let reloaded = SessionStore::from_home(home.path());
        let durable = reloaded
            .get(&entry.session_id)
            .await
            .expect("durable entry");
        assert_eq!(durable.version, current.version);
        assert_eq!(durable.total_tokens, current.total_tokens);
    }

    #[tokio::test]
    async fn equal_version_replacement_does_not_overwrite_a_competing_update() {
        let home = tempdir().expect("tempdir");
        let store = SessionStore::from_home(home.path());
        let mut first = store.get_or_create_for_routing_id("stale-writes").await;
        let mut stale = first.clone();
        first.label = Some("first".to_owned());
        stale.label = Some("stale".to_owned());
        store.upsert(first).await;
        store.upsert(stale.clone()).await;
        let stored = store.get(&stale.session_id).await.expect("stored entry");
        assert_eq!(stored.label.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn persists_routing_ids_across_store_reloads() {
        let home = tempdir().expect("tempdir");
        let store = SessionStore::from_home(home.path());

        let created = store
            .get_or_create_for_routing_id("agent:default:feishu:group:oc_chat_1")
            .await;

        let reloaded = SessionStore::from_home(home.path());
        let restored = reloaded
            .get_by_routing_id("agent:default:feishu:group:oc_chat_1")
            .await
            .expect("routing id should be restored");

        assert_eq!(restored.session_id, created.session_id);
    }

    #[tokio::test]
    async fn pinned_and_archived_state_survives_reload_and_can_be_cleared() {
        let home = tempdir().expect("tempdir");
        let store = SessionStore::from_home(home.path());
        let entry = store.get_or_create_for_routing_id("ui-state").await;
        assert!(store.set_pinned(&entry.session_id, true).await);
        assert!(store.set_archived(&entry.session_id, true).await);
        let reloaded = SessionStore::from_home(home.path());
        let restored = reloaded.get(&entry.session_id).await.expect("restored");
        assert!(restored.pinned);
        assert!(restored.archived);
        assert!(restored.version > entry.version);
        assert!(reloaded.set_pinned(&entry.session_id, false).await);
        assert!(reloaded.set_archived(&entry.session_id, false).await);
        let cleared = SessionStore::from_home(home.path())
            .get(&entry.session_id)
            .await
            .expect("cleared");
        assert!(!cleared.pinned);
        assert!(!cleared.archived);
        let missing = uuid::Uuid::now_v7().to_string();
        assert!(!store.set_pinned(&missing, true).await);
        assert!(!store.set_archived(&missing, true).await);
    }

    #[tokio::test]
    async fn failed_durable_ui_update_keeps_cached_state_and_reports_failure() {
        let home = tempdir().expect("tempdir");
        let store = SessionStore::from_home(home.path());
        let entry = store.get_or_create_for_routing_id("write-failure").await;
        sqlx::query("CREATE TRIGGER fail_updates BEFORE UPDATE ON session_metadata BEGIN SELECT RAISE(FAIL, 'injected storage failure'); END")
            .execute(store.database().await.expect("database")).await.expect("install failure trigger");
        assert!(!store.set_pinned(&entry.session_id, true).await);
        assert!(!store.set_archived(&entry.session_id, true).await);
        let cached = store.get(&entry.session_id).await.expect("cached entry");
        assert!(!cached.pinned);
        assert!(!cached.archived);
        assert_eq!(cached.version, entry.version);
    }

    #[test]
    fn legacy_session_metadata_defaults_ui_state_to_false() {
        let mut json = serde_json::to_value(SessionEntry::new()).expect("serialize");
        json.as_object_mut().expect("object").remove("pinned");
        json.as_object_mut().expect("object").remove("archived");
        let restored: SessionEntry = serde_json::from_value(json).expect("legacy metadata");
        assert!(!restored.pinned);
        assert!(!restored.archived);
    }

    #[tokio::test]
    async fn prune_report_returns_removed_session_ids() {
        let home = tempdir().expect("tempdir");
        let store = SessionStore::from_home(home.path());

        let active_id = {
            let mut entry = SessionEntry::new();
            entry.updated_at = crate::json_store::now_ms();
            let id = entry.session_id.clone();
            store.upsert(entry).await;
            id
        };

        let stale_id = {
            let mut entry = SessionEntry::new();
            entry.updated_at = crate::json_store::now_ms()
                .saturating_sub(Duration::from_secs(31 * 24 * 3600).as_millis() as u64);
            let id = entry.session_id.clone();
            store.upsert(entry).await;
            id
        };

        let report = store.prune_report().await;
        assert_eq!(report.pruned, 1);
        assert_eq!(report.session_ids, vec![stale_id.clone()]);

        let remaining = store.list().await;
        assert!(remaining.iter().any(|entry| entry.session_id == active_id));
        assert!(!remaining.iter().any(|entry| entry.session_id == stale_id));
    }
}

// ─── Reset Policy ──────────────────────────────────────────────────────────

/// Session reset policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[derive(Default)]
pub enum ResetPolicy {
    /// Never reset automatically.
    #[default]
    Never,
    /// Reset daily at a specific hour (0-23, local time).
    Daily { hour: u8 },
    /// Reset after idle for a given number of seconds.
    Idle { timeout_secs: u64 },
}

impl ResetPolicy {
    /// Check if a session should be reset based on its last update time.
    #[must_use]
    pub fn should_reset(&self, last_updated_ms: u64) -> bool {
        let now = crate::json_store::now_ms();
        match self {
            Self::Never => false,
            Self::Daily { hour } => {
                let hour = *hour as u64;
                // Check if midnight + hour has passed since last update.
                let today_reset_ms = today_at_hour_ms(hour);
                last_updated_ms < today_reset_ms && now >= today_reset_ms
            }
            Self::Idle { timeout_secs } => {
                let idle_ms = now.saturating_sub(last_updated_ms);
                idle_ms > timeout_secs * 1000
            }
        }
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────────

fn today_at_hour_ms(hour: u64) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let day_start = now - (now % 86400);
    (day_start + hour * 3600) * 1000
}

#[must_use]
pub fn is_uuid_v7(session_id: &str) -> bool {
    let Ok(uuid) = uuid::Uuid::parse_str(session_id) else {
        return false;
    };
    uuid.get_version_num() == 7
}

/// Create an `Arc<SessionStore>` from the SAVFOX_HOME env or default.
pub fn create_session_store() -> Arc<SessionStore> {
    let home = std::env::var("SAVFOX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".savfox")
        });
    Arc::new(SessionStore::from_home(&home))
}
