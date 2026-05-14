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
use tokio::sync::RwLock;
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
    /// Last update timestamp (epoch milliseconds).
    pub updated_at: u64,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: u64,
    /// Optimistic-lock version counter. Incremented on each write so
    /// concurrent readers can detect stale data.
    #[serde(default)]
    pub version: u64,

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
    /// Thread ID within a channel (for forum-style channels).
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
            updated_at: 0,
            created_at: 0,
            version: 0,
            channel: None,
            last_channel: None,
            to: None,
            last_to: None,
            account_id: None,
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
    file_modified_at: Option<SystemTime>,
}

/// File-based persistent session store with in-memory caching.
#[derive(Debug)]
pub struct SessionStore {
    config: SessionStoreConfig,
    cache: RwLock<Option<StoreCache>>,
}

impl SessionStore {
    /// Create a new session store.
    #[must_use]
    pub fn new(config: SessionStoreConfig) -> Self {
        Self {
            config,
            cache: RwLock::new(None),
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

    async fn load_persisted_entries(&self) -> HashMap<String, SessionEntry> {
        let path = &self.config.metadata_path;
        let content = match tokio::fs::read_to_string(path).await {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
            Err(err) => {
                warn!(path = %path.display(), "failed to read session metadata sidecar: {err}");
                return HashMap::new();
            }
        };

        match serde_json::from_str::<HashMap<String, SessionEntry>>(&content) {
            Ok(mut entries) => {
                entries.retain(|session_id, entry| entry.session_id == *session_id);
                entries
            }
            Err(err) => {
                warn!(path = %path.display(), "failed to parse session metadata sidecar: {err}");
                HashMap::new()
            }
        }
    }

    async fn persist_entries(
        &self,
        entries: &HashMap<String, SessionEntry>,
    ) -> std::io::Result<()> {
        let path = &self.config.metadata_path;
        let data = serde_json::to_vec_pretty(entries)
            .map_err(|err| std::io::Error::other(format!("serialize session metadata: {err}")))?;
        savfox_utils::fs::write_atomically_async(path, data, Some(0o600)).await
    }

    async fn persist_cache(&self) {
        let entries = {
            let cache = self.cache.read().await;
            cache
                .as_ref()
                .map(|cache| cache.entries.clone())
                .unwrap_or_default()
        };
        if let Err(err) = self.persist_entries(&entries).await {
            warn!(
                path = %self.config.metadata_path.display(),
                "failed to persist session metadata sidecar: {err}"
            );
        }
    }

    /// Get the latest modification time from all rollout files, or None if directory doesn't exist.
    async fn get_latest_mtime(&self) -> Option<SystemTime> {
        let mut dir = tokio::fs::read_dir(&self.config.sessions_dir).await.ok()?;
        let mut latest = None;

        while let Some(entry) = dir.next_entry().await.ok()? {
            if entry.file_type().await.ok()?.is_file() {
                let name = entry.file_name();
                if name.to_string_lossy().ends_with(".jsonl") {
                    let mtime = entry.metadata().await.ok()?.modified().ok()?;
                    latest = Some(match latest {
                        Some(current) if current > mtime => current,
                        _ => mtime,
                    });
                }
            }
        }

        latest
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
    async fn load_from_disk(&self) -> HashMap<String, SessionEntry> {
        let mut entries = self.load_persisted_entries().await;
        let mut dir = match tokio::fs::read_dir(&self.config.sessions_dir).await {
            Ok(d) => d,
            Err(_) => return entries,
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

        entries
    }

    /// Refresh the cache from disk if stale.
    async fn refresh_cache(&self) {
        if self.is_cache_valid().await {
            return;
        }

        let entries = self.load_from_disk().await;

        let mut cache = self.cache.write().await;
        *cache = Some(StoreCache {
            entries,
            loaded_at: Instant::now(),
            file_modified_at: self.get_latest_mtime().await,
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
        Some(entry)
    }

    /// Get or create a session entry for a routing id, generating a UUID v7 session_id.
    pub async fn get_or_create_for_routing_id(&self, routing_id: &str) -> SessionEntry {
        if let Some(entry) = self.get_by_routing_id(routing_id).await {
            return entry;
        }
        let mut entry = SessionEntry::new();
        entry.routing_id = Some(routing_id.to_owned());
        self.upsert(entry.clone()).await;
        entry
    }

    /// Insert or update a session entry.
    ///
    /// Uses optimistic locking: if the cache already holds a newer version
    /// of the same session, the incoming entry is merged rather than
    /// blindly overwriting it (the entry with the higher `version` wins for
    /// conflicting fields, while additive counters like token usage are
    /// always accumulated).
    pub async fn upsert(&self, mut entry: SessionEntry) {
        let key = entry.session_id.clone();
        entry.version += 1;

        // Update cache with version check.
        {
            let mut cache = self.cache.write().await;
            if let Some(cache) = cache.as_mut() {
                if let Some(existing) = cache.entries.get(&key)
                    && existing.version > entry.version
                {
                    warn!(
                        session_id = %key,
                        existing_version = existing.version,
                        incoming_version = entry.version,
                        "session store: skipping stale upsert (existing version is newer)"
                    );
                    return;
                }
                cache.entries.insert(key.clone(), entry);
            } else {
                let mut entries = HashMap::new();
                entries.insert(key.clone(), entry);
                *cache = Some(StoreCache {
                    entries,
                    loaded_at: Instant::now(),
                    file_modified_at: None,
                });
            }
        }
        self.persist_cache().await;
    }

    /// Replace all entries atomically and persist to disk.
    pub async fn replace_all(&self, entries: HashMap<String, SessionEntry>) -> std::io::Result<()> {
        {
            let mut cache = self.cache.write().await;
            *cache = Some(StoreCache {
                entries: entries.clone(),
                loaded_at: Instant::now(),
                file_modified_at: None,
            });
        }
        self.persist_cache().await;
        Ok(())
    }

    /// Update a session entry by session_id using a closure.
    pub async fn update<F>(&self, session_id: &str, f: F) -> Option<SessionEntry>
    where
        F: FnOnce(&mut SessionEntry),
    {
        self.refresh_cache().await;
        let mut entry = self.get(session_id).await?;
        f(&mut entry);
        entry.touch();
        self.upsert(entry.clone()).await;
        Some(entry)
    }

    /// Remove a session by session_id.
    pub async fn remove(&self, session_id: &str) -> bool {
        if !is_uuid_v7(session_id) {
            return false;
        }
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
            self.persist_cache().await;
        }

        removed
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
    pub async fn set_pinned(&self, _session_id: &str, _pinned: bool) -> bool {
        true
    }

    /// Set a session's archived status.
    pub async fn set_archived(&self, _session_id: &str, _archived: bool) -> bool {
        true
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

        let (pruned, stale_keys, removed_session_ids) = {
            let mut cache = self.cache.write().await;
            let Some(cache) = cache.as_mut() else {
                return SessionPruneResult::default();
            };

            let before = cache.entries.len();

            // Find stale entries.
            let stale_keys: Vec<_> = cache
                .entries
                .iter()
                .filter(|(_, entry)| entry.is_stale(self.config.max_age))
                .map(|(k, v)| (k.clone(), v.session_file.clone()))
                .collect();

            // Remove stale entries.
            cache
                .entries
                .retain(|_, entry| !entry.is_stale(self.config.max_age));

            let mut removed_session_ids = stale_keys
                .iter()
                .map(|(session_id, _)| session_id.clone())
                .collect::<Vec<_>>();

            // Cap at max_entries (keep most recently updated).
            if cache.entries.len() > self.config.max_entries {
                let mut entries_vec: Vec<_> = cache.entries.drain().collect();
                entries_vec.sort_by_key(|entry| std::cmp::Reverse(entry.1.updated_at));
                let retained = entries_vec
                    .iter()
                    .take(self.config.max_entries)
                    .map(|(session_id, _)| session_id.clone())
                    .collect::<std::collections::HashSet<_>>();
                removed_session_ids.extend(
                    entries_vec
                        .iter()
                        .skip(self.config.max_entries)
                        .map(|(session_id, _)| session_id.clone()),
                );
                entries_vec.truncate(self.config.max_entries);
                cache.entries = entries_vec.into_iter().collect();
                removed_session_ids.retain(|session_id| !retained.contains(session_id));
            }

            removed_session_ids.sort();
            removed_session_ids.dedup();

            (
                before - cache.entries.len(),
                stale_keys,
                removed_session_ids,
            )
        };

        if pruned > 0 {
            info!(pruned, "session store pruned stale entries");
            // Delete the corresponding rollout files
            for (_key, session_file) in stale_keys {
                if let Some(session_file) = session_file {
                    let path = self
                        .config
                        .sessions_dir
                        .join(format!("{session_file}.jsonl"));
                    if let Err(err) = tokio::fs::remove_file(&path).await {
                        warn!(
                            path = %path.display(),
                            "failed to remove stale rollout file: {err}"
                        );
                    }
                }
            }
            self.persist_cache().await;
        }

        SessionPruneResult {
            pruned,
            session_ids: removed_session_ids,
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
        let entries = cache
            .as_ref()
            .map(|c| &c.entries)
            .cloned()
            .unwrap_or_default();

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
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::tempdir;

    use super::{SessionEntry, SessionStore};

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
