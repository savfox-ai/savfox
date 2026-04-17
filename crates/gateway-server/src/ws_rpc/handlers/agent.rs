#![allow(unused_imports)]

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::types::{INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, RpcResult};
use super::channel::{channel_is_configured, load_saved_channel_configs};
use super::channel_management::load_nostr_profile;
use crate::channel::GatewayChannel;

// ── Agent (single-agent operations) ─────────────────────────────────────────

pub(crate) async fn handle_agent(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let agent = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    if message.is_empty() {
        return Err((INVALID_REQUEST, "missing 'message' parameter".to_owned()));
    }

    match channel.invoke_agent_text(message, agent).await {
        Ok(reply) => Ok(json!({ "response": reply })),
        Err(err) => Err((INTERNAL_ERROR, format!("agent error: {err}"))),
    }
}

pub(crate) async fn handle_agent_identity() -> RpcResult {
    Ok(json!({
        "name": "savfox",
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": ["chat", "tools", "sessions", "cron", "nodes", "tts", "a2a", "delegation"],
    }))
}

pub(crate) async fn handle_agent_wait(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let agent = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    if message.is_empty() {
        return Err((INVALID_REQUEST, "missing 'message' parameter".to_owned()));
    }

    match channel.invoke_agent_text(message, agent).await {
        Ok(reply) => Ok(json!({ "response": reply, "done": true })),
        Err(err) => Err((INTERNAL_ERROR, format!("agent.wait error: {err}"))),
    }
}

// ── Agent capabilities & delegation ─────────────────────────────────────────

/// Returns the capabilities of a specific agent, including its tools,
/// skills, connected channels, and current status.
pub(crate) async fn handle_agent_capabilities(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent_id = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    // Collect tools from agent config (if it exists).
    let agents_dir = channel.config().savfox_home.join("agents");
    let agent_config_path = agents_dir.join(format!("{agent_id}.json"));
    let agent_config: Option<Value> = if agent_config_path.exists() {
        tokio::fs::read_to_string(&agent_config_path)
            .await
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
    } else {
        None
    };

    let agent_name = agent_config
        .as_ref()
        .and_then(|c| c.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(agent_id);

    // Tools: extract from config or list known built-in tools.
    let tools: Vec<String> = agent_config
        .as_ref()
        .and_then(|c| c.get("tools"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| {
            // Default agent has access to standard tool set.
            vec![
                "shell".to_owned(),
                "read_file".to_owned(),
                "write_file".to_owned(),
                "list_dir".to_owned(),
                "grep_files".to_owned(),
                "web_search".to_owned(),
                "web_fetch".to_owned(),
                "sessions_send_a2a".to_owned(),
                "agent_step".to_owned(),
            ]
        });

    // Skills: read from agent config.
    let skills: Vec<String> = agent_config
        .as_ref()
        .and_then(|c| c.get("skills"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Channels: derive from configured channel secrets.
    let channels: Vec<String> = {
        let runtime = channel.runtime_channel_secrets().await;
        let saved_configs = load_saved_channel_configs(channel).await;
        let nostr_profile = load_nostr_profile(channel).await;
        let nostr_configured = nostr_profile
            .get("private_key")
            .and_then(|v| v.as_str())
            .is_some_and(|v| !v.trim().is_empty());
        let mut ch = Vec::new();
        for platform in [
            "discord",
            "telegram",
            "slack",
            "webhook",
            "matrix",
            "dingtalk",
            "feishu",
            "mattermost",
            "googlechat",
            "line",
            "irc",
            "signal",
            "whatsapp",
            "nostr",
        ] {
            if channel_is_configured(platform, &runtime, &saved_configs, nostr_configured) {
                ch.push(platform.to_owned());
            }
        }
        ch.sort();
        ch.dedup();
        ch
    };

    // Status: check if the agent is active via session manager.
    let active_sessions = channel.websocket_manager().session_ids().await;
    let status = if active_sessions
        .iter()
        .any(|s| s.to_string().contains(agent_id))
    {
        "active"
    } else if agent_id == "default" {
        "active"
    } else {
        "idle"
    };
    let delegation_chain = savfox_core::a2a::delegation_chain_for(agent_id).await;
    let delegation_chain_json: Vec<Value> = delegation_chain
        .iter()
        .map(|entry| {
            json!({
                "parent_agent": entry.parent_agent,
                "child_agent": entry.child_agent,
                "spawned_at": entry.spawned_at,
                "purpose": entry.purpose,
            })
        })
        .collect();
    let delegation_parent = delegation_chain
        .last()
        .map(|entry| entry.parent_agent.clone());

    Ok(json!({
        "agent": agent_name,
        "agent_id": agent_id,
        "tools": tools,
        "skills": skills,
        "channels": channels,
        "status": status,
        "delegation_parent": delegation_parent,
        "delegation_depth": delegation_chain_json.len(),
        "delegation_chain": delegation_chain_json,
    }))
}

/// List all recorded delegation entries.
pub(crate) async fn handle_agent_delegation_list() -> RpcResult {
    let entries = savfox_core::a2a::list_delegations().await;
    let entries_json: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "parent_agent": e.parent_agent,
                "child_agent": e.child_agent,
                "spawned_at": e.spawned_at,
                "purpose": e.purpose,
            })
        })
        .collect();

    Ok(json!({
        "delegations": entries_json,
        "count": entries_json.len(),
    }))
}

