//! Shared data types for the Savfox gateway server and its web frontend.
//!
//! This crate contains serde-based types that are used for JSON-RPC
//! communication between the backend (native) and frontend (wasm32).
//! It has no platform-specific dependencies.

use serde::{Deserialize, Serialize};

pub const SESSION_LABEL_MAX_CHARS: usize = 48;

pub fn normalize_session_label(raw: &str) -> Option<String> {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let compact = compact.trim();
    if compact.is_empty() {
        return None;
    }
    if compact.chars().count() <= SESSION_LABEL_MAX_CHARS {
        return Some(compact.to_owned());
    }
    let truncated: String = compact.chars().take(SESSION_LABEL_MAX_CHARS).collect();
    Some(format!("{truncated}..."))
}

pub fn derive_session_label(
    first_message: Option<&str>,
    topic: Option<&str>,
    display_name: Option<&str>,
) -> Option<String> {
    first_message
        .and_then(normalize_session_label)
        .or_else(|| topic.and_then(normalize_session_label))
        .or_else(|| display_name.and_then(normalize_session_label))
}

// ── Chat ──

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
    pub timestamp: Option<String>,
    pub thinking: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatAttachment {
    pub filename: String,
    pub mime_type: String,
    pub data_base64: String,
}

// ── JSON-RPC ──

/// JSON-RPC notification (server-push, no `id`)
#[derive(Debug, Deserialize)]
pub struct JsonRpcNotification {
    pub method: String,
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    pub id: u64,
    pub result: Option<serde_json::Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

// ── Status ──

#[derive(Clone, Debug, Deserialize)]
pub struct StatusResponse {
    pub connected_clients: u32,
    pub session_ids: Vec<String>,
}

// ── Sessions ──

/// Sender information for a session.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSender {
    pub user_id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionEntry {
    pub session_id: Option<String>,
    pub id: Option<String>,
    pub scope: Option<String>,
    pub label: Option<String>,
    pub subject: Option<String>,
    pub sender: Option<SessionSender>,
    pub last_activity: Option<String>,
    pub message_count: Option<u32>,
    pub messages: Option<u32>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub thread_id: Option<String>,
}

impl SessionEntry {
    pub fn display_id(&self) -> String {
        self.session_id
            .as_deref()
            .or(self.id.as_deref())
            .unwrap_or("-")
            .to_string()
    }

    pub fn display_count(&self) -> u32 {
        self.message_count.or(self.messages).unwrap_or(0)
    }

    pub fn display_label(&self) -> String {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .or_else(|| self.subject.clone())
            .or_else(|| self.sender.as_ref().and_then(|s| s.display_name.clone()))
            .unwrap_or_else(|| short_id(&self.display_id()))
    }
}

pub fn short_id(id: &str) -> String {
    if id.chars().count() <= 16 {
        id.to_string()
    } else {
        let truncated: String = id.chars().take(16).collect();
        format!("{truncated}...")
    }
}

#[derive(Debug, Deserialize)]
pub struct SessionsResponse {
    pub entries: Vec<SessionEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionDetail {
    pub session_id: Option<String>,
    pub id: Option<String>,
    pub label: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub channel: Option<String>,
    pub scope: Option<String>,
    pub last_activity: Option<String>,
    pub message_count: Option<u32>,
    pub messages: Option<u32>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub thinking_level: Option<String>,
    pub verbose_level: Option<String>,
    pub reasoning_level: Option<String>,
}

impl SessionDetail {
    pub fn display_id(&self) -> String {
        self.session_id
            .as_deref()
            .or(self.id.as_deref())
            .unwrap_or("-")
            .to_string()
    }

