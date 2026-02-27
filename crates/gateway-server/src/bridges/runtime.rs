use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{info, warn};

use savfox_core::models_manager::manager::RefreshStrategy;

use crate::agent_routing::{AgentRouter, RoutingContext, RoutingRule};
use crate::auto_reply::directives::{
    DirectiveKind, fuzzy_match_model_name, parse_directives, parse_model_target,
};
use crate::auto_reply::{CommandAction, CommandContext, CommandRegistry};
use crate::bridge::GatewayBridge;
use crate::compaction::{CompactionConfig, CompactionService};
use crate::config::ResponseFooterConfig;
use crate::identity_links::{
    canonical_for_peer, load_identity_links, peers_for_identity, platform_peer,
    save_identity_links, upsert_link,
};
use crate::log_store;
use crate::response_chunker::chunk_message_for_channel;
use crate::send_policy::{SendMetrics, SendPolicyConfig, ThreadingPolicy};
use crate::session::session_file_to_store_value;
use crate::session::{DmScope, SessionOverrides, SessionStore};
use crate::session::{InboundSessionMeta, track_inbound_message, track_token_usage};

const DEDUPE_TTL_MS: u64 = 10 * 60 * 1000;

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn dedupe_cache() -> &'static Mutex<HashMap<String, u64>> {
    static CACHE: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn thread_once_cache() -> &'static Mutex<HashSet<String>> {
    static CACHE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn send_metrics_store() -> &'static Mutex<HashMap<String, SendMetrics>> {
    static STORE: OnceLock<Mutex<HashMap<String, SendMetrics>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) async fn send_metrics_snapshot() -> HashMap<String, SendMetrics> {
    send_metrics_store().lock().await.clone()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ChannelHealthMetrics {
    pub last_message_time_ms: Option<u64>,
    pub last_event_time_ms: Option<u64>,
    pub reconnect_attempt_count: u64,
    pub connected_since_ms: Option<u64>,
    pub last_probe_time_ms: Option<u64>,
    pub probe_status: Option<String>,
}

impl ChannelHealthMetrics {
    fn mark_event(&mut self, timestamp_ms: u64) {
        self.last_event_time_ms = Some(timestamp_ms);
    }

    fn mark_send(&mut self, success: bool, timestamp_ms: u64) {
        if success {
            self.last_message_time_ms = Some(timestamp_ms);
            if self.connected_since_ms.is_none() {
                self.connected_since_ms = Some(timestamp_ms);
            }
        } else {
            self.reconnect_attempt_count = self.reconnect_attempt_count.saturating_add(1);
        }
    }

    fn mark_probe(&mut self, status: &str, timestamp_ms: u64) {
        self.last_probe_time_ms = Some(timestamp_ms);
        self.probe_status = Some(status.to_string());
    }
}

fn channel_health_store() -> &'static Mutex<HashMap<String, ChannelHealthMetrics>> {
    static STORE: OnceLock<Mutex<HashMap<String, ChannelHealthMetrics>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) async fn channel_health_snapshot() -> HashMap<String, ChannelHealthMetrics> {
    channel_health_store().lock().await.clone()
}

pub(crate) async fn record_channel_probe(platform: &str, status: &str) {
    let platform = platform.trim().to_ascii_lowercase();
    if platform.is_empty() {
        return;
    }
    let now = now_epoch_ms();
    let mut lock = channel_health_store().lock().await;
    lock.entry(platform).or_default().mark_probe(status, now);
}

async fn record_channel_event(channel: &str) {
    let now = now_epoch_ms();
    let platform = channel.split_once(':').map(|(p, _)| p).unwrap_or(channel);
    let mut lock = channel_health_store().lock().await;
    for key in [platform.to_string(), "*".to_string()] {
        lock.entry(key).or_default().mark_event(now);
    }
}

fn command_registry() -> &'static CommandRegistry {
    static REGISTRY: OnceLock<CommandRegistry> = OnceLock::new();
    REGISTRY.get_or_init(CommandRegistry::new)
}

fn compaction_service() -> &'static CompactionService {
    static SERVICE: OnceLock<CompactionService> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let threshold = std::env::var("SAVFOX_MEMORY_FLUSH_SOFT_THRESHOLD_TOKENS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(50_000);
        let prompt = std::env::var("SAVFOX_MEMORY_FLUSH_PROMPT")
            .ok()
            .and_then(|v| {
                let trimmed = v.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            });
        CompactionService::new(CompactionConfig {
            memory_flush_enabled: true,
            memory_flush_soft_threshold_tokens: threshold,
            memory_flush_prompt: prompt,
            ..CompactionConfig::default()
        })
    })
}

fn footer_config_store() -> &'static std::sync::RwLock<ResponseFooterConfig> {
    static STORE: OnceLock<std::sync::RwLock<ResponseFooterConfig>> = OnceLock::new();
    STORE.get_or_init(|| std::sync::RwLock::new(ResponseFooterConfig::default()))
}

pub(crate) fn set_response_footer_config(config: ResponseFooterConfig) {
    if let Ok(mut lock) = footer_config_store().write() {
        *lock = config;
    }
}

fn current_response_footer_config() -> ResponseFooterConfig {
    footer_config_store()
        .read()
        .map(|cfg| cfg.clone())
        .unwrap_or_else(|_| ResponseFooterConfig::default())
}