/// Get the delegation chain for a specific agent, walking up parent links.
pub(crate) async fn handle_agent_delegation_chain(params: &Value) -> RpcResult {
    let agent_id = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if agent_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'agent' parameter".to_owned()));
    }

    let chain = savfox_core::a2a::delegation_chain_for(agent_id).await;
    let chain_json: Vec<Value> = chain
        .iter()
        .map(|e| {
            json!({
                "parent_agent": e.parent_agent,
                "child_agent": e.child_agent,
                "spawned_at": e.spawned_at,
                "purpose": e.purpose,
            })
        })
        .collect();

    Ok(json!({
        "agent": agent_id,
        "chain": chain_json,
        "depth": chain_json.len(),
    }))
}

/// Record a new delegation entry between a parent and child agent.
pub(crate) async fn handle_agent_delegation_record(params: &Value) -> RpcResult {
    let parent = params
        .get("parent_agent")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let child = params
        .get("child_agent")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let purpose = params
        .get("purpose")
        .and_then(|v| v.as_str())
        .unwrap_or("manual delegation");

    if parent.is_empty() || child.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'parent_agent' or 'child_agent' parameter".to_owned(),
        ));
    }

    let spawned_at = savfox_core::a2a::now_ms();

    savfox_core::a2a::record_delegation(savfox_core::a2a::DelegationEntry {
        parent_agent: parent.to_owned(),
        child_agent: child.to_owned(),
        spawned_at,
        purpose: purpose.to_owned(),
    })
    .await;

    Ok(json!({
        "status": "recorded",
        "parent_agent": parent,
        "child_agent": child,
        "spawned_at": spawned_at,
        "purpose": purpose,
    }))
}

/// Remove a delegation entry by child agent ID.
pub(crate) async fn handle_agent_delegation_remove(params: &Value) -> RpcResult {
    let child = params
        .get("child_agent")
        .or_else(|| params.get("agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if child.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'child_agent' parameter".to_owned(),
        ));
    }

    let removed = savfox_core::a2a::remove_delegation(child).await;

    Ok(json!({
        "status": if removed { "removed" } else { "not_found" },
        "child_agent": child,
    }))
}

// ── Agents (multi-agent CRUD) ───────────────────────────────────────────────

/// Get the agents directory (SAVFOX_HOME/agents/).
pub(crate) fn agents_dir(channel: &GatewayChannel) -> std::path::PathBuf {
    channel.config().savfox_home.join("agents")
}

/// Read an agent config JSON file.
async fn read_agent_config(path: &std::path::Path) -> Option<Value> {
    let data = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str(&data).ok()
}

