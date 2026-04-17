use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use savfox_core::config::channel_store::{ChannelConfig, Router};
use serde_json::{Value, json};
use tracing::warn;

use super::trigger::{
    AgentTriggerConfig, ExternalBotPolicy, IdleReplyConfig, IngestPolicy, SenderKind,
};
use crate::agent_routing::{AgentRouter, RoutingContext};
use crate::auto_reply::GroupActivation;
use crate::channel::GatewayChannel;
use crate::identity_links::{
    canonical_for_peer, load_identity_links, peers_for_identity, platform_peer,
    save_identity_links, upsert_link,
};
use crate::session::{DmScope, SessionStore};

#[derive(Debug, Clone, Default)]
pub(crate) struct StartThreadMeta {
    pub peer_id: Option<String>,
    pub forced_agent_id: Option<String>,
    pub routing_group_id: Option<String>,
    pub routing_thread_id: Option<String>,
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
    pub saved_channel_config_id: Option<String>,
    pub sender_kind: SenderKind,
    pub is_mentioned: bool,
    pub reply_to_self: bool,
    pub is_command: bool,
    pub used_plain_text_fallback: bool,
    pub participant_count: Option<u32>,
    pub explicitly_targets_other_agent: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct TextTargetMatch {
    pub agent_id: Option<String>,
    pub stripped_prompt: Option<String>,
}

fn extract_agent_from_routing_id(routing_id: &str) -> Option<String> {
    let without_prefix = routing_id.strip_prefix("agent:")?;
    let head = without_prefix.split(':').next()?.trim();
    if head.is_empty() {
        None
    } else {
        Some(head.to_owned())
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
            out.push(trimmed.to_owned());
        }
    }
    out
}

async fn resolve_parent_thread_agent(
    session_store: &Arc<SessionStore>,
    parent_thread_id: Option<&str>,
) -> Option<String> {
    const MAX_PARENT_DEPTH: usize = 8;
    let mut current = parent_thread_id?.to_owned();
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
        current = next.to_owned();
    }
    None
}

async fn read_json_value(path: &Path) -> Option<Value> {
    let data = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str(&data).ok()
}

fn agents_dir(savfox_home: &Path) -> PathBuf {
    savfox_home.join("agents")
}

fn sanitize_agent_file_stem(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        let mapped = match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            _ => ch,
        };
        out.push(mapped);
    }

    let out = out.trim_matches([' ', '.']).to_owned();
    if out.is_empty() || out == "." || out == ".." {
        None
    } else {
        Some(out)
    }
}

fn normalized_agent_name_key(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

fn default_agent_stub() -> Value {
    json!({
        "id": "default",
        "name": "Savvy fox",
        "builtin": true,
        "status": "active",
    })
}

fn normalize_alias(raw: &str) -> Option<String> {
    let alias = raw.trim().trim_start_matches('@').trim();
    if alias.is_empty() {
        None
    } else {
        Some(alias.to_ascii_lowercase())
    }
}

fn collect_agent_aliases(config: &Value, fallback_id: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    for value in [
        Some(fallback_id.to_owned()),
        config.get("id").and_then(Value::as_str).map(str::to_string),
        config
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        config
            .get("identity")
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(alias) = normalize_alias(&value) {
            aliases.push(alias);
        }
    }

    if let Some(extra_aliases) = config.get("agent_aliases").and_then(Value::as_array) {
        for value in extra_aliases.iter().filter_map(Value::as_str) {
            if let Some(alias) = normalize_alias(value) {
                aliases.push(alias);
            }
        }
    }

    dedupe_values_case_insensitive(aliases)
}

async fn load_all_agent_configs(savfox_home: &Path) -> Vec<(String, Value)> {
    let dir = agents_dir(savfox_home);
    let mut configs = Vec::new();

    let default_path = dir.join("default.json");
    if default_path.exists() {
        if let Some(config) = read_json_value(&default_path).await {
            configs.push(("default".to_owned(), config));
        }
    } else {
        configs.push(("default".to_owned(), default_agent_stub()));
    }

    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return configs;
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if stem.eq_ignore_ascii_case("default") {
            continue;
        }
        if let Some(config) = read_json_value(&path).await {
            configs.push((stem.to_owned(), config));
        }
    }

    configs
}

