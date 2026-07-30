use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose;
use savfox_exec_policy::{Decision, blocking_append_prefix_rule, blocking_remove_prefix_rule};
use savfox_skill_registry::package::SkillSourceType;
use savfox_skill_registry::{SkillInstaller, SkillManifest, SkillPackage, SkillSource};
use savfox_utils::home_dir::SKILLS_SUBDIR;
use serde_json::{Value, json};

use super::super::types::{INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, RpcResult};
use super::super::utils::{opt_bool, opt_str, require_str};
use crate::channel::GatewayChannel;
use crate::exec_approval::{
    ApprovalForwardingConfig, ExecApprovalRequest, ExecApprovalResolution, ResolveOutcome,
    forward_approval_to_chat, generate_approval_nonce, list_pending_approvals,
    notify_approval_resolved, persist_pending_approval, persist_resolved_approval,
};
use crate::security::approval_coordinator::{
    AuthenticatedApprovalOutcome, resolve_authenticated_approval,
};
use crate::security::execution_policy::{ApprovalClientCapabilities, resolve_execution_security};
use crate::session::GatewaySessionManager;
use crate::{approval_policy_store, skills_store, tts_service};

// ── TTS (text-to-speech) ────────────────────────────────────────────────────

pub(crate) async fn handle_tts_status(channel: &Arc<GatewayChannel>) -> RpcResult {
    tts_service::status(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
}

pub(crate) async fn handle_tts_providers(channel: &Arc<GatewayChannel>) -> RpcResult {
    tts_service::providers_with_status(&channel.config().savfox_home)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))
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
        return Err((INVALID_PARAMS, "missing 'enabled' parameter".to_owned()));
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
    let url = require_str(params, "url")?.trim().to_owned();
    if url.is_empty() {
        return Err((INVALID_PARAMS, "url must not be empty".to_owned()));
    }

    // Derive a skill folder name from the URL as {domain}/{restpath}
    // e.g. "https://github.com/org/repo.git" → "github.com/org/repo"
    let name = {
        let stripped = url.trim_end_matches('/').trim_end_matches(".git");
        // Extract "domain/rest/path" from "https://domain/rest/path"
        let after_scheme = stripped.split("://").nth(1).unwrap_or(stripped);
        // Clean up: filter empty segments, rejoin
        let segments: Vec<&str> = after_scheme.split('/').filter(|s| !s.is_empty()).collect();
        segments.join("/")
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err((
            INVALID_PARAMS,
            "could not derive skill name from url".to_owned(),
        ));
    }

    let skills_dir = channel.config().savfox_home.join(SKILLS_SUBDIR);
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
            subdir: params
                .get("subdir")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned()),
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
            result.error.unwrap_or_else(|| "unknown error".to_owned()),
        ))
    }
}

pub(crate) async fn handle_skills_install_zip(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    use savfox_skill_registry::{ConflictStrategy, zip_installer};

    // Expect base64-encoded zip data.
    let data_b64 = require_str(params, "data")?;
    let zip_bytes = general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| (INVALID_PARAMS, format!("invalid base64 data: {e}")))?;

    let strategy = match params.get("conflict_strategy").and_then(|v| v.as_str()) {
        Some("skip") => ConflictStrategy::Skip,
        _ => ConflictStrategy::Overwrite,
    };

    let skills_dir = channel.config().savfox_home.join(SKILLS_SUBDIR);
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
        "mode": policy.get("mode").cloned().unwrap_or(Value::String("auto".to_owned())),
        "execution_mode": policy.get("execution_mode").cloned().unwrap_or(Value::String("unattended".to_owned())),
        "rules": policy.get("rules").cloned().unwrap_or(Value::Array(Vec::new())),
        "deprecated": policy.get("deprecated").cloned().unwrap_or(Value::Bool(true)),
        "read_only": policy.get("read_only").cloned().unwrap_or(Value::Bool(true)),
        "canonical_rule_source": policy.get("canonical_rule_source").cloned().unwrap_or(Value::String("rules/default.rules".to_owned())),
        "migration": policy.get("migration").cloned().unwrap_or(Value::Null),
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

    // S3: server always generates the single-use nonce. The caller cannot
    // pre-supply one — even a malicious agent that guesses the request id
    // cannot inject the matching nonce because we overwrite this field
    // here before persisting.
    let request = ExecApprovalRequest {
        id: uuid::Uuid::now_v7().to_string(),
        command: crate::exec_approval::sanitized_approval_text(command, 2_048),
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
            .map(|reason| crate::exec_approval::sanitized_approval_text(reason, 512)),
        agent_id: params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        session_id: params
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        created_at_ms: now_ms,
        expires_at_ms: now_ms.saturating_add(300_000),
        nonce: generate_approval_nonce(),
        ..Default::default()
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
        "command": request.command,
        "status": "pending",
        "nonce": request.nonce,
    }))
}