async fn write_agent_config(path: &std::path::Path, config: &Value) -> Result<(), String> {
    let data = serde_json::to_string_pretty(config).unwrap_or_default();
    tokio::fs::write(path, data)
        .await
        .map_err(|err| format!("failed to write agent config: {err}"))
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

fn default_agent_name_from_config(config: &Value, fallback: &str) -> String {
    config
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| {
            config
                .get("identity")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| fallback.to_owned())
}

fn default_agent_stub() -> Value {
    json!({
        "id": "default",
        "name": "Savvy fox",
        "description": "Default Savfox assistant agent",
        "builtin": true,
        "status": "active",
    })
}

pub(crate) fn normalized_agent_name_key(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

fn normalize_agent_model_fields(config: &mut Value) {
    let primary = config
        .get("models")
        .and_then(|models| models.get("primary"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    if let Some(primary) = primary {
        let model_missing = config
            .get("model")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .is_none_or(|value| value.is_empty());
        if model_missing {
            config["model"] = json!(primary);
        }
    }

    let fallback_missing = config.get("fallback_models").is_none();
    if !fallback_missing {
        return;
    }

    let Some(fallbacks) = config
        .get("models")
        .and_then(|models| models.get("fallbacks"))
        .and_then(|value| value.as_array())
    else {
        return;
    };

    let normalized: Vec<String> = fallbacks
        .iter()
        .filter_map(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();
    if !normalized.is_empty() {
        config["fallback_models"] = json!(normalized);
    }
}

pub(crate) fn normalize_agent_config(config: &mut Value, fallback_id: &str, builtin: bool) {
    let id_missing = config
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .is_none_or(|value| value.is_empty());
    if id_missing {
        config["id"] = json!(fallback_id);
    }

    let name_missing = config
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .is_none_or(|value| value.is_empty());
    if name_missing {
        let default_name = default_agent_name_from_config(config, fallback_id);
        let default_name = if builtin
            && fallback_id.eq_ignore_ascii_case("default")
            && default_name == fallback_id
        {
            "Savvy fox".to_owned()
        } else {
            default_name
        };
        config["name"] = json!(default_name);
    }

    normalize_agent_model_fields(config);

    if !builtin {
        return;
    }

    config["builtin"] = json!(true);
    if config
        .get("description")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .is_none_or(|value| value.is_empty())
    {
        config["description"] = json!("Default Savfox assistant agent");
    }
    if config.get("status").is_none() {
        config["status"] = json!("active");
    }
}

/// Load the agent's JSON config and apply its `permission_policy` to the
/// session `Config` (sandbox, approval, and tool access).
pub(crate) async fn apply_agent_permission_policy_to_config(
    config: &mut savfox_core::config::Config,
    channel: &GatewayChannel,
    agent_ref: &str,
) {
    use savfox_protocol::protocol::ToolAccessPolicy;

    // Resolve the agent config file.
    let dir = agents_dir(channel);
    let path = if let Some(stem) = resolve_agent_file_stem(channel, agent_ref).await {
        dir.join(format!("{stem}.json"))
    } else {
        dir.join(format!("{agent_ref}.json"))
    };

    let agent_config = match read_agent_config(&path).await {
        Some(cfg) => cfg,
        None => return,
    };

    let Some(policy_val) = agent_config.get("permission_policy") else {
        return;
    };

    // Apply sandbox policy.
    if let Some(sandbox_str) = policy_val.get("sandbox").and_then(|v| v.as_str()) {
        use savfox_core::protocol::SandboxPolicy;
        let sandbox = match sandbox_str {
            "read-only" => Some(SandboxPolicy::ReadOnly),
            "workspace-write" => Some(SandboxPolicy::new_workspace_write_policy()),
            "danger-full-access" => Some(SandboxPolicy::DangerFullAccess),
            _ => None,
        };
        if let Some(sb) = sandbox
            && let Err(e) = config.sandbox_policy.set(sb)
        {
            tracing::warn!("agent permission policy sandbox rejected by constraints: {e}");
        }
    }

    // Apply approval policy.
    if let Some(approval_str) = policy_val.get("approval").and_then(|v| v.as_str()) {
        use savfox_core::protocol::AskForApproval;
        let approval = match approval_str {
            "untrusted" => Some(AskForApproval::UnlessTrusted),
            "on-failure" => Some(AskForApproval::OnFailure),
            "on-request" => Some(AskForApproval::OnRequest),
            "never" => Some(AskForApproval::Never),
            _ => None,
        };
        if let Some(ap) = approval
            && let Err(e) = config.approval_policy.set(ap)
        {
            tracing::warn!("agent permission policy approval rejected by constraints: {e}");
        }
    }

    // Apply tool access policy.
    if let Some(tool_access_val) = policy_val.get("tool_access")
        && let Ok(tool_access) = serde_json::from_value::<ToolAccessPolicy>(tool_access_val.clone())
    {
        config.tool_access_policy = Some(tool_access);
    }
}

pub(crate) fn default_agent_config_from_source(config: &Value) -> Value {
    let mut default_config = config.clone();
    default_config["id"] = json!("default");
    normalize_agent_config(&mut default_config, "default", true);
    default_config["builtin"] = json!(true);
    default_config["status"] = json!("active");
    default_config["is_default"] = json!(true);
    default_config
}

async fn clear_default_agent_markers(
    channel: &GatewayChannel,
    keep_stem: Option<&str>,
) -> Result<(), String> {
    let dir = agents_dir(channel);
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(format!("failed to read agents directory: {err}")),
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
        if keep_stem.is_some_and(|keep| stem.eq_ignore_ascii_case(keep)) {
            continue;
        }

        let Some(mut config) = read_agent_config(&path).await else {
            continue;
        };
        if !config
            .get("is_default")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }

        config["is_default"] = json!(false);
        write_agent_config(&path, &config).await?;
    }

    Ok(())
}

async fn load_default_agent_config(channel: &GatewayChannel) -> Value {
    let path = agents_dir(channel).join("default.json");
    let mut config = read_agent_config(&path)
        .await
        .unwrap_or_else(default_agent_stub);
    normalize_agent_config(&mut config, "default", true);
    config
}

async fn find_agent_name_conflict(
    channel: &GatewayChannel,
    candidate_name: &str,
    exclude_stem: Option<&str>,
) -> Option<String> {
    let wanted = normalized_agent_name_key(candidate_name)?;
    let exclude_default = exclude_stem.is_some_and(|value| value.eq_ignore_ascii_case("default"));

    if !exclude_default {
        let default_config = load_default_agent_config(channel).await;
        let default_name = default_agent_name_from_config(&default_config, "default");
        if normalized_agent_name_key(&default_name).as_deref() == Some(wanted.as_str()) {
            return Some("default".to_owned());
        }
    }

    let dir = agents_dir(channel);
    let mut entries = tokio::fs::read_dir(&dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }

        let Some(stem) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(str::to_string)
        else {
            continue;
        };

        if stem.eq_ignore_ascii_case("default") {
            continue;
        }
        if exclude_stem.is_some_and(|exclude| stem.eq_ignore_ascii_case(exclude)) {
            continue;
        }

        let Some(config) = read_agent_config(&path).await else {
            continue;
        };
        let current_name = default_agent_name_from_config(&config, &stem);
        if normalized_agent_name_key(&current_name).as_deref() == Some(wanted.as_str()) {
            return Some(stem);
        }
    }

    None
}

async fn resolve_agent_file_stem(channel: &GatewayChannel, agent_ref: &str) -> Option<String> {
    let dir = agents_dir(channel);
    let trimmed = agent_ref.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(safe_ref) = sanitize_agent_file_stem(trimmed) {
        let direct = dir.join(format!("{safe_ref}.json"));
        if direct.exists() {
            return Some(safe_ref);
        }
    }

    let mut entries = tokio::fs::read_dir(&dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_owned())
        else {
            continue;
        };

        if stem.eq_ignore_ascii_case(trimmed) {
            return Some(stem);
        }

        let Some(config) = read_agent_config(&path).await else {
            continue;
        };

        let id_match = config
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_some_and(|id| id.eq_ignore_ascii_case(trimmed));
        if id_match {
            return Some(stem);
        }

        let name_match = config
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_some_and(|name| name.eq_ignore_ascii_case(trimmed));
        if name_match {
            return Some(stem);
        }

        let identity_match = config
            .get("identity")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_some_and(|name| name.eq_ignore_ascii_case(trimmed));
        if identity_match {
            return Some(stem);
        }
    }

    None
}

async fn resolve_agent_files_dir(channel: &GatewayChannel, agent_ref: &str) -> PathBuf {
    let base = agents_dir(channel);
    let safe_ref = sanitize_agent_file_stem(agent_ref).unwrap_or_else(|| "default".to_owned());
    let by_ref = base.join(&safe_ref);
    if by_ref.exists() {
        return by_ref;
    }

    if let Some(stem) = resolve_agent_file_stem(channel, agent_ref).await {
        let by_stem = base.join(&stem);
        if by_stem.exists() {
            return by_stem;
        }
    }

    by_ref
}

pub(crate) async fn handle_agents_list(channel: &Arc<GatewayChannel>) -> RpcResult {
    let dir = agents_dir(channel);
    let mut agents = vec![load_default_agent_config(channel).await];

    // Scan agents directory for user-defined agents.
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                let Some(file_stem) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_owned())
                else {
                    continue;
                };
                if file_stem.eq_ignore_ascii_case("default") {
                    continue;
                }
                if let Some(mut config) = read_agent_config(&path).await {
                    normalize_agent_config(&mut config, &file_stem, false);
                    agents.push(config);
                }
            }
        }
    }

    let mut enriched = Vec::with_capacity(agents.len());
    for mut agent in agents {
        let agent_id = agent
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if !agent_id.is_empty() {
            let chain = savfox_core::a2a::delegation_chain_for(agent_id).await;
            let chain_json: Vec<Value> = chain
                .iter()
                .map(|entry| {
                    json!({
                        "parent_agent": entry.parent_agent,
                        "child_agent": entry.child_agent,
                        "spawned_at": entry.spawned_at,
                        "purpose": entry.purpose,
                    })
                })
                .collect();
            let delegation_parent = chain.last().map(|entry| entry.parent_agent.clone());
            agent["delegation_parent"] =
                serde_json::to_value(delegation_parent).unwrap_or(Value::Null);
            agent["delegation_depth"] = json!(chain_json.len());
            agent["delegation_chain"] = json!(chain_json);
        }
        enriched.push(agent);
    }

    Ok(json!({ "agents": enriched }))
}