async fn resolve_agent_config_value(savfox_home: &Path, agent_ref: &str) -> Option<Value> {
    let dir = agents_dir(savfox_home);
    let trimmed = agent_ref.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(safe_ref) = sanitize_agent_file_stem(trimmed) {
        let direct = dir.join(format!("{safe_ref}.json"));
        if direct.exists() {
            return read_json_value(&direct).await;
        }
    }

    for (stem, config) in load_all_agent_configs(savfox_home).await {
        if stem.eq_ignore_ascii_case(trimmed) {
            return Some(config);
        }

        let id_match = config
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|value| value.eq_ignore_ascii_case(trimmed));
        if id_match {
            return Some(config);
        }

        let name_match = config
            .get("name")
            .and_then(Value::as_str)
            .and_then(normalized_agent_name_key)
            .as_deref()
            == normalized_agent_name_key(trimmed).as_deref();
        if name_match {
            return Some(config);
        }
    }

    None
}

pub(super) fn parse_group_activation_str(value: Option<&str>) -> GroupActivation {
    match value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("keyword") => GroupActivation::Keyword,
        Some("always") => GroupActivation::Always,
        Some("command") => GroupActivation::Command,
        Some("off") => GroupActivation::Off,
        _ => GroupActivation::Mention,
    }
}

fn parse_group_activation(value: Option<&Value>) -> GroupActivation {
    parse_group_activation_str(value.and_then(Value::as_str))
}

fn parse_idle_reply_config(value: Option<&Value>) -> IdleReplyConfig {
    let Some(config) = value.and_then(Value::as_object) else {
        return IdleReplyConfig::default();
    };

    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let delay_secs = config
        .get("delay_secs")
        .and_then(Value::as_u64)
        .unwrap_or(180)
        .max(30);
    let max_per_hour = config
        .get("max_per_hour")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, u32::MAX as u64) as u32;
    let prompt = config
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    IdleReplyConfig {
        enabled,
        delay_secs,
        max_per_hour,
        prompt,
    }
}

pub(super) async fn load_agent_trigger_config(
    savfox_home: &Path,
    agent_ref: &str,
) -> AgentTriggerConfig {
    let Some(config) = resolve_agent_config_value(savfox_home, agent_ref).await else {
        return AgentTriggerConfig::default();
    };

    let fallback_id = config
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(agent_ref);
    let group_keywords = config
        .get("group_keywords")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    AgentTriggerConfig {
        group_activation: parse_group_activation(config.get("group_activation")),
        group_keywords,
        ingest_policy: IngestPolicy::from_str(config.get("ingest_policy").and_then(Value::as_str)),
        external_bot_policy: ExternalBotPolicy::from_str(
            config.get("external_bot_policy").and_then(Value::as_str),
        ),
        agent_aliases: collect_agent_aliases(&config, fallback_id),
        idle_reply: parse_idle_reply_config(config.get("idle_reply")),
    }
}

fn split_leading_alias<'a>(text: &'a str, alias: &str) -> Option<&'a str> {
    let trimmed = text.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let alias_lower = alias.trim().to_ascii_lowercase();
    if alias_lower.is_empty() {
        return None;
    }

    for candidate in [format!("@{alias_lower}"), alias_lower.clone()] {
        let Some(rest) = lower.strip_prefix(&candidate) else {
            continue;
        };
        let separator = rest.chars().next();
        if rest.is_empty()
            || separator
                .is_some_and(|ch| matches!(ch, ':' | ',' | ' ' | '\t' | '，' | '：' | '\n' | '\r'))
        {
            let original_rest = &trimmed[candidate.len()..];
            return Some(
                original_rest
                    .trim_start_matches(|ch: char| {
                        matches!(ch, ':' | ',' | ' ' | '\t' | '，' | '：')
                    })
                    .trim(),
            );
        }
    }

    None
}

pub(super) async fn resolve_text_target_match(savfox_home: &Path, text: &str) -> TextTargetMatch {
    let mut best_match: Option<(usize, TextTargetMatch)> = None;

    for (stem, config) in load_all_agent_configs(savfox_home).await {
        let aliases = collect_agent_aliases(&config, &stem);
        for alias in aliases {
            let Some(stripped_prompt) = split_leading_alias(text, &alias) else {
                continue;
            };
            let rank = alias.len();
            let candidate = TextTargetMatch {
                agent_id: Some(
                    config
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or(&stem)
                        .to_owned(),
                ),
                stripped_prompt: if stripped_prompt.is_empty() {
                    None
                } else {
                    Some(stripped_prompt.to_owned())
                },
            };
            if best_match
                .as_ref()
                .is_none_or(|(best_rank, _)| rank > *best_rank)
            {
                best_match = Some((rank, candidate));
            }
        }
    }

    best_match.map(|(_, matched)| matched).unwrap_or_default()
}

