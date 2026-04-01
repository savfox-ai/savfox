use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use savfox_core::config::channel_store::{ChannelConfig, Router};
use tracing::warn;

use crate::agent_routing::{AgentRouter, RoutingContext};
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
        return forced_agent_id.to_string();
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
        channel: platform.to_string(),
        channel_id: Some(channel_id.to_string()),
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
                    return agent_id.to_string();
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

    "default".to_string()
}