pub(crate) async fn handle_agents_get(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let agent_ref = params
        .get("id")
        .or_else(|| params.get("name"))
        .or_else(|| params.get("agent"))
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if agent_ref.trim().is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'name' or 'id' parameter".to_owned(),
        ));
    }

    if agent_ref.trim().eq_ignore_ascii_case("default") {
        return Ok(load_default_agent_config(channel).await);
    }

    let Some(file_stem) = resolve_agent_file_stem(channel, agent_ref).await else {
        return Err((INVALID_REQUEST, format!("agent not found: {agent_ref}")));
    };
    if file_stem.eq_ignore_ascii_case("default") {
        return Ok(load_default_agent_config(channel).await);
    }
    let path = agents_dir(channel).join(format!("{file_stem}.json"));
    let Some(mut config) = read_agent_config(&path).await else {
        return Err((INVALID_REQUEST, format!("agent not found: {agent_ref}")));
    };

    normalize_agent_config(&mut config, &file_stem, false);

    Ok(config)
}

pub(crate) async fn handle_agents_create(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let raw_id = params
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if raw_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_owned()));
    }
    let id = sanitize_agent_file_stem(raw_id).ok_or_else(|| {
        (
            INVALID_REQUEST,
            "invalid 'id' parameter (empty after sanitization)".to_owned(),
        )
    })?;
    if id != raw_id {
        return Err((
            INVALID_REQUEST,
            "invalid 'id' parameter: use letters, numbers, '-', '_' without path/special characters".to_owned(),
        ));
    }
    if id.eq_ignore_ascii_case("default") {
        return Err((
            INVALID_REQUEST,
            "the default agent already exists; edit it instead".to_owned(),
        ));
    }
    if resolve_agent_file_stem(channel, &id).await.is_some() {
        return Err((INVALID_REQUEST, format!("agent already exists: {id}")));
    }

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_default();
    if name.is_empty() {
        return Err((INVALID_REQUEST, "missing 'name' parameter".to_owned()));
    }
    if find_agent_name_conflict(channel, &name, None)
        .await
        .is_some()
    {
        return Err((
            INVALID_REQUEST,
            format!("agent name already exists: {name}"),
        ));
    }

    let description = params
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let system_prompt = params
        .get("system_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Support new `models` object format
    let models_obj = params.get("models");

    let mut agent_config = json!({
        "id": id,
        "name": name,
        "description": description,
        "system_prompt": system_prompt,
        "created_at": chrono::Utc::now().to_rfc3339(),
    });

    // Store models in new format, with backward compat for flat `model` field
    if let Some(models) = models_obj {
        agent_config["models"] = models.clone();
    }
    if !model.is_empty() {
        agent_config["model"] = json!(model);
        // Also set as models.primary if models wasn't provided
        if models_obj.is_none() {
            agent_config["models"] = json!({ "primary": model });
        }
    }

    // Per-agent config overrides
    for key in &[
        "provider",
        "thinking",
        "verbose",
        "memory",
        "compaction",
        "sandbox",
        "heartbeat",
        "group_activation",
        "group_keywords",
        "agent_aliases",
        "ingest_policy",
        "external_bot_policy",
        "idle_reply",
        "dm_scope",
        "identity",
        "permission_policy",
        "matrix_auto_user_channels",
    ] {
        if let Some(val) = params.get(*key) {
            agent_config[*key] = val.clone();
        }
    }

    let dir = agents_dir(channel);
    let _ = tokio::fs::create_dir_all(&dir).await;
    let path = dir.join(format!("{id}.json"));
    if path.exists() {
        return Err((INVALID_REQUEST, format!("agent already exists: {id}")));
    }
    let data = serde_json::to_string_pretty(&agent_config).unwrap_or_default();
    if let Err(err) = tokio::fs::write(&path, data).await {
        return Err((
            INTERNAL_ERROR,
            format!("failed to write agent config: {err}"),
        ));
    }

    Ok(json!({
        "id": id,
        "name": name,
        "status": "created",
    }))
}

