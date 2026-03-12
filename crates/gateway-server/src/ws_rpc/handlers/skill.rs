#![allow(unused_imports)]

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose;
use serde_json::{Value, json};

use super::super::types::{INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, RpcResult};
use super::super::utils::{opt_bool, opt_str, require_str};
use crate::channel::GatewayChannel;
use crate::exec_approval::{
    ApprovalForwardingConfig, ExecApprovalRequest, ExecApprovalResolution,
    forward_approval_to_chat, list_pending_approvals, notify_approval_resolved,
    persist_pending_approval, persist_resolved_approval,
};
use crate::session::GatewaySessionManager;
use savfox_skill_registry::{SkillInstaller, SkillPackage, SkillSource, SkillManifest};
use savfox_skill_registry::package::SkillSourceType;

use crate::{approval_policy_store, skills_store, tts_service};

// ── TTS (text-to-speech) ────────────────────────────────────────────────────

pub(crate) async fn handle_tts_status(channel: &Arc<GatewayChannel>) -> RpcResult {
    tts_service::status(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

pub(crate) async fn handle_tts_providers() -> RpcResult {
    Ok(json!({ "providers": tts_service::providers() }))
}

pub(crate) async fn handle_tts_enable(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let provider = params.get("provider").and_then(|v| v.as_str());
    let voice = params.get("voice").and_then(|v| v.as_str());
    let model = params.get("model").and_then(|v| v.as_str());
    tts_service::enable(&channel.config().savfox_home, provider, voice, model)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

pub(crate) async fn handle_tts_disable(channel: &Arc<GatewayChannel>) -> RpcResult {
    tts_service::disable(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

pub(crate) async fn handle_tts_convert(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let _text = require_str(params, "text")?;
    tts_service::convert(&channel.config().savfox_home, channel.http_client(), params)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

pub(crate) async fn handle_tts_set_provider(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let provider = require_str(params, "provider")?;
    let voice = params.get("voice").and_then(|v| v.as_str());
    let model = params.get("model").and_then(|v| v.as_str());
    tts_service::set_provider(&channel.config().savfox_home, provider, voice, model)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

pub(crate) async fn handle_tts_voices(params: &Value) -> RpcResult {
    let provider = params
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("openai");
    Ok(tts_service::voices_for_provider(provider))
}

pub(crate) async fn handle_tts_set_voice(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let voice = require_str(params, "voice")?;
    tts_service::set_voice(&channel.config().savfox_home, voice)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

pub(crate) async fn handle_tts_settings(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let speed = params.get("speed").and_then(|v| v.as_f64());
    let pitch = params.get("pitch").and_then(|v| v.as_f64());
    tts_service::update_settings(&channel.config().savfox_home, speed, pitch)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

// ── Skills ──────────────────────────────────────────────────────────────────

pub(crate) async fn handle_skills_status(channel: &Arc<GatewayChannel>) -> RpcResult {
    skills_store::status(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

pub(crate) async fn handle_skills_bins(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    skills_store::bins(&channel.config().savfox_home, Some(params))
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

pub(crate) async fn handle_skills_update(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let name = require_str(params, "name")?;
    let enabled = params.get("enabled").and_then(|v| v.as_bool());
    let Some(enabled) = enabled else {
        return Err((INVALID_PARAMS, "missing 'enabled' parameter".to_string()));
    };
    skills_store::set_enabled(&channel.config().savfox_home, name, enabled)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

pub(crate) async fn handle_skills_set_env(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let key = require_str(params, "key")?;
    let value = require_str(params, "value")?;
    skills_store::set_env(&channel.config().savfox_home, key, value)
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

pub(crate) async fn handle_skills_install_url(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let url = require_str(params, "url")?.trim().to_string();
    if url.is_empty() {
        return Err((INVALID_PARAMS, "url must not be empty".to_string()));
    }

    // Derive a skill folder name from the URL as {domain}/{restpath}
    // e.g. "https://github.com/org/repo.git" → "github.com/org/repo"
    let name = {
        let stripped = url
            .trim_end_matches('/')
            .trim_end_matches(".git");
        // Extract "domain/rest/path" from "https://domain/rest/path"
        let after_scheme = stripped
            .split("://")
            .nth(1)
            .unwrap_or(stripped);
        // Clean up: filter empty segments, rejoin
        let segments: Vec<&str> = after_scheme
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        segments.join("/")
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err((
            INVALID_PARAMS,
            "could not derive skill name from url".to_string(),
        ));
    }

    let skills_dir = channel.config().savfox_home.join("skills");
    let installer = SkillInstaller::new(skills_dir);

    // Git URL installs use SkillSourceType::Git → skills/{domain}/{org}/{repo}
    let package = SkillPackage {
        manifest: SkillManifest {
            name: name.clone(),
            ..Default::default()
        },
        source: SkillSource {
            source_type: SkillSourceType::Git,
            url: Some(url),
            path: None,
            registry: None,
            checksum: None,
            subdir: params.get("subdir").and_then(|v| v.as_str()).map(|s| s.to_string()),
        },
        installed: false,
        installed_version: None,
        install_path: None,
    };

    let result = installer
        .install(&package, None)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("install failed: {e}")))?;

    if result.success {
        // Rescan to discover new skills and persist their state.
        let savfox_home = &channel.config().savfox_home;
        let bins_val = skills_store::bins(savfox_home, None)
            .await
            .unwrap_or_else(|_| json!({ "bins": [] }));
        let new_skills: Vec<&Value> = bins_val
            .get("bins")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|b| {
                        b.get("flock")
                            .and_then(|f| f.as_str())
                            .is_some_and(|f| f == name)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let new_count = new_skills.len();
        let auto_disabled = new_skills
            .iter()
            .any(|b| b.get("enabled").and_then(|v| v.as_bool()) == Some(false));
        let skill_names: Vec<&str> = new_skills
            .iter()
            .filter_map(|b| b.get("name").and_then(|v| v.as_str()))
            .collect();

        Ok(json!({
            "name": result.name,
            "status": "installed",
            "path": result.install_path.display().to_string(),
            "flock": name,
            "new_skills_count": new_count,
            "new_skills": skill_names,
            "auto_disabled": auto_disabled,
        }))
    } else {
        Err((
            INTERNAL_ERROR,
            result.error.unwrap_or_else(|| "unknown error".to_string()),
        ))
    }
}

pub(crate) async fn handle_skills_install_zip(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    use savfox_skill_registry::zip_installer;
    use savfox_skill_registry::ConflictStrategy;

    // Expect base64-encoded zip data.
    let data_b64 = require_str(params, "data")?;
    let zip_bytes = general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| (INVALID_PARAMS, format!("invalid base64 data: {e}")))?;

    let strategy = match params.get("conflict_strategy").and_then(|v| v.as_str()) {
        Some("skip") => ConflictStrategy::Skip,
        _ => ConflictStrategy::Overwrite,
    };

    let skills_dir = channel.config().savfox_home.join("skills");
    let result = zip_installer::install_from_zip_bytes(zip_bytes, skills_dir, strategy)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("zip install failed: {e}")))?;

    // Rescan to discover new skills and persist their state.
    let savfox_home = &channel.config().savfox_home;
    let bins_val = skills_store::bins(savfox_home, None)
        .await
        .unwrap_or_else(|_| json!({ "bins": [] }));
    let auto_disabled = bins_val
        .get("bins")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter().any(|b| {
                b.get("disabled_reason")
                    .and_then(|v| v.as_str())
                    .is_some_and(|r| r.contains("too many new skills"))
            })
        })
        .unwrap_or(false);

    Ok(json!({
        "installed": result.installed,
        "skipped": result.skipped,
        "errors": result.errors,
        "auto_disabled": auto_disabled,
    }))
}

// ── Exec approvals ──────────────────────────────────────────────────────────

pub(crate) async fn handle_exec_approvals_get(channel: &Arc<GatewayChannel>) -> RpcResult {
    let approvals = list_pending_approvals(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("failed to load approvals: {err}")))?;
    let policy = approval_policy_store::get_global(&channel.config().savfox_home)
        .await
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to load approval policy: {err}"),
            )
        })?;
    let count = approvals.len();
    Ok(json!({
        "mode": policy.get("mode").cloned().unwrap_or(Value::String("auto".to_string())),
        "rules": policy.get("rules").cloned().unwrap_or(Value::Array(Vec::new())),
        "approvals": approvals,
        "count": count,
    }))
}

pub(crate) async fn handle_exec_approvals_set(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let mode = opt_str(params, "mode", "auto");
    approval_policy_store::set_global(&channel.config().savfox_home, mode, params.get("rules"))
        .await
        .map_err(|err| (INVALID_REQUEST, err))
}

pub(crate) async fn handle_exec_approvals_node_get(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let node_id = require_str(params, "node_id")?;
    approval_policy_store::get_node(&channel.config().savfox_home, node_id)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

pub(crate) async fn handle_exec_approvals_node_set(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let node_id = require_str(params, "node_id")?;
    let mode = opt_str(params, "mode", "auto");
    approval_policy_store::set_node(
        &channel.config().savfox_home,
        node_id,
        mode,
        params.get("rules"),
    )
    .await
    .map_err(|err| (INVALID_REQUEST, err))
}

pub(crate) async fn handle_exec_approval_request(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let command = require_str(params, "command")?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let request = ExecApprovalRequest {
        id: uuid::Uuid::now_v7().to_string(),
        command: command.to_owned(),
        cwd: params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        host: params
            .get("host")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        security: params
            .get("security")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        ask: params
            .get("ask")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        agent_id: params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        session_id: params
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        created_at_ms: now_ms,
        expires_at_ms: params
            .get("expires_at_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(now_ms + 300_000), // Default 5 min expiry
    };

    if let Err(err) = persist_pending_approval(&channel.config().savfox_home, &request).await {
        return Err((
            INTERNAL_ERROR,
            format!("failed to persist approval request: {err}"),
        ));
    }

    // Load forwarding config from env and forward the approval.
    let config = load_approval_forwarding_config();
    forward_approval_to_chat(channel, session_mgr, &request, &config).await;

    Ok(json!({
        "request_id": request.id,
        "command": command,
        "status": "pending",
    }))
}

pub(crate) async fn handle_exec_approval_resolve(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let request_id = require_str(params, "request_id")?;
    let approved = opt_bool(params, "approved", false);

    let resolution = ExecApprovalResolution {
        id: request_id.to_owned(),
        approved,
        resolved_by: params
            .get("resolved_by")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        reason: params
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
    };

    let resolved_pending = persist_resolved_approval(&channel.config().savfox_home, &resolution)
        .await
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to persist approval resolution: {err}"),
            )
        })?;

    // Notify channels about the resolution.
    let config = load_approval_forwarding_config();
    notify_approval_resolved(channel, session_mgr, &resolution, &config).await;

    Ok(json!({
        "request_id": request_id,
        "approved": approved,
        "status": "resolved",
        "resolved_pending": resolved_pending,
    }))
}

/// Load approval forwarding config from environment variables.
fn load_approval_forwarding_config() -> ApprovalForwardingConfig {
    let enabled = std::env::var("SAVFOX_APPROVAL_FORWARDING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let mode = std::env::var("SAVFOX_APPROVAL_MODE").unwrap_or_else(|_| "targets".to_string());

    let targets = std::env::var("SAVFOX_APPROVAL_TARGETS")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    let agent_filter = std::env::var("SAVFOX_APPROVAL_AGENT_FILTER")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    let session_filter = std::env::var("SAVFOX_APPROVAL_SESSION_FILTER")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    ApprovalForwardingConfig {
        enabled,
        mode,
        targets,
        agent_filter,
        session_filter,
    }
}