pub(crate) async fn handle_exec_approval_resolve(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_mgr: &Arc<GatewaySessionManager>,
    authenticated_subject: &str,
) -> RpcResult {
    let request_id = require_str(params, "request_id")?;
    let legacy_approved = opt_bool(params, "approved", false);
    let decision = params
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or(if legacy_approved {
            "approve-once"
        } else {
            "deny"
        });
    let nonce = require_str(params, "nonce")?;
    let resolved_by = Some(format!("token:{authenticated_subject}"));
    let reason = params
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|reason| crate::exec_approval::sanitized_approval_text(reason, 512));

    match resolve_authenticated_approval(
        channel,
        request_id,
        nonce,
        decision,
        resolved_by.clone(),
        reason.clone(),
    )
    .await
    .map_err(|error| {
        (
            INTERNAL_ERROR,
            format!("failed to submit coordinated approval: {error}"),
        )
    })? {
        AuthenticatedApprovalOutcome::Resolved { decision } => {
            let approved = decision.starts_with("approved");
            let resolution = ExecApprovalResolution {
                id: request_id.to_owned(),
                approved,
                resolved_by,
                reason,
                nonce: nonce.to_owned(),
            };
            let config = load_approval_forwarding_config();
            notify_approval_resolved(channel, session_mgr, &resolution, &config).await;
            return Ok(json!({
                "request_id": request_id,
                "approved": approved,
                "decision": decision,
                "status": "resolved",
                "resolved_pending": true,
                "coordinated": true,
            }));
        }
        AuthenticatedApprovalOutcome::NonceMismatch => {
            return Err((
                INVALID_REQUEST,
                "approval nonce missing or invalid".to_owned(),
            ));
        }
        AuthenticatedApprovalOutcome::UnsupportedDecision => {
            return Err((
                INVALID_REQUEST,
                "decision is not available for this approval".to_owned(),
            ));
        }
        AuthenticatedApprovalOutcome::NotCoordinated => {}
    }
    if let Some(request) =
        crate::exec_approval::find_pending_approval(&channel.config().savfox_home, request_id)
            .await
            .map_err(|error| (INTERNAL_ERROR, error))?
        && request.is_coordinator_owned()
    {
        return Err((
            INVALID_REQUEST,
            "approval coordinator is no longer active; re-issue the request".to_owned(),
        ));
    }

    let resolution = ExecApprovalResolution {
        id: request_id.to_owned(),
        approved: legacy_approved,
        resolved_by,
        reason,
        nonce: nonce.to_owned(),
    };

    // S3: nonce verification + persistence happen atomically inside
    // `persist_resolved_approval`. Reject malformed/legacy/replayed
    // resolutions with `INVALID_REQUEST` and never mutate the store.
    let outcome = persist_resolved_approval(&channel.config().savfox_home, &resolution)
        .await
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to persist approval resolution: {err}"),
            )
        })?;
    let resolved_pending = match outcome {
        ResolveOutcome::Resolved => true,
        ResolveOutcome::NotPending => {
            return Err((INVALID_REQUEST, "approval is not pending".to_owned()));
        }
        ResolveOutcome::NonceMismatch => {
            return Err((
                INVALID_REQUEST,
                "approval nonce missing or invalid".to_owned(),
            ));
        }
        ResolveOutcome::LegacyMissingNonce => {
            return Err((
                INVALID_REQUEST,
                "approval has no server-issued nonce; re-issue the request".to_owned(),
            ));
        }
    };

    // Notify channels about the resolution.
    let config = load_approval_forwarding_config();
    notify_approval_resolved(channel, session_mgr, &resolution, &config).await;

    Ok(json!({
        "request_id": request_id,
        "approved": legacy_approved,
        "status": "resolved",
        "resolved_pending": resolved_pending,
    }))
}