pub(crate) async fn handle_agents_update(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent_ref = params
        .get("id")
        .or_else(|| params.get("name"))
        .or_else(|| params.get("agent"))
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent_ref.trim().is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'id' or 'name' parameter".to_owned(),
        ));
    }

    let dir = agents_dir(channel);
    let resolved_id = resolve_agent_file_stem(channel, agent_ref)
        .await
        .or_else(|| sanitize_agent_file_stem(agent_ref))
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("invalid agent reference: {agent_ref}"),
            )
        })?;
    let path = dir.join(format!("{resolved_id}.json"));
    if let Some(name) = params
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && find_agent_name_conflict(channel, name, Some(resolved_id.as_str()))
            .await
            .is_some()
    {
        return Err((
            INVALID_REQUEST,
            format!("agent name already exists: {name}"),
        ));
    }

    // Read existing config or start fresh.
    let mut config = if resolved_id.eq_ignore_ascii_case("default") {
        load_default_agent_config(channel).await
    } else {
        read_agent_config(&path)
            .await
            .unwrap_or(json!({"id": resolved_id, "name": agent_ref}))
    };
    config["id"] = json!(resolved_id.clone());

    // Merge updatable fields.
    if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
        config["name"] = json!(name);
    }
    if let Some(desc) = params.get("description").and_then(|v| v.as_str()) {
        config["description"] = json!(desc);
    }
    if let Some(model) = params.get("model").and_then(|v| v.as_str()) {
        config["model"] = json!(model);
    }
    if let Some(provider) = params.get("provider").and_then(|v| v.as_str()) {
        config["provider"] = json!(provider);
    }
    if let Some(prompt) = params.get("system_prompt").and_then(|v| v.as_str()) {
        config["system_prompt"] = json!(prompt);
    }
    if let Some(models) = params.get("models") {
        config["models"] = models.clone();
    }
    if let Some(fallbacks) = params.get("fallback_models") {
        // Legacy: also store in models.fallbacks
        if config.get("models").is_none() {
            config["models"] = json!({});
        }
        config["models"]["fallbacks"] = fallbacks.clone();
    }
    // Per-agent config overrides
    for key in &[
        "thinking",
        "verbose",
        "memory",
        "compaction",
        "sandbox",
        "heartbeat",
        "group_activation",
        "group_keywords",
        "agent_aliases",
        "ingest_policy",
        "external_bot_policy",
        "idle_reply",
        "dm_scope",
        "identity",
        "permission_policy",
        "matrix_auto_user_channels",
    ] {
        if let Some(val) = params.get(*key) {
            config[*key] = val.clone();
        }
    }

    if resolved_id.eq_ignore_ascii_case("default") {
        config["is_default"] = json!(true);
    } else if let Some(is_default) = params.get("is_default").and_then(Value::as_bool) {
        config["is_default"] = json!(is_default);
    }
    config["updated_at"] = json!(chrono::Utc::now().to_rfc3339());
    normalize_agent_config(
        &mut config,
        &resolved_id,
        resolved_id.eq_ignore_ascii_case("default"),
    );

    let _ = tokio::fs::create_dir_all(&dir).await;
    if let Err(err) = write_agent_config(&path, &config).await {
        return Err((INTERNAL_ERROR, err));
    }

    let set_as_default = resolved_id.eq_ignore_ascii_case("default")
        || params
            .get("is_default")
            .and_then(Value::as_bool)
            .unwrap_or(false);

    if set_as_default {
        if let Err(err) = clear_default_agent_markers(channel, Some(resolved_id.as_str())).await {
            return Err((INTERNAL_ERROR, err));
        }
        if !resolved_id.eq_ignore_ascii_case("default") {
            let default_path = dir.join("default.json");
            let default_config = default_agent_config_from_source(&config);
            if let Err(err) = write_agent_config(&default_path, &default_config).await {
                return Err((INTERNAL_ERROR, err));
            }
        }
    }

    Ok(json!({ "id": resolved_id, "status": "updated" }))
}