fn truncate_chars(text: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= max_len {
        return text.to_string();
    }
    if max_len <= 3 {
        return ".".repeat(max_len);
    }
    let mut out = String::new();
    for ch in text.chars().take(max_len - 3) {
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn render_footer_template(
    template: &str,
    model: &str,
    provider: &str,
    profile: Option<&str>,
    usage: Option<&savfox_protocol::protocol::TokenUsage>,
) -> String {
    let profile_value = profile.unwrap_or("").trim().to_string();
    let token_value = usage.map(|u| u.total_tokens.max(0));
    let cost_value = token_value.map(|v| v as f64 * 0.00001);

    let profile_segment = if profile_value.is_empty() {
        String::new()
    } else {
        format!(" | profile: {profile_value}")
    };
    let tokens_segment = token_value
        .map(|v| format!(" | tokens: {v}"))
        .unwrap_or_default();
    let cost_segment = cost_value
        .map(|v| format!(" | est. cost: ${v:.4}"))
        .unwrap_or_default();
    let tokens_text = token_value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "n/a".to_string());
    let cost_text = cost_value
        .map(|v| format!("{v:.4}"))
        .unwrap_or_else(|| "n/a".to_string());

    template
        .replace("{model}", model)
        .replace("{provider}", provider)
        .replace("{profile}", &profile_value)
        .replace("{tokens}", &tokens_text)
        .replace("{cost}", &cost_text)
        .replace("{profile_segment}", &profile_segment)
        .replace("{tokens_segment}", &tokens_segment)
        .replace("{cost_segment}", &cost_segment)
}

fn format_model_footer(
    config: &ResponseFooterConfig,
    platform: &str,
    model: &str,
    provider: &str,
    profile: Option<&str>,
    usage: Option<&savfox_protocol::protocol::TokenUsage>,
) -> Option<String> {
    if !config.enabled {
        return None;
    }
    let template = config
        .channel_templates
        .get(platform)
        .map(String::as_str)
        .unwrap_or(config.template.as_str());
    let rendered = render_footer_template(template, model, provider, profile, usage);
    let rendered = rendered.trim();
    if rendered.is_empty() {
        return None;
    }
    let max_len = config
        .channel_max_length
        .get(platform)
        .copied()
        .unwrap_or(config.max_length);
    let footer = truncate_chars(rendered, max_len);
    if footer.trim().is_empty() {
        None
    } else {
        Some(footer)
    }
}

fn append_footer(reply: &str, footer: &str) -> String {
    let reply = reply.trim_end();
    if reply.is_empty() {
        footer.to_string()
    } else {
        format!("{reply}\n\n---\n{footer}")
    }
}

fn command_result_message(
    result: &crate::auto_reply::CommandResult,
    fallback: &'static str,
) -> String {
    if let Some(error) = result.error.as_deref() {
        return format!("Command error: {error}");
    }
    result.reply.clone().unwrap_or_else(|| fallback.to_string())
}

async fn apply_command_action(
    session_store: &Arc<SessionStore>,
    session_id: &str,
    action: &CommandAction,
) {
    match action {
        CommandAction::SetModel { model } => {
            let model_value = model.clone();
            let provider_value = model_value
                .split('/')
                .next()
                .map(std::string::ToString::to_string);
            let _ = session_store
                .update(session_id, move |entry| {
                    entry.model = Some(model_value.clone());
                    entry.provider = provider_value.clone();
                    entry.patch_overrides(SessionOverrides {
                        model: Some(model_value.clone()),
                        ..SessionOverrides::default()
                    });
                })
                .await;
        }
        CommandAction::SetThinking { level } => {
            let level_value = level.clone();
            let _ = session_store
                .update(session_id, move |entry| {
                    entry.patch_overrides(SessionOverrides {
                        thinking: Some(level_value.clone()),
                        ..SessionOverrides::default()
                    });
                })
                .await;
        }
        CommandAction::SetVerbose { enabled } => {
            let verbose_value = if *enabled {
                "on".to_string()
            } else {
                "off".to_string()
            };
            let _ = session_store
                .update(session_id, move |entry| {
                    entry.patch_overrides(SessionOverrides {
                        verbose: Some(verbose_value.clone()),
                        ..SessionOverrides::default()
                    });
                })
                .await;
        }
        CommandAction::Compact { .. } => {
            let _ = session_store
                .update(session_id, |entry| {
                    entry.compaction_count = entry.compaction_count.saturating_add(1);
                    entry.touch();
                })
                .await;
        }
        CommandAction::Reset => {
            let _ = session_store
                .update(session_id, |entry| {
                    entry.model = None;
                    entry.provider = None;
                    entry.overrides = None;
                    entry.thread_id = None;
                    entry.session_file = None;
                    entry.touch();
                })
                .await;
        }
        CommandAction::Stop
        | CommandAction::NewSession
        | CommandAction::ShowHelp
        | CommandAction::ShowStatus => {}
    }
}

fn sanitize_session_id_for_path(session_id: &str) -> String {
    let mut out = String::with_capacity(session_id.len());
    for ch in session_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "session".to_string()
    } else {
        out
    }
}