pub(super) async fn resolve_linked_identity(
    savfox_home: &Path,
    session_store: &Arc<SessionStore>,
    platform: &str,
    peer_id: Option<&str>,
    name: Option<&str>,
) -> Option<String> {
    let peer = platform_peer(platform, peer_id?)?;
    let mut links = load_identity_links(savfox_home).await;

    if let Some(identity) = canonical_for_peer(&links, &peer) {
        return Some(identity);
    }

    let name = name?.trim().to_ascii_lowercase();
    if name.is_empty() {
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
            .and_then(|s| s.name.as_ref())?
            .trim()
            .to_ascii_lowercase();
        if existing_name == name {
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

fn channel_kind_matches(config_kind: &str, platform: &str) -> bool {
    let config_kind = config_kind.trim().to_ascii_lowercase();
    let platform = platform.trim().to_ascii_lowercase();
    config_kind == platform
        || (config_kind == "feishu" && platform == "lark")
        || (config_kind == "lark" && platform == "feishu")
}

async fn load_saved_channel_config(
    savfox_home: &std::path::Path,
    platform: &str,
    channel_id: &str,
    saved_channel_config_id: Option<&str>,
) -> Option<ChannelConfig> {
    let savfox_home = savfox_home.to_path_buf();

    if let Some(config_id) = saved_channel_config_id
        && let Ok(Some(config)) =
            savfox_core::config::channel_store::get_channel_config(&savfox_home, config_id).await
        && config.enabled
        && channel_kind_matches(&config.kind, platform)
    {
        return Some(config);
    }

    if platform.eq_ignore_ascii_case("matrix")
        && let Ok(Some(matrix_config)) =
            savfox_channels::matrix::resolve_matrix_outbound_config(&savfox_home, channel_id).await
        && let Ok(Some(config)) =
            savfox_core::config::channel_store::get_channel_config(&savfox_home, &matrix_config.id)
                .await
        && config.enabled
    {
        return Some(config);
    }

    match savfox_core::config::channel_store::get_channel_config(&savfox_home, platform).await {
        Ok(Some(config)) if config.enabled => Some(config),
        Ok(_) => None,
        Err(err) => {
            warn!(
                platform,
                channel_id,
                error = %err,
                "failed to resolve saved channel config for routed agent"
            );
            None
        }
    }
}

/// Result of checking access policies for an incoming message.
pub(super) enum PolicyDecision {
    /// Message is allowed.
    Allow,
    /// Message is blocked. Contains a human-readable reason.
    Block(String),
}

/// Check the DM/group access policies for the incoming message.
pub(super) async fn check_channel_policies(
    savfox_home: &std::path::Path,
    platform: &str,
    channel_id: &str,
    peer_id: Option<&str>,
    chat_type: Option<&str>,
    group_id: Option<&str>,
    saved_channel_config_id: Option<&str>,
) -> PolicyDecision {
    let config =
        match load_saved_channel_config(savfox_home, platform, channel_id, saved_channel_config_id)
            .await
        {
            Some(config) => config,
            None => return PolicyDecision::Allow,
        };

    let is_group = matches!(chat_type, Some("group" | "supergroup" | "channel"));

    if is_group {
        if let Some(policy) = &config.group_policy {
            let check_id = group_id.unwrap_or(channel_id);
            if !policy.is_allowed(check_id) {
                return PolicyDecision::Block(format!(
                    "group_policy denied group/channel '{check_id}'"
                ));
            }
        }
    } else {
        if let Some(policy) = &config.dm_policy {
            let check_id = peer_id.unwrap_or(channel_id);
            if !policy.is_allowed(check_id) {
                return PolicyDecision::Block(format!("dm_policy denied peer '{check_id}'"));
            }
        }
    }

    PolicyDecision::Allow
}

pub(super) async fn resolve_routed_agent(
    gateway_channel: &GatewayChannel,
    session_store: &Arc<SessionStore>,
    platform: &str,
    channel_id: &str,
    name: Option<&str>,
    meta: &StartThreadMeta,
) -> String {
    if let Some(forced_agent_id) = meta
        .forced_agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return forced_agent_id.to_owned();
    }

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
        channel: platform.to_owned(),
        channel_id: Some(channel_id.to_owned()),
        sender_id: meta
            .peer_id
            .clone()
            .or_else(|| name.map(std::string::ToString::to_string)),
        group_id: meta.group_id.clone(),
        guild_id: meta.guild_id.clone(),
        team_id: meta.team_id.clone(),
        account_id: meta.account_id.clone(),
        role_ids: role_values,
        parent_channel_id: None,
        parent_sender_id: meta.parent_sender_id.clone(),
        is_dm: !matches!(meta.chat_type.as_deref(), Some("group" | "channel")),
    };

    if let Some(config) = load_saved_channel_config(
        &gateway_channel.config().savfox_home,
        platform,
        channel_id,
        meta.saved_channel_config_id.as_deref(),
    )
    .await
        && let Some(router) = config.router
    {
        match router {
            Router::AgentId { agent_id } => {
                let agent_id = agent_id.trim();
                if !agent_id.is_empty() {
                    return agent_id.to_owned();
                }
            }
            Router::RouteRules {
                default_agent_id,
                rules,
            } => {
                let resolved =
                    AgentRouter::new(rules, default_agent_id).resolve_with_details(&routing_ctx);
                if resolved.matched_rule {
                    return resolved.agent_id;
                }
                if let Some(parent_agent) =
                    resolve_parent_thread_agent(session_store, meta.parent_thread_id.as_deref())
                        .await
                {
                    return parent_agent;
                }
                return resolved.agent_id;
            }
        }
    }

    if let Some(parent_agent) =
        resolve_parent_thread_agent(session_store, meta.parent_thread_id.as_deref()).await
    {
        return parent_agent;
    }

    "default".to_owned()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        AgentTriggerConfig, GroupActivation, load_agent_trigger_config, parse_group_activation_str,
        resolve_text_target_match,
    };

    #[tokio::test]
    async fn text_target_match_uses_agent_aliases() {
        let home = tempdir().expect("tempdir");
        let agents_dir = home.path().join("agents");
        tokio::fs::create_dir_all(&agents_dir).await.expect("mkdir");
        tokio::fs::write(
            agents_dir.join("reviewer.json"),
            r#"{
  "id": "reviewer",
  "name": "Code Reviewer",
  "agent_aliases": ["reviewer", "code-reviewer"]
}"#,
        )
        .await
        .expect("write config");

        let matched = resolve_text_target_match(home.path(), "reviewer: inspect this diff").await;
        assert_eq!(matched.agent_id.as_deref(), Some("reviewer"));
        assert_eq!(
            matched.stripped_prompt.as_deref(),
            Some("inspect this diff")
        );
    }

    #[tokio::test]
    async fn load_agent_trigger_config_reads_runtime_policy_fields() {
        let home = tempdir().expect("tempdir");
        let agents_dir = home.path().join("agents");
        tokio::fs::create_dir_all(&agents_dir).await.expect("mkdir");
        tokio::fs::write(
            agents_dir.join("reviewer.json"),
            r#"{
  "id": "reviewer",
  "group_activation": "keyword",
  "group_keywords": ["review", "diff"],
  "agent_aliases": ["reviewer"],
  "ingest_policy": "targeted_only",
  "external_bot_policy": "ingest_only",
  "idle_reply": {
    "enabled": true,
    "delay_secs": 240,
    "max_per_hour": 2,
    "prompt": "Follow up if the room stays quiet."
  }
}"#,
        )
        .await
        .expect("write config");

        let config = load_agent_trigger_config(home.path(), "reviewer").await;
        assert_eq!(config.group_activation, GroupActivation::Keyword);
        assert_eq!(
            config.group_keywords,
            vec!["review".to_owned(), "diff".to_owned()]
        );
        assert_eq!(config.agent_aliases, vec!["reviewer".to_owned()]);
        assert!(matches!(
            config.ingest_policy,
            super::IngestPolicy::TargetedOnly
        ));
        assert!(matches!(
            config.external_bot_policy,
            super::ExternalBotPolicy::IngestOnly
        ));
        assert!(config.idle_reply.enabled);
        assert_eq!(config.idle_reply.delay_secs, 240);
        assert_eq!(config.idle_reply.max_per_hour, 2);
        assert_eq!(
            config.idle_reply.prompt.as_deref(),
            Some("Follow up if the room stays quiet.")
        );
    }

    #[test]
    fn parse_group_activation_str_understands_session_override_values() {
        assert_eq!(
            parse_group_activation_str(Some("keyword")),
            GroupActivation::Keyword
        );
        assert_eq!(
            parse_group_activation_str(Some("off")),
            GroupActivation::Off
        );
        assert_eq!(
            parse_group_activation_str(Some("invalid-value")),
            GroupActivation::Mention
        );
    }

    #[test]
    fn default_agent_trigger_config_is_safe() {
        let config = AgentTriggerConfig::default();
        assert_eq!(config.group_activation, GroupActivation::Mention);
        assert!(config.group_keywords.is_empty());
    }
}