pub(crate) async fn handle_agents_delete(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent_ref = params
        .get("id")
        .or_else(|| params.get("name"))
        .or_else(|| params.get("agent"))
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent_ref.trim().is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'id' or 'name' parameter".to_owned(),
        ));
    }

    let resolved_id = resolve_agent_file_stem(channel, agent_ref)
        .await
        .or_else(|| sanitize_agent_file_stem(agent_ref))
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("invalid agent reference: {agent_ref}"),
            )
        })?;
    if resolved_id.eq_ignore_ascii_case("default") {
        return Err((
            INVALID_REQUEST,
            "cannot delete the default agent".to_owned(),
        ));
    }

    let dir = agents_dir(channel);
    let path = dir.join(format!("{resolved_id}.json"));
    let _ = tokio::fs::remove_file(&path).await;

    // Also remove the agent's files directory.
    let files_dir = dir.join(&resolved_id);
    let _ = tokio::fs::remove_dir_all(&files_dir).await;
    if let Some(ref_dir) = sanitize_agent_file_stem(agent_ref)
        && ref_dir != resolved_id
    {
        let _ = tokio::fs::remove_dir_all(dir.join(ref_dir)).await;
    }

    Ok(json!({ "id": resolved_id, "status": "deleted" }))
}

/// Reset an agent config to its default stub.
///
/// For the "default" agent, this replaces the saved config with the hardcoded
/// stub. For user-created agents, this clears all overrides and keeps only the
/// id and name.
pub(crate) async fn handle_agents_reset(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent_ref = params
        .get("id")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent_ref.trim().is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_owned()));
    }

    let dir = agents_dir(channel);
    let resolved_id = resolve_agent_file_stem(channel, agent_ref)
        .await
        .or_else(|| sanitize_agent_file_stem(agent_ref))
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("invalid agent reference: {agent_ref}"),
            )
        })?;

    let path = dir.join(format!("{resolved_id}.json"));

    let mut config = if resolved_id.eq_ignore_ascii_case("default") {
        // Reset to hardcoded default stub.
        default_agent_stub()
    } else {
        // For user agents, keep id/name but clear everything else.
        let existing = read_agent_config(&path).await;
        let name = existing
            .as_ref()
            .and_then(|c| c.get("name").and_then(|v| v.as_str()))
            .unwrap_or(agent_ref)
            .to_owned();
        json!({
            "id": resolved_id,
            "name": name,
            "status": "active",
        })
    };

    config["updated_at"] = json!(chrono::Utc::now().to_rfc3339());
    normalize_agent_config(
        &mut config,
        &resolved_id,
        resolved_id.eq_ignore_ascii_case("default"),
    );

    let _ = tokio::fs::create_dir_all(&dir).await;
    if let Err(err) = write_agent_config(&path, &config).await {
        return Err((INTERNAL_ERROR, err));
    }

    Ok(json!({ "id": resolved_id, "status": "reset", "config": config }))
}