fn memory_flush_markdown(session_id: &str, flush: &serde_json::Value) -> String {
    let metadata = flush
        .get("metadata")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let removed_count = metadata
        .get("removed_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let pre_tokens = metadata
        .get("pre_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let post_tokens = metadata
        .get("post_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let prompt = metadata
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let summary = flush
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    format!(
        "# Compaction Memory Flush\n\n\
         - session_id: `{session_id}`\n\
         - removed_count: {removed_count}\n\
         - pre_tokens: {pre_tokens}\n\
         - post_tokens: {post_tokens}\n\
         - prompt: {prompt}\n\n\
         ## Summary\n\n\
         {summary}\n"
    )
}

async fn persist_memory_flush_record(
    savfox_home: &Path,
    session_id: &str,
    flush: &serde_json::Value,
) -> Result<(u64, u64), std::io::Error> {
    let dir = savfox_home.join("sessions").join("compaction_flush");
    tokio::fs::create_dir_all(&dir).await?;

    let file_name = format!(
        "{}-{}.md",
        sanitize_session_id_for_path(session_id),
        now_epoch_ms()
    );
    let file_path = dir.join(file_name);
    let markdown = memory_flush_markdown(session_id, flush);
    tokio::fs::write(&file_path, markdown.as_bytes()).await?;

    let metadata = flush
        .get("metadata")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let pre = metadata
        .get("pre_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let post = metadata
        .get("post_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let bytes = markdown.len() as u64;
    Ok((bytes, pre.saturating_sub(post)))
}

async fn maybe_auto_memory_flush(
    session_store: &Arc<SessionStore>,
    savfox_home: &Path,
    session_id: &str,
    previous_total_tokens: u64,
    input_tokens: u64,
    output_tokens: u64,
    prompt: &str,
    reply: &str,
) {
    if input_tokens == 0 && output_tokens == 0 {
        return;
    }

    let service = compaction_service();
    let cfg = service.config();
    if !cfg.memory_flush_enabled {
        return;
    }

    let threshold = cfg.memory_flush_soft_threshold_tokens.max(1);
    let updated_total = previous_total_tokens
        .saturating_add(input_tokens)
        .saturating_add(output_tokens);

    if updated_total < threshold {
        return;
    }

    // Trigger once per threshold bucket to avoid flushing every single message.
    if previous_total_tokens / threshold == updated_total / threshold {
        return;
    }

    let messages = vec![
        serde_json::json!({
            "role": "user",
            "content": prompt,
        }),
        serde_json::json!({
            "role": "assistant",
            "content": reply,
        }),
    ];
    let compacted = service.compact(session_id, &messages, 1);
    let Some(flush_entry) = service.generate_memory_flush(session_id, &compacted) else {
        return;
    };

    match persist_memory_flush_record(savfox_home, session_id, &flush_entry).await {
        Ok((bytes, tokens_saved)) => {
            let _ = session_store
                .update(session_id, |entry| {
                    entry.compaction_count = entry.compaction_count.saturating_add(1);
                    entry.memory_flush_count = entry.memory_flush_count.saturating_add(1);
                    entry.memory_flush_bytes = entry.memory_flush_bytes.saturating_add(bytes);
                    entry.memory_flush_tokens_saved =
                        entry.memory_flush_tokens_saved.saturating_add(tokens_saved);
                    entry.touch();
                })
                .await;
            info!(
                session_id,
                bytes, tokens_saved, "auto memory flush persisted for compaction"
            );
            log_store::append_log(
                "info",
                "bridge/runtime",
                format!(
                    "memory flush persisted: session_id={session_id}, bytes={bytes}, tokens_saved={tokens_saved}"
                ),
            )
            .await;
        }
        Err(err) => {
            warn!(
                session_id = %session_id,
                "failed to persist memory flush record: {err}"
            );
            log_store::append_log(
                "warn",
                "bridge/runtime",
                format!("memory flush persist failed: session_id={session_id}, err={err}"),
            )
            .await;
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct StartThreadMeta {
    pub peer_id: Option<String>,
    pub group_id: Option<String>,
    pub thread_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub reply_target: Option<String>,
    pub guild_id: Option<String>,
    pub team_id: Option<String>,
    pub account_id: Option<String>,
    pub parent_sender_id: Option<String>,
    pub role_ids: Vec<String>,
    pub slack_groups: Vec<String>,
    pub chat_type: Option<String>,
    pub dm_scope: Option<DmScope>,
    pub topic: Option<String>,
}

fn extract_agent_from_routing_id(routing_id: &str) -> Option<String> {
    let without_prefix = routing_id.strip_prefix("agent:")?;
    let head = without_prefix.split(':').next()?.trim();
    if head.is_empty() {
        None
    } else {
        Some(head.to_string())
    }
}

fn dedupe_values_case_insensitive(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(trimmed.to_string());
        }
    }
    out
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DmScopePolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<DmScope>,
    #[serde(default)]
    agents: HashMap<String, DmScope>,
    #[serde(default)]
    channels: HashMap<String, DmScope>,
}

impl DmScopePolicyConfig {
    fn normalize(&mut self) {
        self.agents = self
            .agents
            .drain()
            .map(|(key, value)| (key.trim().to_ascii_lowercase(), value))
            .collect();
        self.channels = self
            .channels
            .drain()
            .map(|(key, value)| (key.trim().to_ascii_lowercase(), value))
            .collect();
    }

    fn resolve(&self, agent_id: &str, channel: &str) -> Option<DmScope> {
        let channel = channel.trim().to_ascii_lowercase();
        if let Some(scope) = self.channels.get(&channel) {
            return Some(*scope);
        }
        if let Some((platform, _)) = channel.split_once(':')
            && let Some(scope) = self.channels.get(platform)
        {
            return Some(*scope);
        }
        let agent_id = agent_id.trim().to_ascii_lowercase();
        if let Some(scope) = self.agents.get(&agent_id) {
            return Some(*scope);
        }
        if let Some(scope) = self.channels.get("*") {
            return Some(*scope);
        }
        self.default
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AgentTonePolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    #[serde(default)]
    channels: HashMap<String, String>,
}

impl AgentTonePolicyConfig {
    fn normalize(&mut self) {
        self.default = normalize_tone_value(self.default.as_deref());
        self.channels = self
            .channels
            .drain()
            .filter_map(|(key, value)| {
                let key = key.trim().to_ascii_lowercase();
                normalize_tone_value(Some(value.as_str())).map(|tone| (key, tone))
            })
            .collect();
    }

    fn resolve(&self, channel: &str) -> Option<String> {
        if let Some(tone) = self.channels.get(channel) {
            return Some(tone.clone());
        }
        if let Some((platform, _)) = channel.split_once(':')
            && let Some(tone) = self.channels.get(platform)
        {
            return Some(tone.clone());
        }
        if let Some(tone) = self.channels.get("*") {
            return Some(tone.clone());
        }
        self.default.clone()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ChannelTonePolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    #[serde(default)]
    channels: HashMap<String, String>,
    #[serde(default)]
    agents: HashMap<String, AgentTonePolicyConfig>,
}

impl ChannelTonePolicyConfig {
    fn normalize(&mut self) {
        self.default = normalize_tone_value(self.default.as_deref());
        self.channels = self
            .channels
            .drain()
            .filter_map(|(key, value)| {
                let key = key.trim().to_ascii_lowercase();
                normalize_tone_value(Some(value.as_str())).map(|tone| (key, tone))
            })
            .collect();
        self.agents = self
            .agents
            .drain()
            .filter_map(|(key, mut value)| {
                value.normalize();
                let key = key.trim().to_ascii_lowercase();
                if key.is_empty() {
                    None
                } else {
                    Some((key, value))
                }
            })
            .collect();
    }

    fn resolve(&self, agent_id: &str, channel: &str) -> Option<String> {
        let channel = channel.trim().to_ascii_lowercase();
        let agent = agent_id.trim().to_ascii_lowercase();

        if let Some(agent_cfg) = self.agents.get(&agent)
            && let Some(tone) = agent_cfg.resolve(&channel)
        {
            return Some(tone);
        }
        if let Some(tone) = self.channels.get(&channel) {
            return Some(tone.clone());
        }
        if let Some((platform, _)) = channel.split_once(':')
            && let Some(tone) = self.channels.get(platform)
        {
            return Some(tone.clone());
        }
        if let Some(tone) = self.channels.get("*") {
            return Some(tone.clone());
        }
        self.default.clone()
    }
}

fn normalize_tone_value(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

async fn load_routing_rules(savfox_home: &Path) -> Vec<RoutingRule> {
    let path = savfox_home.join("routing-rules.json");
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Vec::new();
    }
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(v) => v,
        Err(err) => {
            warn!(
                "failed to read routing rules file {}: {err}",
                path.display()
            );
            return Vec::new();
        }
    };
    match serde_json::from_str::<Vec<RoutingRule>>(&content) {
        Ok(rules) => rules,
        Err(err) => {
            warn!("invalid routing rules file {}: {err}", path.display());
            Vec::new()
        }
    }
}

async fn resolve_parent_thread_agent(
    session_store: &Arc<SessionStore>,
    parent_thread_id: Option<&str>,
) -> Option<String> {
    const MAX_PARENT_DEPTH: usize = 8;
    let mut current = parent_thread_id?.to_string();
    let mut by_thread = HashMap::new();
    for entry in session_store.list().await {
        if let Some(thread_id) = entry.thread_id.clone() {
            by_thread.insert(thread_id, entry);
        }
    }

    for _ in 0..MAX_PARENT_DEPTH {
        let entry = by_thread.get(&current)?;
        if let Some(agent) = entry
            .routing_id
            .as_deref()
            .and_then(extract_agent_from_routing_id)
        {
            return Some(agent);
        }
        let Some(next) = entry.parent_thread_id.as_deref() else {
            return None;
        };
        if next == current {
            return None;
        }
        current = next.to_string();
    }
    None
}

async fn load_dm_scope_policy(savfox_home: &Path) -> DmScopePolicyConfig {
    let path = savfox_home.join("dm-scope.json");
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return DmScopePolicyConfig::default();
    }
    let Ok(content) = tokio::fs::read_to_string(&path).await else {
        return DmScopePolicyConfig::default();
    };
    let mut cfg = serde_json::from_str::<DmScopePolicyConfig>(&content).unwrap_or_default();
    cfg.normalize();
    cfg
}

async fn configured_dm_scope(savfox_home: &Path, agent_id: &str, channel: &str) -> DmScope {
    let policy = load_dm_scope_policy(savfox_home).await;
    if let Some(scope) = policy.resolve(agent_id, channel) {
        return scope;
    }
    let env_scope = std::env::var("SAVFOX_DM_SCOPE").ok();
    DmScope::from_str(env_scope.as_deref())
}

async fn load_channel_tone_policy(savfox_home: &Path) -> ChannelTonePolicyConfig {
    let path = savfox_home.join("channel-tone.json");
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return ChannelTonePolicyConfig::default();
    }
    let Ok(content) = tokio::fs::read_to_string(&path).await else {
        return ChannelTonePolicyConfig::default();
    };
    let mut cfg = serde_json::from_str::<ChannelTonePolicyConfig>(&content).unwrap_or_default();
    cfg.normalize();
    cfg
}

async fn configured_channel_tone_suffix(
    savfox_home: &Path,
    agent_id: &str,
    channel: &str,
) -> Option<String> {
    let policy = load_channel_tone_policy(savfox_home).await;
    policy.resolve(agent_id, channel)
}

fn append_channel_tone_suffix(prompt: &str, tone_suffix: Option<&str>) -> String {
    let Some(tone_suffix) = normalize_tone_value(tone_suffix) else {
        return prompt.to_string();
    };
    let trimmed_prompt = prompt.trim_end();
    if trimmed_prompt.is_empty() {
        format!("[system tone]\n{tone_suffix}")
    } else {
        format!(
            "{trimmed_prompt}\n\n[system tone]\nApply the following channel style guidance in your reply:\n{tone_suffix}"
        )
    }
}

async fn resolve_linked_identity(
    savfox_home: &Path,
    session_store: &Arc<SessionStore>,
    platform: &str,
    peer_id: Option<&str>,
    display_name: Option<&str>,
) -> Option<String> {
    let peer = platform_peer(platform, peer_id?)?;
    let mut links = load_identity_links(savfox_home).await;

    if let Some(identity) = canonical_for_peer(&links, &peer) {
        return Some(identity);
    }

    let display_name = display_name?.trim().to_ascii_lowercase();
    if display_name.is_empty() {
        return None;
    }

    let inferred = session_store.list().await.into_iter().find_map(|entry| {
        let identity = entry.identity?;
        let known_peers = peers_for_identity(&links, &identity);
        if known_peers.is_empty() {
            return None;
        }
        let existing_name = entry
            .sender
            .as_ref()
            .and_then(|s| s.display_name.as_ref())?
            .trim()
            .to_ascii_lowercase();
        if existing_name == display_name {
            Some(identity)
        } else {
            None
        }
    })?;

    if upsert_link(&mut links, &inferred, std::slice::from_ref(&peer)).is_some() {
        let _ = save_identity_links(savfox_home, &links).await;
    }
    Some(inferred)
}

async fn resolve_routed_agent(
    bridge: &GatewayBridge,
    session_store: &Arc<SessionStore>,
    platform: &str,
    channel: &str,
    display_name: Option<&str>,
    meta: &StartThreadMeta,
) -> String {
    let rules = load_routing_rules(&bridge.config().savfox_home).await;
    let router = AgentRouter::new(rules, "default".to_string());

    let mut role_values = Vec::new();
    role_values.extend(meta.role_ids.clone());
    role_values.extend(meta.slack_groups.clone());
    if platform.eq_ignore_ascii_case("discord") {
        role_values = AgentRouter::extract_discord_roles(&role_values);
    } else if platform.eq_ignore_ascii_case("slack") {
        role_values = AgentRouter::extract_slack_groups(&role_values);
    }
    role_values = dedupe_values_case_insensitive(role_values);

    let routing_ctx = RoutingContext {
        channel: platform.to_string(),
        channel_id: Some(channel.to_string()),
        sender_id: meta
            .peer_id
            .clone()
            .or_else(|| display_name.map(std::string::ToString::to_string)),
        group_id: meta.group_id.clone(),
        guild_id: meta.guild_id.clone(),
        team_id: meta.team_id.clone(),
        account_id: meta.account_id.clone(),
        role_ids: role_values,
        parent_channel_id: None,
        parent_sender_id: meta.parent_sender_id.clone(),
        is_dm: !matches!(meta.chat_type.as_deref(), Some("group" | "channel")),
    };
    let resolved = router.resolve(&routing_ctx);
    if resolved != "default" {
        return resolved;
    }

    if let Some(parent_agent) =
        resolve_parent_thread_agent(session_store, meta.parent_thread_id.as_deref()).await
    {
        return parent_agent;
    }

    resolved
}

pub(crate) async fn should_drop_duplicate(event_key: Option<String>) -> bool {
    let Some(key) = event_key else {
        return false;
    };
    let now = now_epoch_ms();
    let mut lock = dedupe_cache().lock().await;
    lock.retain(|_, ts| now.saturating_sub(*ts) <= DEDUPE_TTL_MS);
    if lock.contains_key(&key) {
        return true;
    }
    lock.insert(key, now);
    false
}

pub(crate) async fn spawn_start_thread_pipeline(
    bridge: Arc<GatewayBridge>,
    session_store: Arc<SessionStore>,
    platform: &'static str,
    channel: String,
    prompt: String,
    display_name: Option<String>,
) {
    spawn_start_thread_pipeline_with_meta(
        bridge,
        session_store,
        platform,
        channel,
        prompt,
        display_name,
        None,
    )
    .await;
}

pub(crate) async fn spawn_start_thread_pipeline_with_meta(
    bridge: Arc<GatewayBridge>,
    session_store: Arc<SessionStore>,
    platform: &'static str,
    channel: String,
    prompt: String,
    display_name: Option<String>,
    meta: Option<StartThreadMeta>,
) {
    let parsed = parse_directives(&prompt);
    let cleaned_prompt = if parsed.directives.is_empty() {
        prompt.trim().to_string()
    } else {
        parsed.cleaned_text.trim().to_string()
    };
    if cleaned_prompt.is_empty() {
        let outbound_channel = format!("{platform}:{channel}");
        let msg = "Savfox: message is empty after parsing directives.";
        let _ = send_with_retry(&bridge, &outbound_channel, msg, Some(1), None, None).await;
        return;
    }

    let outbound_channel = format!("{platform}:{channel}");
    record_channel_event(&outbound_channel).await;
    let start_meta = meta.unwrap_or_default();
    let linked_identity = resolve_linked_identity(
        &bridge.config().savfox_home,
        &session_store,
        platform,
        start_meta.peer_id.as_deref(),
        display_name.as_deref(),
    )
    .await;
    let routed_agent = resolve_routed_agent(
        &bridge,
        &session_store,
        platform,
        &channel,
        display_name.as_deref(),
        &start_meta,
    )
    .await;
    let dm_scope = if let Some(scope) = start_meta.dm_scope {
        scope
    } else {
        configured_dm_scope(
            &bridge.config().savfox_home,
            &routed_agent,
            &outbound_channel,
        )
        .await
    };

    let tracked = track_inbound_message(
        &session_store,
        InboundSessionMeta {
            agent_id: &routed_agent,
            platform,
            channel_id: &channel,
            peer_id: start_meta.peer_id.as_deref(),
            identity: linked_identity.as_deref(),
            group_id: start_meta.group_id.as_deref(),
            thread_id: start_meta.thread_id.as_deref(),
            parent_thread_id: start_meta.parent_thread_id.as_deref(),
            reply_target: start_meta.reply_target.as_deref(),
            account_id: start_meta.account_id.as_deref(),
            display_name: display_name.as_deref(),
            topic: start_meta.topic.as_deref(),
            first_message: Some(cleaned_prompt.as_str()),
            chat_type: start_meta.chat_type.as_deref(),
            dm_scope,
        },
    )
    .await;
    log_store::append_log(
        "info",
        "bridge/runtime",
        format!(
            "start_thread platform={platform} channel={channel} session_id={}",
            tracked.session_id
        ),
    )
    .await;

    if command_registry().has_command(&cleaned_prompt) {
        let mut metadata = HashMap::new();
        if let Some(model) = tracked
            .overrides
            .as_ref()
            .and_then(|o| o.model.as_ref())
            .or(tracked.model.as_ref())
        {
            metadata.insert("model".to_string(), model.clone());
        }
        metadata.insert("tokens_used".to_string(), tracked.total_tokens.to_string());

        let command_ctx = CommandContext {
            sender_id: start_meta
                .peer_id
                .clone()
                .or_else(|| display_name.clone())
                .unwrap_or_else(|| format!("{platform}:{channel}")),
            channel_id: outbound_channel.clone(),
            session_id: Some(tracked.session_id.clone()),
            is_authorized: true,
            is_mentioned: true,
            is_group: matches!(tracked.chat_type.as_deref(), Some("group" | "channel")),
            metadata,
        };

        if let Some(result) = command_registry().handle_command(&cleaned_prompt, &command_ctx) {
            if let Some(action) = result.action.as_ref() {
                apply_command_action(&session_store, &tracked.session_id, action).await;
            }

            let response = command_result_message(&result, "Command executed.");
            if let Err(err) = send_with_retry(
                &bridge,
                &outbound_channel,
                &response,
                None,
                tracked.thread_id.as_deref(),
                tracked.reply_target.as_deref(),
            )
            .await
            {
                warn!(
                    channel = %outbound_channel,
                    "bridge runtime: failed to send command reply: {err}"
                );
                log_store::append_log(
                    "warn",
                    "bridge/runtime",
                    format!("send command reply failed: channel={outbound_channel}, err={err}"),
                )
                .await;
            }
            return;
        }
    }

    let model_directive = parsed
        .directives
        .iter()
        .rev()
        .find(|d| d.kind == DirectiveKind::Model)
        .map(|d| d.value.clone());
    let (effective_model, model_profile) = if let Some(raw_model) = model_directive {
        let parsed_target = parse_model_target(&raw_model);
        let candidates: Vec<String> = bridge
            .session_manager()
            .list_models(bridge.config(), RefreshStrategy::Offline)
            .await
            .into_iter()
            .map(|m| m.id)
            .collect();
        let resolved = fuzzy_match_model_name(&parsed_target.model, &candidates)
            .unwrap_or(parsed_target.model);
        (resolved, parsed_target.profile)
    } else {
        (routed_agent.clone(), None)
    };
    let provider = effective_model
        .split('/')
        .next()
        .unwrap_or("unknown")
        .to_string();
    let tone_suffix = configured_channel_tone_suffix(
        &bridge.config().savfox_home,
        &routed_agent,
        &outbound_channel,
    )
    .await;
    let effective_prompt = append_channel_tone_suffix(&cleaned_prompt, tone_suffix.as_deref());
    if tone_suffix.is_some() {
        log_store::append_log(
            "info",
            "bridge/runtime",
            format!(
                "channel tone override applied: channel={outbound_channel}, agent={routed_agent}"
            ),
        )
        .await;
    }
    match bridge
        .invoke_agent_text_in_session_with_metadata(
            &effective_prompt,
            &effective_model,
            Some(&tracked.session_id),
        )
        .await
    {
        Ok(result) => {
            let mut input_tokens = 0_u64;
            let mut output_tokens = 0_u64;
            if let Some(tokens) = result.last_token_usage.as_ref() {
                input_tokens = tokens.input_tokens.max(0) as u64;
                output_tokens = tokens.output_tokens.max(0) as u64;
                track_token_usage(
                    &session_store,
                    &tracked.session_id,
                    input_tokens,
                    output_tokens,
                )
                .await;
            }

            maybe_auto_memory_flush(
                &session_store,
                &bridge.config().savfox_home,
                &tracked.session_id,
                tracked.total_tokens,
                input_tokens,
                output_tokens,
                &cleaned_prompt,
                &result.reply,
            )
            .await;

            let footer_config = current_response_footer_config();
            let reply = if let Some(footer) = format_model_footer(
                &footer_config,
                platform,
                &effective_model,
                &provider,
                model_profile.as_deref(),
                result.last_token_usage.as_ref(),
            ) {
                append_footer(&result.reply, &footer)
            } else {
                result.reply.clone()
            };
            if let Some(path) = result.rollout_path {
                let session_file = session_file_to_store_value(&bridge.config().savfox_home, &path);
                let thread_id = result.session_id;
                let session_id = tracked.session_id.clone();
                let _ = session_store
                    .update(&session_id, move |entry| {
                        entry.session_file = Some(session_file.clone());
                        if entry.thread_id.is_none() {
                            entry.thread_id = Some(thread_id.clone());
                        }
                    })
                    .await;
            }
            if let Err(err) = send_with_retry(
                &bridge,
                &outbound_channel,
                &reply,
                None,
                tracked.thread_id.as_deref(),
                tracked.reply_target.as_deref(),
            )
            .await
            {
                warn!(
                    channel = %outbound_channel,
                    "bridge runtime: failed to send agent reply after retries: {err}"
                );
                log_store::append_log(
                    "warn",
                    "bridge/runtime",
                    format!(
                        "send reply failed after retries: channel={outbound_channel}, err={err}"
                    ),
                )
                .await;
            } else {
                log_store::append_log(
                    "info",
                    "bridge/runtime",
                    format!(
                        "reply sent: channel={outbound_channel}, bytes={}",
                        reply.len()
                    ),
                )
                .await;
            }
        }
        Err(err) => {
            let fallback = format!("Savfox agent error: {err}");
            if let Err(send_err) = send_with_retry(
                &bridge,
                &outbound_channel,
                &fallback,
                None,
                tracked.thread_id.as_deref(),
                tracked.reply_target.as_deref(),
            )
            .await
            {
                warn!(
                    channel = %outbound_channel,
                    "bridge runtime: failed to send error reply: {send_err}"
                );
                log_store::append_log(
                    "warn",
                    "bridge/runtime",
                    format!("send fallback failed: channel={outbound_channel}, err={send_err}"),
                )
                .await;
            }
        }
    }
}

fn thread_tracking_key(
    channel: &str,
    thread_id: Option<&str>,
    reply_target: Option<&str>,
) -> Option<String> {
    let thread = thread_id
        .and_then(|v| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .or_else(|| {
            reply_target.and_then(|v| {
                let trimmed = v.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            })
        })?;
    Some(format!("{channel}::{thread}"))
}

async fn record_send_metrics(channel: &str, success: bool, latency_ms: u64) {
    let mut lock = send_metrics_store().lock().await;
    let platform = channel.split_once(':').map(|(p, _)| p).unwrap_or(channel);
    for key in [platform.to_string(), "*".to_string()] {
        let metric = lock.entry(key).or_default();
        metric.attempts = metric.attempts.saturating_add(1);
        if success {
            metric.success = metric.success.saturating_add(1);
            metric.total_latency_ms = metric.total_latency_ms.saturating_add(latency_ms);
        } else {
            metric.failed = metric.failed.saturating_add(1);
        }
    }
    drop(lock);

    let now = now_epoch_ms();
    let mut health = channel_health_store().lock().await;
    for key in [platform.to_string(), "*".to_string()] {
        health.entry(key).or_default().mark_send(success, now);
    }
}

async fn write_dead_letter(
    bridge: &GatewayBridge,
    policy_cfg: &SendPolicyConfig,
    channel: &str,
    text: &str,
    error: &str,
    thread_id: Option<&str>,
    reply_target: Option<&str>,
    attempts: usize,
) {
    let dir = policy_cfg.dead_letter_path(&bridge.config().savfox_home);
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return;
    }
    let file_name = format!("{}-{}.json", now_epoch_ms(), uuid::Uuid::now_v7());
    let payload = json!({
        "timestamp_ms": now_epoch_ms(),
        "channel": channel,
        "thread_id": thread_id,
        "reply_target": reply_target,
        "attempts": attempts,
        "error": error,
        "text": text,
    });
    if let Ok(serialized) = serde_json::to_vec_pretty(&payload) {
        let _ = tokio::fs::write(dir.join(file_name), serialized).await;
    }
}

async fn send_with_retry(
    bridge: &GatewayBridge,
    channel: &str,
    text: &str,
    attempt_override: Option<usize>,
    thread_id: Option<&str>,
    reply_target: Option<&str>,
) -> anyhow::Result<()> {
    let policy_cfg = SendPolicyConfig::load(&bridge.config().savfox_home).await;
    let policy = policy_cfg.resolve(channel);
    let max_attempts = attempt_override.unwrap_or(policy.retry_count).max(1);
    let timeout = Duration::from_millis(policy.timeout_ms.max(1));
    let base_backoff = policy.backoff_ms.max(1);
    let chunk_max_override = (policy.chunk_max_chars > 0).then_some(policy.chunk_max_chars);
    let chunk_texts: Vec<String> = if policy.auto_chunk {
        chunk_message_for_channel(
            text,
            channel,
            chunk_max_override,
            policy.chunk_overlap_chars,
        )
        .into_iter()
        .map(|chunk| chunk.text)
        .collect()
    } else {
        vec![text.to_string()]
    };

    let thread_key = thread_tracking_key(channel, thread_id, reply_target);
    let include_first_threading = match policy.threading {
        ThreadingPolicy::Off => false,
        ThreadingPolicy::All => true,
        ThreadingPolicy::First => {
            if let Some(key) = thread_key.as_ref() {
                !thread_once_cache().lock().await.contains(key)
            } else {
                false
            }
        }
    };
    for (chunk_index, chunk_text) in chunk_texts.iter().enumerate() {
        let include_threading = match policy.threading {
            ThreadingPolicy::Off => false,
            ThreadingPolicy::All => true,
            ThreadingPolicy::First => include_first_threading && chunk_index == 0,
        };
        let scoped_thread_id = if include_threading { thread_id } else { None };
        let scoped_reply_target = if include_threading {
            reply_target
        } else {
            None
        };

        for attempt in 1..=max_attempts {
            let started = Instant::now();
            let send_result = tokio::time::timeout(
                timeout,
                bridge.send_platform_message_with_context(
                    channel,
                    chunk_text,
                    None,
                    None,
                    None,
                    scoped_thread_id,
                    scoped_reply_target,
                ),
            )
            .await;

            match send_result {
                Ok(Ok(())) => {
                    let latency_ms = started.elapsed().as_millis() as u64;
                    record_send_metrics(channel, true, latency_ms).await;
                    if policy.threading == ThreadingPolicy::First
                        && include_threading
                        && let Some(key) = thread_key.as_ref()
                    {
                        thread_once_cache().lock().await.insert(key.clone());
                    }
                    break;
                }
                Ok(Err(err)) => {
                    let latency_ms = started.elapsed().as_millis() as u64;
                    record_send_metrics(channel, false, latency_ms).await;
                    if attempt >= max_attempts {
                        let chunk_error = if chunk_texts.len() > 1 {
                            format!(
                                "chunk {}/{} failed: {err}",
                                chunk_index + 1,
                                chunk_texts.len()
                            )
                        } else {
                            err.to_string()
                        };
                        write_dead_letter(
                            bridge,
                            &policy_cfg,
                            channel,
                            chunk_text,
                            &chunk_error,
                            scoped_thread_id,
                            scoped_reply_target,
                            attempt,
                        )
                        .await;
                        return Err(err);
                    }
                }
                Err(_) => {
                    let latency_ms = started.elapsed().as_millis() as u64;
                    record_send_metrics(channel, false, latency_ms).await;
                    if attempt >= max_attempts {
                        let err = anyhow::anyhow!("send timeout after {}ms", policy.timeout_ms);
                        let chunk_error = if chunk_texts.len() > 1 {
                            format!(
                                "chunk {}/{} timeout: {err}",
                                chunk_index + 1,
                                chunk_texts.len()
                            )
                        } else {
                            err.to_string()
                        };
                        write_dead_letter(
                            bridge,
                            &policy_cfg,
                            channel,
                            chunk_text,
                            &chunk_error,
                            scoped_thread_id,
                            scoped_reply_target,
                            attempt,
                        )
                        .await;
                        return Err(err);
                    }
                }
            }

            let backoff_ms =
                base_backoff.saturating_mul(1u64 << (attempt.saturating_sub(1) as u32));
            tokio::time::sleep(Duration::from_millis(backoff_ms.min(5_000))).await;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        AgentTonePolicyConfig, ChannelTonePolicyConfig, append_channel_tone_suffix,
        format_model_footer,
    };
    use crate::config::ResponseFooterConfig;
    use savfox_protocol::protocol::TokenUsage;

    #[test]
    fn footer_respects_global_disable() {
        let cfg = ResponseFooterConfig {
            enabled: false,
            ..ResponseFooterConfig::default()
        };
        let footer = format_model_footer(&cfg, "discord", "openai/gpt-4o", "openai", None, None);
        assert!(footer.is_none());
    }

    #[test]
    fn footer_uses_channel_template_and_max_length() {
        let mut cfg = ResponseFooterConfig::default();
        cfg.channel_templates
            .insert("telegram".to_string(), "m:{model} t:{tokens}".to_string());
        cfg.channel_max_length.insert("telegram".to_string(), 14);

        let usage = TokenUsage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 12345,
        };

        let footer = format_model_footer(
            &cfg,
            "telegram",
            "openai/gpt-4o-mini",
            "openai",
            None,
            Some(&usage),
        )
        .expect("footer should render");

        assert!(footer.chars().count() <= 14);
    }

    #[test]
    fn channel_tone_resolve_prefers_agent_channel_matrix() {
        let mut cfg = ChannelTonePolicyConfig {
            default: Some("default style".to_string()),
            channels: HashMap::from([("slack".to_string(), "formal".to_string())]),
            agents: HashMap::from([(
                "support".to_string(),
                AgentTonePolicyConfig {
                    default: Some("friendly".to_string()),
                    channels: HashMap::from([
                        ("discord".to_string(), "emoji".to_string()),
                        ("discord:123".to_string(), "very formal".to_string()),
                    ]),
                },
            )]),
        };
        cfg.normalize();

        assert_eq!(
            cfg.resolve("support", "discord:123"),
            Some("very formal".to_string())
        );
        assert_eq!(
            cfg.resolve("support", "discord:999"),
            Some("emoji".to_string())
        );
        assert_eq!(
            cfg.resolve("support", "telegram:1"),
            Some("friendly".to_string())
        );
        assert_eq!(
            cfg.resolve("other", "slack:C01"),
            Some("formal".to_string())
        );
        assert_eq!(
            cfg.resolve("other", "telegram:777"),
            Some("default style".to_string())
        );
    }

    #[test]
    fn append_channel_tone_suffix_is_noop_when_missing() {
        let prompt = "summarize this";
        assert_eq!(append_channel_tone_suffix(prompt, None), prompt);
    }

    #[test]
    fn append_channel_tone_suffix_injects_instruction() {
        let prompt = "summarize this";
        let out = append_channel_tone_suffix(prompt, Some("Be more formal on Slack."));
        assert!(out.contains(prompt));
        assert!(out.contains("[system tone]"));
        assert!(out.contains("Be more formal on Slack."));
    }
}