    pub fn display_count(&self) -> u32 {
        self.message_count.or(self.messages).unwrap_or(0)
    }
}

// ── Memory ──

#[derive(Clone, Debug, Deserialize)]
pub struct MemoryEntry {
    pub slug: String,
    pub layer: String,
    pub tags: Vec<String>,
    pub priority: i32,
    pub pinned: bool,
    pub author: String,
    pub body_bytes: u64,
    pub body: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MemoryLayer {
    pub layer: String,
    pub path: String,
    pub exists: bool,
}

#[derive(Debug, Deserialize)]
pub struct MemoryListResponse {
    pub entries: Vec<MemoryEntry>,
}

#[derive(Debug, Deserialize)]
pub struct MemoryLayersResponse {
    pub layers: Vec<MemoryLayer>,
}

// ── Models ──

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: Option<String>,
    pub provider: Option<String>,
    pub model_code: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub max_tokens: Option<i64>,
    pub temperature: Option<f64>,
    pub is_default: Option<bool>,
    pub builtin: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ModelsResponse {
    pub models: Vec<ModelInfo>,
}

// ── Agents ──

#[derive(Clone, Debug, Deserialize)]
pub struct AgentModels {
    pub primary: Option<String>,
    pub fallbacks: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentEntry {
    pub id: Option<String>,
    pub name: String,
    pub model: Option<String>,
    pub models: Option<AgentModels>,
    pub system_prompt: Option<String>,
    pub thinking: Option<String>,
    pub verbose: Option<String>,
    pub status: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentsResponse {
    pub agents: Vec<AgentEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentFile {
    #[serde(alias = "path")]
    pub name: String,
    pub size: Option<u64>,
    pub content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentFilesResponse {
    pub files: Vec<AgentFile>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentDetail {
    pub name: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub status: Option<String>,
    pub created_at: Option<String>,
    pub emoji: Option<String>,
    pub theme_color: Option<String>,
    pub is_default: Option<bool>,
    pub fallback_models: Option<Vec<String>>,
    pub tools_profile: Option<String>,
    pub tools: Option<Vec<String>>,
}

// ── Channels ──

#[derive(Clone, Debug, Deserialize)]
pub struct ChannelEntry {
    pub platform: String,
    pub name: String,
    pub status: Option<String>,
    pub connected_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChannelsResponse {
    pub channels: Vec<ChannelEntry>,
}

// ── Cron ──

#[derive(Clone, Debug, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub name: Option<String>,
    pub schedule: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub enabled: Option<bool>,
    pub next_run: Option<String>,
    pub last_run: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CronListResponse {
    pub jobs: Vec<CronJob>,
}

#[derive(Debug, Deserialize)]
pub struct CronStatusResponse {
    pub running: Option<bool>,
    pub job_count: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CronRunEntry {
    pub job_id: String,
    pub started_at: Option<String>,
    pub status: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct CronRunsResponse {
    pub runs: Vec<CronRunEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CronJobDetail {
    pub id: String,
    pub name: Option<String>,
    pub schedule: Option<serde_json::Value>,
    pub payload: Option<serde_json::Value>,
    pub enabled: Option<bool>,
    pub next_run: Option<String>,
    pub last_run: Option<String>,
    pub agent_id: Option<String>,
    pub session_target: Option<String>,
    pub wake_mode: Option<String>,
    pub delivery: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CronRunDetail {
    pub job_id: String,
    pub started_at: Option<String>,
    pub status: Option<String>,
    pub duration_ms: Option<u64>,
    pub session_id: Option<String>,
    pub error: Option<String>,
    pub result_preview: Option<String>,
}

// ── Logs ──

#[derive(Clone, Debug, Deserialize)]
pub struct LogEntry {
    pub timestamp: Option<String>,
    pub level: Option<String>,
    pub message: String,
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LogsResponse {
    pub entries: Vec<LogEntry>,
}

// ── Usage ──

#[derive(Clone, Debug, Deserialize)]
pub struct UsageStatus {
    pub total_tokens: Option<u64>,
    pub total_cost: Option<f64>,
    pub session_count: Option<u32>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsageCostEntry {
    pub session_id: String,
    pub tokens: Option<u64>,
    pub cost: Option<f64>,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct UsageCostResponse {
    pub entries: Vec<UsageCostEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsageDetail {
    pub total_tokens: Option<u64>,
    pub total_cost: Option<f64>,
    pub session_count: Option<u32>,
    pub total_messages: Option<u64>,
    pub tool_calls: Option<u64>,
    pub errors: Option<u64>,
    pub cache_hits: Option<u64>,
    pub cache_misses: Option<u64>,
    pub hourly_distribution: Option<Vec<u64>>,
    pub daily_distribution: Option<Vec<serde_json::Value>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionsUsageEntry {
    pub key: Option<String>,
    pub session_id: Option<String>,
    pub label: Option<String>,
    pub agent_id: Option<String>,
    pub channel: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost: Option<f64>,
    pub context_weight: Option<f64>,
    pub message_count: Option<u32>,
    pub first_activity: Option<i64>,
    pub last_activity: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionsUsageTotals {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cost: Option<f64>,
    pub session_count: Option<u32>,
    pub message_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionsUsageResult {
    pub updated_at: Option<i64>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub sessions: Option<Vec<SessionsUsageEntry>>,
    pub totals: Option<SessionsUsageTotals>,
    pub daily: Option<Vec<serde_json::Value>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CostUsageDailyEntry {
    pub date: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost: Option<f64>,
    pub session_count: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CostUsageSummary {
    pub updated_at: Option<i64>,
    pub days: Option<u32>,
    pub daily: Option<Vec<CostUsageDailyEntry>>,
    pub totals: Option<SessionsUsageTotals>,
}

// ── Approvals ──

#[derive(Clone, Debug, Deserialize)]
pub struct ApprovalSettings {
    pub enabled: Option<bool>,
    pub auto_approve_safe: Option<bool>,
    pub timeout_secs: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub command: Option<String>,
    pub node: Option<String>,
    pub timestamp: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApprovalsResponse {
    pub pending: Vec<ApprovalRequest>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ExecApprovalFull {
    pub id: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub host: Option<String>,
    pub security: Option<String>,
    pub ask: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub expires_at_ms: Option<u64>,
    pub node: Option<String>,
    pub timestamp: Option<String>,
    pub status: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExecApprovalPolicy {
    pub mode: Option<String>,
    pub rules: Option<Vec<String>>,
}

// ── Nodes ──

#[derive(Clone, Debug, Deserialize)]
pub struct NodeEntry {
    pub id: String,
    pub name: Option<String>,
    pub status: Option<String>,
    pub platform: Option<String>,
    pub last_seen: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NodesResponse {
    pub nodes: Vec<NodeEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DevicePairEntry {
    pub id: String,
    pub name: Option<String>,
    pub status: Option<String>,
    pub requested_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeDetail {
    pub id: String,
    pub name: Option<String>,
    pub status: Option<String>,
    pub platform: Option<String>,
    pub last_seen: Option<String>,
    pub capabilities: Option<Vec<String>>,
    pub commands: Option<Vec<String>>,
    pub tokens: Option<Vec<NodeToken>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeToken {
    pub role: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub status: Option<String>,
}

// ── Skills ──

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub version: Option<String>,
    pub installed: Option<bool>,
    pub category: Option<String>,
    pub eligible: Option<bool>,
    pub missing_deps: Option<Vec<String>>,
    #[serde(alias = "requires_key_env")]
    pub primary_env: Option<String>,
    pub env_set: Option<bool>,
    pub enabled: Option<bool>,
    pub description: Option<String>,
    pub disabled_reason: Option<String>,
    pub allowlist_blocked: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct SkillsStatusResponse {
    pub installed_count: Option<u32>,
    pub available_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct SkillsBinsResponse {
    pub bins: Vec<SkillEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SkillDetail {
    pub name: String,
    pub version: Option<String>,
    pub installed: Option<bool>,
    pub category: Option<String>,
    pub eligible: Option<bool>,
    pub missing_deps: Option<Vec<String>>,
    #[serde(alias = "requires_key_env")]
    pub primary_env: Option<String>,
    pub env_set: Option<bool>,
    pub enabled: Option<bool>,
    pub description: Option<String>,
    pub disabled_reason: Option<String>,
    pub allowlist_blocked: Option<bool>,
}

// ── Config ──

#[derive(Debug, Deserialize)]
pub struct ConfigSchema {
    pub properties: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ConfigSnapshot {
    pub path: Option<String>,
    pub exists: Option<bool>,
    pub valid: Option<bool>,
    pub hash: Option<String>,
    pub config: Option<serde_json::Value>,
    pub issues: Option<Vec<ConfigSnapshotIssue>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ConfigSnapshotIssue {
    pub path: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ConfigPatchResult {
    pub ok: Option<bool>,
    pub path: Option<String>,
    pub key: Option<String>,
    pub entry: Option<serde_json::Value>,
}

// ── TTS ──

#[derive(Clone, Debug, Deserialize)]
pub struct TtsStatus {
    pub enabled: Option<bool>,
    pub provider: Option<String>,
    pub voice: Option<String>,
}

// ── Wizard ──

#[derive(Clone, Debug, Deserialize)]
pub struct WizardStatus {
    pub state: Option<String>,
    pub current_step: Option<u32>,
    pub total_steps: Option<u32>,
    pub step_title: Option<String>,
    pub step_description: Option<String>,
}

// ── Voice ──

#[derive(Clone, Debug, Deserialize)]
pub struct VoiceSettings {
    pub enabled: Option<bool>,
    pub wake_word: Option<String>,
    pub sensitivity: Option<f64>,
}

// ── Channel Status Types ──

#[derive(Clone, Debug, Deserialize)]
pub struct ChannelsStatusSnapshot {
    pub ts: Option<i64>,
    pub channels: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChannelAccountSnapshot {
    pub account_id: String,
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub last_inbound_at: Option<i64>,
    pub last_outbound_at: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DiscordStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub guild_count: Option<u32>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
    pub bot_username: Option<String>,
    pub bot_avatar_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TelegramStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub mode: Option<String>,
    pub bot_username: Option<String>,
    pub last_probe_at: Option<i64>,
    pub last_error: Option<String>,
    pub probe: Option<TelegramProbeResult>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TelegramProbeResult {
    pub ok: Option<bool>,
    pub status: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct WhatsAppStatus {
    pub configured: Option<bool>,
    pub linked: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub auth_age_ms: Option<i64>,
    pub last_connected_at: Option<i64>,
    pub last_message_at: Option<i64>,
    pub last_error: Option<String>,
    pub qr_data_url: Option<String>,
    pub self_user: Option<WhatsAppSelfUser>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct WhatsAppSelfUser {
    pub push_name: Option<String>,
    pub id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SlackStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub workspace_name: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SignalStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub phone_number: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct IMessageStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NostrStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub public_key: Option<String>,
    pub relay_count: Option<u32>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct GoogleChatStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct MatrixStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub homeserver: Option<String>,
    pub user_id: Option<String>,
    pub room_count: Option<u32>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct MattermostStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub server_url: Option<String>,
    pub team_name: Option<String>,
    pub bot_username: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct LineStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub bot_name: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct FeishuStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub app_id: Option<String>,
    pub bot_name: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct IrcStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub server: Option<String>,
    pub nickname: Option<String>,
    pub channel_count: Option<u32>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct MsTeamsStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub tenant_id: Option<String>,
    pub bot_name: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}