pub(crate) async fn handle_agents_files_list(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent_ref = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let dir = resolve_agent_files_dir(channel, agent_ref).await;
    let mut files = Vec::new();

    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_file()
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
            {
                let size = tokio::fs::metadata(&path)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0);
                files.push(json!({ "name": name, "size": size }));
            }
        }
    }

    Ok(json!({ "agent_id": agent_ref, "files": files }))
}

pub(crate) async fn handle_agents_files_get(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent_ref = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let file_path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if file_path.is_empty() {
        return Err((INVALID_REQUEST, "missing 'path' parameter".to_owned()));
    }

    // Sanitize path to prevent directory traversal.
    let safe_name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_path);

    let dir = resolve_agent_files_dir(channel, agent_ref).await;
    let path = dir.join(safe_name);
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Ok(json!({ "agent_id": agent_ref, "path": safe_name, "content": content })),
        Err(_) => Ok(json!({ "agent_id": agent_ref, "path": safe_name, "content": null })),
    }
}

pub(crate) async fn handle_agents_files_set(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent_ref = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let file_path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if file_path.is_empty() {
        return Err((INVALID_REQUEST, "missing 'path' parameter".to_owned()));
    }

    // Sanitize path to prevent directory traversal.
    let safe_name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_path);

    let dir = resolve_agent_files_dir(channel, agent_ref).await;
    let _ = tokio::fs::create_dir_all(&dir).await;
    let path = dir.join(safe_name);

    if let Err(err) = tokio::fs::write(&path, content).await {
        return Err((INTERNAL_ERROR, format!("failed to write file: {err}")));
    }

    Ok(json!({ "agent_id": agent_ref, "path": safe_name, "status": "saved" }))
}

pub(crate) async fn handle_agents_files_delete(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent_ref = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let file_path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if file_path.is_empty() {
        return Err((INVALID_REQUEST, "missing 'path' parameter".to_owned()));
    }

    // Sanitize path to prevent directory traversal.
    let safe_name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_path);

    let dir = resolve_agent_files_dir(channel, agent_ref).await;
    let path = dir.join(safe_name);
    match tokio::fs::remove_file(&path).await {
        Ok(_) => Ok(json!({ "agent_id": agent_ref, "path": safe_name, "status": "deleted" })),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(json!({
            "agent_id": agent_ref,
            "path": safe_name,
            "status": "deleted"
        })),
        Err(err) => Err((INTERNAL_ERROR, format!("failed to delete file: {err}"))),
    }
}