fn security_command_argv(params: &Value) -> Result<Vec<String>, (i64, String)> {
    let command = params
        .get("command")
        .ok_or_else(|| (INVALID_PARAMS, "missing 'command' parameter".to_owned()))?;
    let argv = match command {
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_owned).ok_or_else(|| {
                    (
                        INVALID_PARAMS,
                        "command array entries must be strings".to_owned(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Value::String(command) => shlex::split(command)
            .ok_or_else(|| (INVALID_PARAMS, "could not parse command".to_owned()))?,
        _ => {
            return Err((
                INVALID_PARAMS,
                "'command' must be a string or argv array".to_owned(),
            ));
        }
    };
    if argv.is_empty() {
        return Err((INVALID_PARAMS, "command cannot be empty".to_owned()));
    }
    Ok(argv)
}

fn security_rule_decision(params: &Value) -> Result<Decision, (i64, String)> {
    match require_str(params, "decision")? {
        "allow" => Ok(Decision::Allow),
        "ask" | "prompt" => Ok(Decision::Prompt),
        "deny" | "forbidden" => Ok(Decision::Forbidden),
        value => Err((INVALID_PARAMS, format!("unsupported decision '{value}'"))),
    }
}

pub(crate) async fn handle_security_policy_simulate(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let argv = security_command_argv(params)?;
    let agent = params
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let resolved = resolve_execution_security(
        channel.config(),
        &channel.config().savfox_home,
        agent,
        ApprovalClientCapabilities::interactive(),
    )
    .await;
    let simulation = savfox_core::simulate_exec_policy(&resolved.config, &argv)
        .await
        .map_err(|error| (INTERNAL_ERROR, error.to_string()))?;
    Ok(json!({
        "command": argv,
        "agent": agent,
        "decision": simulation.decision,
        "reason": simulation.reason,
        "bypass_sandbox": simulation.bypass_sandbox,
        "proposed_rule": simulation.proposed_rule,
        "matched_rules": simulation.matched_rules,
        "execution_mode": resolved.context.mode.as_str(),
        "sandbox": resolved.context.effective_sandbox,
        "enforcement": resolved.context.sandbox_enforcement,
        "policy_fingerprint": resolved.context.policy_fingerprint,
        "fallback_reason": resolved.context.fallback_reason,
    }))
}

pub(crate) async fn handle_security_rules_list(channel: &Arc<GatewayChannel>) -> RpcResult {
    let path = channel
        .config()
        .savfox_home
        .join("rules")
        .join("default.rules");
    let contents = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err((INTERNAL_ERROR, error.to_string())),
    };
    let rules = contents
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            let parsed = line
                .trim()
                .strip_prefix("prefix_rule(pattern=")
                .and_then(|body| body.split_once(", decision=\""))
                .and_then(|(pattern, decision)| {
                    let decision = decision.strip_suffix("\")")?;
                    let command = serde_json::from_str::<Vec<String>>(pattern).ok()?;
                    Some((command, decision))
                });
            json!({
                "index": index,
                "source": "user",
                "path": path,
                "rule": line,
                "command": parsed.as_ref().map(|(command, _)| command),
                "decision": parsed.as_ref().map(|(_, decision)| decision),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "rules": rules, "count": rules.len(), "path": path }))
}

pub(crate) async fn handle_security_rules_add(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let argv = security_command_argv(params)?;
    let decision = security_rule_decision(params)?;
    let path = channel
        .config()
        .savfox_home
        .join("rules")
        .join("default.rules");
    let command = argv.clone();
    tokio::task::spawn_blocking(move || {
        blocking_append_prefix_rule(&path, &command, decision).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| (INTERNAL_ERROR, error.to_string()))?
    .map_err(|error| (INTERNAL_ERROR, error))?;
    Ok(json!({ "status": "added", "command": argv }))
}

pub(crate) async fn handle_security_rules_remove(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let argv = security_command_argv(params)?;
    let decision = security_rule_decision(params)?;
    let path = channel
        .config()
        .savfox_home
        .join("rules")
        .join("default.rules");
    let command = argv.clone();
    let removed = tokio::task::spawn_blocking(move || {
        blocking_remove_prefix_rule(&path, &command, decision).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| (INTERNAL_ERROR, error.to_string()))?
    .map_err(|error| (INTERNAL_ERROR, error))?;
    Ok(json!({
        "status": if removed { "removed" } else { "not_found" },
        "command": argv,
    }))
}

/// Load approval forwarding config from environment variables.
fn load_approval_forwarding_config() -> ApprovalForwardingConfig {
    let enabled = std::env::var("SAVFOX_APPROVAL_FORWARDING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let mode = std::env::var("SAVFOX_APPROVAL_MODE").unwrap_or_else(|_| "targets".to_owned());

    let targets = std::env::var("SAVFOX_APPROVAL_TARGETS")
        .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
        .unwrap_or_default();

    let agent_filter = std::env::var("SAVFOX_APPROVAL_AGENT_FILTER")
        .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
        .unwrap_or_default();

    let session_filter = std::env::var("SAVFOX_APPROVAL_SESSION_FILTER")
        .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
        .unwrap_or_default();

    ApprovalForwardingConfig {
        enabled,
        mode,
        targets,
        agent_filter,
        session_filter,
    }
}