// ── Agent Skills ────────────────────────────────────────────────────────────

pub(crate) async fn handle_agents_skills_get(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent_ref = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .or_else(|| params.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent_ref.trim().is_empty() {
        return Err((INVALID_REQUEST, "missing 'agent' parameter".to_owned()));
    }

    let config = if agent_ref.trim().eq_ignore_ascii_case("default") {
        load_default_agent_config(channel).await
    } else {
        let Some(file_stem) = resolve_agent_file_stem(channel, agent_ref).await else {
            return Err((INVALID_REQUEST, format!("agent not found: {agent_ref}")));
        };
        if file_stem.eq_ignore_ascii_case("default") {
            load_default_agent_config(channel).await
        } else {
            let path = agents_dir(channel).join(format!("{file_stem}.json"));
            read_agent_config(&path)
                .await
                .ok_or_else(|| (INVALID_REQUEST, format!("agent not found: {agent_ref}")))?
        }
    };

    let skills = config.get("skills").cloned().unwrap_or_else(|| json!([]));

    Ok(json!({ "agent": agent_ref, "skills": skills }))
}

pub(crate) async fn handle_agents_skills_set(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent_ref = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .or_else(|| params.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent_ref.trim().is_empty() {
        return Err((INVALID_REQUEST, "missing 'agent' parameter".to_owned()));
    }

    let skill_name = params.get("skill").and_then(|v| v.as_str()).unwrap_or("");
    if skill_name.trim().is_empty() {
        return Err((INVALID_REQUEST, "missing 'skill' parameter".to_owned()));
    }

    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let dir = agents_dir(channel);
    let resolved_id = resolve_agent_file_stem(channel, agent_ref)
        .await
        .or_else(|| sanitize_agent_file_stem(agent_ref))
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("invalid agent reference: {agent_ref}"),
            )
        })?;
    let path = dir.join(format!("{resolved_id}.json"));

    let mut config = if resolved_id.eq_ignore_ascii_case("default") {
        load_default_agent_config(channel).await
    } else {
        read_agent_config(&path)
            .await
            .ok_or_else(|| (INVALID_REQUEST, format!("agent not found: {agent_ref}")))?
    };

    // Get existing skills list or create empty one.
    let mut skills: Vec<String> = config
        .get("skills")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default();

    if enabled {
        if !skills.iter().any(|s| s == skill_name) {
            skills.push(skill_name.to_owned());
        }
    } else {
        skills.retain(|s| s != skill_name);
    }

    config["skills"] = json!(skills);
    config["updated_at"] = json!(chrono::Utc::now().to_rfc3339());

    let _ = tokio::fs::create_dir_all(&dir).await;
    if let Err(err) = write_agent_config(&path, &config).await {
        return Err((INTERNAL_ERROR, err));
    }

    Ok(json!({
        "agent": resolved_id,
        "skill": skill_name,
        "enabled": enabled,
        "skills": config["skills"],
    }))
}
// ── Agent Avatar Management (#63) ───────────────────────────────────────────

/// Store an avatar path in the agent's config JSON.
pub(crate) async fn handle_agent_avatar_set(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let avatar = params.get("avatar").and_then(|v| v.as_str()).unwrap_or("");
    if avatar.is_empty() {
        return Err((INVALID_PARAMS, "missing 'avatar' parameter".to_owned()));
    }

    let dir = agents_dir(channel);
    if !dir.exists() {
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("mkdir error: {e}")))?;
    }

    let config_path = dir.join(format!("{agent_id}.json"));
    let mut config: Value = if config_path.exists() {
        crate::json_store::load_json_value(&config_path).await
    } else {
        json!({ "id": agent_id, "name": agent_id })
    };

    config["avatar"] = json!(avatar);
    config["updated_at"] = json!(chrono::Utc::now().to_rfc3339());

    let json_str = serde_json::to_string_pretty(&config)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(&config_path, json_str)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({
        "status": "ok",
        "agent": agent_id,
        "avatar": avatar,
    }))
}

/// Return the current avatar path for an agent.
pub(crate) async fn handle_agent_avatar_get(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let config_path = agents_dir(channel).join(format!("{agent_id}.json"));
    let avatar = if config_path.exists() {
        let config = crate::json_store::load_json_value(&config_path).await;
        config
            .get("avatar")
            .and_then(|v| v.as_str())
            .map(String::from)
    } else {
        None
    };

    Ok(json!({
        "agent": agent_id,
        "avatar": avatar,
    }))
}
