use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use savfox_gateway_shared::{AgentExecutionPolicy, AgentPermissionPolicy};
use savfox_utils::home_dir::AGENTS_SUBDIR;
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::types::{INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, RpcResult};
use super::channel::{channel_is_configured, load_saved_channel_configs};
use super::channel_management::load_nostr_profile;
use crate::channel::GatewayChannel;
use crate::chat_session::validate_uuid_v7_session_id;
use crate::security::execution_policy::{ApprovalClientCapabilities, resolve_execution_security};
use crate::security::path_safety::safe_join;
use crate::terminal_agent::{
    TerminalCommandResolver, TerminalCommandTemplate, TerminalTemplateValues, command_health,
    path_writable, resolve_cwd, terminal_profile_preset, terminal_profile_presets,
    terminal_runtime_metrics_snapshot,
};
use crate::terminal_pty::{
    TerminalPtyCloseReason, TerminalPtySessionKey, TerminalPtySize, TerminalPtySpawnSpec,
    TerminalPtyWrite, TerminalPtyWriteKind, terminal_pty_manager,
};

// ── Agent (single-agent operations) ─────────────────────────────────────────

const MANAGED_PTY_DEFAULT_TIMEOUT_SECS: u64 = 300;
const MANAGED_PTY_MIN_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone, Default, Deserialize)]
struct ManagedPtyDelegateSpec {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    stdin: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    interactive_command: Option<String>,
    #[serde(default)]
    interactive_args: Option<Vec<String>>,
}

fn parse_pty_size(params: &Value) -> TerminalPtySize {
    let cols = params
        .get("cols")
        .or_else(|| params.get("columns"))
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value >= 20)
        .unwrap_or(TerminalPtySize::default().cols);
    let rows = params
        .get("rows")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value >= 5)
        .unwrap_or(TerminalPtySize::default().rows);
    TerminalPtySize { cols, rows }
}

fn parse_string_array(value: Option<&Value>) -> Option<Vec<String>> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
}

fn parse_string_map(value: Option<&Value>) -> std::collections::BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|items| {
            items
                .iter()
                .filter_map(|(key, value)| {
                    let key = key.trim();
                    if key.is_empty() {
                        return None;
                    }
                    let value = value.as_str().unwrap_or("").to_owned();
                    Some((key.to_owned(), value))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn terminal_pty_key_from_params(params: &Value) -> Result<TerminalPtySessionKey, String> {
    let agent_raw = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default");
    // The agent id becomes a filesystem path segment under
    // `savfox_home/terminal-agents/<agent>/pty/...` when a managed PTY is
    // spawned. Validate it as a single safe segment so a value like
    // `../../../../etc` cannot escape savfox_home (path traversal).
    let agent = crate::security::path_safety::safe_filename_segment(agent_raw)
        .ok_or_else(|| format!("invalid agent identifier: {agent_raw:?}"))?
        .to_owned();
    let session_id = validate_uuid_v7_session_id(
        params
            .get("session_id")
            .or_else(|| params.get("session"))
            .and_then(Value::as_str),
    )?
    .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    Ok(TerminalPtySessionKey::new(agent, session_id))
}

async fn resolve_terminal_pty_spawn_spec(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    key: &TerminalPtySessionKey,
) -> Result<TerminalPtySpawnSpec, String> {
    let mut raw_agent = None;
    let mut agent_name = key.agent_id.clone();
    let mut spec = ManagedPtyDelegateSpec::default();
    if let Some((file_stem, raw_config)) =
        crate::agent_terminal_delegate::resolve_agent_config(channel.config(), &key.agent_id).await
    {
        agent_name = raw_config
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&key.agent_id)
            .to_owned();
        raw_agent = Some(file_stem);
        if let Some(delegate) = raw_config.get("terminal") {
            spec = serde_json::from_value::<ManagedPtyDelegateSpec>(delegate.clone())
                .map_err(|err| format!("invalid terminal config for managed PTY: {err}"))?;
        }
    }

    let command = params
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            spec.interactive_command
                .as_deref()
                .or(spec.command.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| {
            format!(
                "agent `{}` has no managed PTY command; configure terminal.command or pass command",
                key.agent_id
            )
        })?;
    let args = parse_string_array(params.get("args"))
        .or_else(|| spec.interactive_args.clone())
        .unwrap_or(spec.args.clone());
    let cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or(spec.cwd.clone());
    let mut env = spec.env.clone();
    env.extend(parse_string_map(params.get("env")));

    let agent_dir = raw_agent.unwrap_or_else(|| key.agent_id.clone());
    let root = channel
        .config()
        .savfox_home
        .join("terminal-agents")
        .join(agent_dir)
        .join("pty")
        .join(&key.session_id);
    let home_dir = root.join("home");
    let workspace_dir = root.join("workspace");
    let log_dir = root.join("logs");
    tokio::fs::create_dir_all(&home_dir)
        .await
        .map_err(|err| format!("failed to create managed PTY home: {err}"))?;
    tokio::fs::create_dir_all(&workspace_dir)
        .await
        .map_err(|err| format!("failed to create managed PTY workspace: {err}"))?;
    tokio::fs::create_dir_all(&log_dir)
        .await
        .map_err(|err| format!("failed to create managed PTY logs: {err}"))?;

    let values = TerminalTemplateValues {
        agent_id: key.agent_id.clone(),
        agent_name,
        session_id: key.session_id.clone(),
        agent_home: home_dir.to_string_lossy().into_owned(),
        workspace_dir: workspace_dir.to_string_lossy().into_owned(),
        log_dir: log_dir.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let resolved = TerminalCommandResolver::new(channel.config().cwd.clone())
        .resolve(
            TerminalCommandTemplate {
                command,
                args,
                stdin: spec.stdin.clone(),
                cwd,
                env,
                timeout_secs: None,
                default_args_to_prompt: false,
                default_timeout_secs: MANAGED_PTY_DEFAULT_TIMEOUT_SECS,
                min_timeout_secs: MANAGED_PTY_MIN_TIMEOUT_SECS,
                max_output_bytes: 0,
            },
            &values,
        )
        .map_err(|err| err.to_string())?;

    Ok(TerminalPtySpawnSpec {
        program: resolved.spec.program,
        args: resolved.spec.args,
        cwd: resolved.spec.cwd,
        env: resolved.spec.env,
        size: parse_pty_size(params),
    })
}

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

/// Open the agent's configured CLI tool in a system terminal window so the
/// user can interact with it directly. Returns details about the spawned
/// terminal — see `agent_terminal_launcher` for platform behavior.
pub(crate) async fn handle_agent_terminal_launch(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .trim()
        .to_owned();
    if agent.is_empty() {
        return Err((INVALID_REQUEST, "missing 'agent' parameter".to_owned()));
    }

    match crate::agent_terminal_launcher::launch_interactive(channel.config(), &agent).await {
        Ok(result) => Ok(json!({
            "launched": true,
            "agent": {
                "id": result.agent_id,
                "name": result.agent_name,
            },
            "terminal": result.terminal,
            "command": result.program,
            "args": result.args,
            "cwd": result.cwd.to_string_lossy(),
            "pid": result.pid,
        })),
        Err(err) => Err((
            INTERNAL_ERROR,
            format!("agent.terminal.launch error: {err}"),
        )),
    }
}

pub(crate) async fn handle_agent_terminal_profile_list() -> RpcResult {
    Ok(json!({
        "profiles": terminal_profile_presets(),
    }))
}

pub(crate) async fn handle_agent_terminal_health(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let agent = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut profile = params
        .get("profile")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("codex")
        .to_owned();
    let mut command = params
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let mut cwd_raw = params
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let mut health_args_override = params
        .get("version_args")
        .or_else(|| params.get("health_check_args"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty());

    if let Some(agent) = agent {
        let (file_stem, raw_config) =
            crate::agent_terminal_delegate::resolve_agent_config(channel.config(), agent)
                .await
                .ok_or_else(|| (INVALID_REQUEST, format!("agent `{agent}` not found")))?;
        let delegate = raw_config.get("terminal").ok_or_else(|| {
            (
                INVALID_REQUEST,
                format!("agent `{agent}` has no terminal configuration"),
            )
        })?;
        let delegate: crate::agent_terminal_delegate::AgentTerminalDelegateConfig =
            serde_json::from_value(delegate.clone()).map_err(|err| {
                (
                    INVALID_REQUEST,
                    format!("invalid terminal configuration for `{file_stem}`: {err}"),
                )
            })?;
        if let Some(value) = delegate
            .profile
            .as_deref()
            .or(delegate.runtime.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            profile = value.to_owned();
        }
        if command.is_none() {
            command = delegate
                .health_check_command
                .as_deref()
                .or(delegate.command.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
        }
        if health_args_override.is_none() {
            health_args_override = delegate
                .health_check_args
                .clone()
                .filter(|items| !items.is_empty());
        }
        if command.is_none() {
            command = delegate
                .command
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
        }
        if cwd_raw.is_none() {
            cwd_raw = delegate
                .cwd
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
        }
    }

    let preset = terminal_profile_preset(&profile);
    let command = command.unwrap_or_else(|| preset.command.to_owned());
    let version_args = health_args_override.unwrap_or_else(|| {
        preset
            .version_args
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect()
    });
    let version_arg_refs = version_args.iter().map(String::as_str).collect::<Vec<_>>();

    let cwd = resolve_cwd(&channel.config().cwd, cwd_raw.as_deref());
    let cwd_exists = cwd.exists();
    let cwd_is_dir = cwd.is_dir();
    let terminal_root = channel.config().savfox_home.join("terminal-agents");
    let terminal_root_writable = path_writable(&terminal_root).await;
    let command = command_health(&command, &version_arg_refs, Duration::from_secs(5)).await;

    Ok(json!({
        "agent": agent,
        "profile": profile,
        "preset": preset,
        "command": command,
        "version_args": version_args,
        "cwd": cwd.to_string_lossy(),
        "cwd_exists": cwd_exists,
        "cwd_is_dir": cwd_is_dir,
        "terminal_root": terminal_root.to_string_lossy(),
        "terminal_root_writable": terminal_root_writable,
    }))
}

pub(crate) async fn handle_agent_terminal_metrics() -> RpcResult {
    Ok(json!({
        "metrics": terminal_runtime_metrics_snapshot(),
    }))
}

pub(crate) async fn handle_agent_terminal_pty_start(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let key = terminal_pty_key_from_params(params).map_err(|message| (INVALID_PARAMS, message))?;
    let spec = resolve_terminal_pty_spawn_spec(params, channel, &key)
        .await
        .map_err(|message| (INVALID_PARAMS, message))?;
    let session = terminal_pty_manager()
        .get_or_spawn(key.clone(), spec)
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("managed PTY start failed: {err}")))?;
    let metadata = session.metadata().await;
    Ok(json!({
        "started": true,
        "agent": key.agent_id,
        "session_id": key.session_id,
        "metadata": metadata,
        "transcript": session.transcript().await,
    }))
}

pub(crate) async fn handle_agent_terminal_pty_write(params: &Value) -> RpcResult {
    let key = terminal_pty_key_from_params(params).map_err(|message| (INVALID_PARAMS, message))?;
    let kind = params
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("line");
    let kind = match kind {
        "text" => TerminalPtyWriteKind::Text,
        "line" | "" => TerminalPtyWriteKind::Line,
        "newline" => TerminalPtyWriteKind::Newline,
        "interrupt" => TerminalPtyWriteKind::Interrupt,
        "control_sequence" | "control" => TerminalPtyWriteKind::ControlSequence,
        "manual_complete" | "complete" => TerminalPtyWriteKind::ManualComplete,
        other => {
            return Err((
                INVALID_PARAMS,
                format!("unsupported managed PTY write kind `{other}`"),
            ));
        }
    };
    let input = TerminalPtyWrite {
        kind,
        text: params
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    };
    terminal_pty_manager()
        .write(&key, input)
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("managed PTY write failed: {err}")))?;
    Ok(json!({
        "written": true,
        "agent": key.agent_id,
        "session_id": key.session_id,
        "metadata": terminal_pty_manager().metadata(&key).await.ok(),
    }))
}

pub(crate) async fn handle_agent_terminal_pty_read(params: &Value) -> RpcResult {
    let key = terminal_pty_key_from_params(params).map_err(|message| (INVALID_PARAMS, message))?;
    let since_sequence = params
        .get("since_sequence")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let wait_for_text = params
        .get("wait_for_text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let timeout_ms = params
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(30_000);

    let entries = if let Some(needle) = wait_for_text {
        terminal_pty_manager()
            .wait_for_text(&key, needle, Duration::from_millis(timeout_ms.max(1)))
            .await
            .map_err(|err| (INTERNAL_ERROR, format!("managed PTY read failed: {err}")))?
            .unwrap_or_default()
    } else {
        terminal_pty_manager()
            .read_transcript(&key)
            .await
            .map_err(|err| (INTERNAL_ERROR, format!("managed PTY read failed: {err}")))?
    };
    let entries = entries
        .into_iter()
        .filter(|entry| entry.sequence > since_sequence)
        .collect::<Vec<_>>();
    Ok(json!({
        "agent": key.agent_id,
        "session_id": key.session_id,
        "entries": entries,
        "metadata": terminal_pty_manager().metadata(&key).await.ok(),
    }))
}

pub(crate) async fn handle_agent_terminal_pty_resize(params: &Value) -> RpcResult {
    let key = terminal_pty_key_from_params(params).map_err(|message| (INVALID_PARAMS, message))?;
    let size = parse_pty_size(params);
    terminal_pty_manager()
        .resize(&key, size)
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("managed PTY resize failed: {err}")))?;
    Ok(json!({
        "resized": true,
        "agent": key.agent_id,
        "session_id": key.session_id,
        "metadata": terminal_pty_manager().metadata(&key).await.ok(),
    }))
}

pub(crate) async fn handle_agent_terminal_pty_close(params: &Value) -> RpcResult {
    let key = terminal_pty_key_from_params(params).map_err(|message| (INVALID_PARAMS, message))?;
    let metadata = terminal_pty_manager()
        .close(&key, TerminalPtyCloseReason::ExplicitClose)
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("managed PTY close failed: {err}")))?;
    Ok(json!({
        "closed": metadata.is_some(),
        "agent": key.agent_id,
        "session_id": key.session_id,
        "metadata": metadata,
    }))
}

pub(crate) async fn handle_agent_terminal_pty_list() -> RpcResult {
    Ok(json!({
        "sessions": terminal_pty_manager().list_metadata().await,
    }))
}

pub(crate) async fn handle_agent_terminal_pty_close_idle() -> RpcResult {
    let closed = terminal_pty_manager().close_idle().await.map_err(|err| {
        (
            INTERNAL_ERROR,
            format!("managed PTY idle cleanup failed: {err}"),
        )
    })?;
    Ok(json!({
        "closed": closed,
    }))
}

pub(crate) async fn handle_agent_terminal_cleanup(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let dry_run = params
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let all = params.get("all").and_then(Value::as_bool).unwrap_or(false);
    let agent = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let session_id = validate_uuid_v7_session_id(
        params
            .get("session_id")
            .or_else(|| params.get("session"))
            .and_then(Value::as_str),
    )
    .map_err(|message| (INVALID_PARAMS, message))?;

    if !all && agent.is_none() && session_id.is_none() {
        return Err((
            INVALID_PARAMS,
            "missing cleanup target: provide agent, session_id, or all=true".to_owned(),
        ));
    }

    let terminal_root = channel.config().savfox_home.join("terminal-agents");
    let targets = terminal_cleanup_targets(&terminal_root, agent, session_id.as_deref(), all)
        .await
        .map_err(|message| (INVALID_PARAMS, message))?;

    let mut cleaned = Vec::new();
    let mut missing = Vec::new();
    let mut errors = Vec::new();
    for target in targets {
        let target_string = target.to_string_lossy().to_string();
        if tokio::fs::metadata(&target).await.is_err() {
            missing.push(target_string);
            continue;
        }
        if dry_run {
            cleaned.push(target_string);
            continue;
        }
        match tokio::fs::remove_dir_all(&target).await {
            Ok(()) => cleaned.push(target_string),
            Err(err) => errors.push(json!({
                "path": target_string,
                "error": err.to_string(),
            })),
        }
    }

    let cleaned_count = cleaned.len();
    let missing_count = missing.len();
    let error_count = errors.len();

    Ok(json!({
        "dry_run": dry_run,
        "cleaned": cleaned,
        "cleaned_count": cleaned_count,
        "missing": missing,
        "missing_count": missing_count,
        "errors": errors,
        "error_count": error_count,
    }))
}

async fn terminal_cleanup_targets(
    terminal_root: &std::path::Path,
    agent: Option<&str>,
    session_id: Option<&str>,
    all: bool,
) -> Result<Vec<PathBuf>, String> {
    if all {
        return list_terminal_session_dirs(terminal_root).await;
    }

    if let Some(agent) = agent {
        let agent_root = safe_join(terminal_root, agent, "")
            .ok_or_else(|| format!("invalid agent identifier `{agent}`"))?;
        let sessions_root = agent_root.join("sessions");
        if let Some(session_id) = session_id {
            return Ok(vec![sessions_root.join(session_id)]);
        }
        return Ok(vec![sessions_root]);
    }

    let Some(session_id) = session_id else {
        return Ok(Vec::new());
    };
    let mut targets = Vec::new();
    let mut agents = match tokio::fs::read_dir(terminal_root).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(targets),
        Err(err) => {
            return Err(format!(
                "failed to list terminal root `{}`: {err}",
                terminal_root.display()
            ));
        }
    };
    while let Some(entry) = agents.next_entry().await.map_err(|err| {
        format!(
            "failed to read terminal root `{}`: {err}",
            terminal_root.display()
        )
    })? {
        let path = entry.path();
        if !entry
            .file_type()
            .await
            .map(|ty| ty.is_dir())
            .unwrap_or(false)
        {
            continue;
        }
        targets.push(path.join("sessions").join(session_id));
    }
    Ok(targets)
}

async fn list_terminal_session_dirs(
    terminal_root: &std::path::Path,
) -> Result<Vec<PathBuf>, String> {
    let mut targets = Vec::new();
    let mut agents = match tokio::fs::read_dir(terminal_root).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(targets),
        Err(err) => {
            return Err(format!(
                "failed to list terminal root `{}`: {err}",
                terminal_root.display()
            ));
        }
    };
    while let Some(agent_entry) = agents.next_entry().await.map_err(|err| {
        format!(
            "failed to read terminal root `{}`: {err}",
            terminal_root.display()
        )
    })? {
        if !agent_entry
            .file_type()
            .await
            .map(|ty| ty.is_dir())
            .unwrap_or(false)
        {
            continue;
        }
        let sessions_root = agent_entry.path().join("sessions");
        let mut sessions = match tokio::fs::read_dir(&sessions_root).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(format!(
                    "failed to list terminal sessions `{}`: {err}",
                    sessions_root.display()
                ));
            }
        };
        while let Some(session_entry) = sessions.next_entry().await.map_err(|err| {
            format!(
                "failed to read terminal sessions `{}`: {err}",
                sessions_root.display()
            )
        })? {
            if session_entry
                .file_type()
                .await
                .map(|ty| ty.is_dir())
                .unwrap_or(false)
            {
                targets.push(session_entry.path());
            }
        }
    }
    Ok(targets)
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
    let agents_dir = channel.config().savfox_home.join(AGENTS_SUBDIR);
    let Some(agent_config_path) = safe_join(&agents_dir, agent_id, ".json") else {
        return Err((INVALID_PARAMS, "invalid agent identifier".to_owned()));
    };
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
    channel.config().savfox_home.join(AGENTS_SUBDIR)
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
        .map(str::to_owned)
        .or_else(|| {
            config
                .get("identity")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| fallback.to_owned())
}

fn default_agent_stub() -> Value {
    json!({
        "id": "default",
        "name": "Savvy fox",
        "kind": "native",
        "native": {
            "provider": "default",
            "model": "default"
        },
        "description": "Default Savfox assistant agent",
        "builtin": true,
        "status": "active",
    })
}

fn trimmed_string_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn legacy_primary_model(config: &Value) -> Option<String> {
    trimmed_string_at(config, "model")
        .or_else(|| {
            config
                .get("models")
                .and_then(|models| trimmed_string_at(models, "primary"))
        })
        .map(str::to_owned)
}

fn provider_for_model(model: &str) -> String {
    model
        .split_once('/')
        .map(|(provider, _)| provider.trim())
        .filter(|provider| !provider.is_empty())
        .unwrap_or("default")
        .to_owned()
}

fn legacy_fallback_models(config: &Value) -> Option<Value> {
    let fallbacks = config
        .get("models")
        .and_then(|models| {
            models
                .get("fallback_models")
                .or_else(|| models.get("fallbacks"))
                .or_else(|| models.get("fallback"))
        })
        .or_else(|| config.get("fallback_models"))?;

    match fallbacks {
        Value::Array(items) => {
            let models: Vec<_> = items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(|model| json!(model))
                .collect();
            (!models.is_empty()).then_some(Value::Array(models))
        }
        Value::String(model) => {
            let model = model.trim();
            (!model.is_empty()).then(|| json!([model]))
        }
        _ => None,
    }
}

fn normalize_native_agent_config(config: &mut Value) {
    let legacy_model = legacy_primary_model(config);
    let legacy_fallbacks = legacy_fallback_models(config);

    if !config.get("native").is_some_and(Value::is_object) {
        let model = legacy_model.clone().unwrap_or_else(|| "default".to_owned());
        config["native"] = json!({
            "provider": provider_for_model(&model),
            "model": model,
        });
    }

    if let Some(native) = config.get_mut("native").and_then(Value::as_object_mut) {
        let model = native
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| legacy_model.clone())
            .unwrap_or_else(|| "default".to_owned());
        native.insert("model".to_owned(), json!(model));

        let provider_missing = native
            .get("provider")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_none_or(|provider| provider.is_empty());
        if provider_missing {
            native.insert("provider".to_owned(), json!(provider_for_model(&model)));
        }

        if !native.contains_key("fallback_models")
            && let Some(fallbacks) = legacy_fallbacks
        {
            native.insert("fallback_models".to_owned(), fallbacks);
        }
    }
}

fn has_enabled_terminal_config(config: &Value) -> bool {
    let terminal = config
        .get("terminal")
        .or_else(|| config.get("terminal_delegate"));
    terminal.is_some_and(|terminal| {
        terminal.get("enabled").and_then(Value::as_bool) == Some(true)
            && trimmed_string_at(terminal, "command").is_some()
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

    if !config.get("terminal").is_some_and(Value::is_object)
        && let Some(terminal_delegate) = config.get("terminal_delegate").cloned()
        && terminal_delegate.is_object()
    {
        config["terminal"] = terminal_delegate;
    }

    let kind = config
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| matches!(*value, "native" | "terminal"))
        .map(ToOwned::to_owned)
        .or_else(|| has_enabled_terminal_config(config).then(|| "terminal".to_owned()))
        .unwrap_or_else(|| "native".to_owned());
    config["kind"] = json!(kind.as_str());
    if kind == "native" {
        normalize_native_agent_config(config);
    } else if kind == "terminal"
        && let Some(terminal) = config.get_mut("terminal").and_then(Value::as_object_mut)
        && !terminal.contains_key("runtime")
    {
        terminal.insert("runtime".to_owned(), json!("codex"));
    }

    if builtin
        && config
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "native")
        && !config.get("native").is_some_and(Value::is_object)
    {
        normalize_native_agent_config(config);
    }

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

fn required_non_empty_string<'a>(
    object: &'a Value,
    path: &'static str,
) -> std::result::Result<&'a str, String> {
    object
        .as_object()
        .and_then(|object| object.get(path))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing `{path}`"))
}

fn validate_agent_shape(config: &Value) -> std::result::Result<(), String> {
    let kind = config
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "missing `kind`".to_owned())?;
    match kind {
        "native" => {
            let native = config
                .get("native")
                .filter(|value| value.is_object())
                .ok_or_else(|| "missing `native` configuration for native agent".to_owned())?;
            required_non_empty_string(native, "provider")
                .map_err(|err| format!("invalid native agent: {err}"))?;
            required_non_empty_string(native, "model")
                .map_err(|err| format!("invalid native agent: {err}"))?;
        }
        "terminal" => {
            let terminal = config
                .get("terminal")
                .filter(|value| value.is_object())
                .ok_or_else(|| "missing `terminal` configuration for terminal agent".to_owned())?;
            let runtime = required_non_empty_string(terminal, "runtime")
                .map_err(|err| format!("invalid terminal agent: {err}"))?;
            if !matches!(runtime, "codex" | "claude") {
                return Err(
                    "invalid terminal agent: `terminal.runtime` must be `codex` or `claude`"
                        .to_owned(),
                );
            }
            if terminal.get("enabled").and_then(Value::as_bool) != Some(true) {
                return Err("invalid terminal agent: terminal agents must be enabled".to_owned());
            }
            required_non_empty_string(terminal, "command")
                .map_err(|err| format!("invalid terminal agent: {err}"))?;
        }
        _ => return Err("invalid `kind`; expected `native` or `terminal`".to_owned()),
    }

    if let Some(permission_policy) = config
        .get("permission_policy")
        .filter(|permission_policy| !permission_policy.is_null())
    {
        serde_json::from_value::<AgentPermissionPolicy>(permission_policy.clone())
            .map_err(|err| format!("invalid `permission_policy`: {err}"))?;
    }

    if let Some(execution_policy) = config
        .get("execution_policy")
        .filter(|execution_policy| !execution_policy.is_null())
    {
        serde_json::from_value::<AgentExecutionPolicy>(execution_policy.clone())
            .map_err(|err| format!("invalid `execution_policy`: {err}"))?;
    }

    Ok(())
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

async fn attach_effective_security(channel: &GatewayChannel, agent: &mut Value) {
    let agent_id = agent.get("id").and_then(Value::as_str).unwrap_or("default");
    let resolved = resolve_execution_security(
        channel.config(),
        &channel.config().savfox_home,
        agent_id,
        ApprovalClientCapabilities::interactive(),
    )
    .await;
    agent["effective_security"] = json!({
        "execution_mode": resolved.context.mode.as_str(),
        "sandbox_policy": resolved.context.effective_sandbox,
        "sandbox_enforcement": resolved.context.sandbox_enforcement,
        "policy_fingerprint": resolved.context.policy_fingerprint,
        "fallback_reason": resolved.context.fallback_reason,
    });
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
            .map(str::to_owned)
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

    if let Some(safe_ref) = sanitize_agent_file_stem(trimmed)
        && let Some(direct) = safe_join(&dir, &safe_ref, ".json")
        && direct.exists()
    {
        return Some(safe_ref);
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
        attach_effective_security(channel, &mut agent).await;
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
        let mut config = load_default_agent_config(channel).await;
        attach_effective_security(channel, &mut config).await;
        return Ok(config);
    }

    let Some(file_stem) = resolve_agent_file_stem(channel, agent_ref).await else {
        return Err((INVALID_REQUEST, format!("agent not found: {agent_ref}")));
    };
    if file_stem.eq_ignore_ascii_case("default") {
        let mut config = load_default_agent_config(channel).await;
        attach_effective_security(channel, &mut config).await;
        return Ok(config);
    }
    let Some(path) = safe_join(&agents_dir(channel), &file_stem, ".json") else {
        return Err((INVALID_REQUEST, format!("agent not found: {agent_ref}")));
    };
    let Some(mut config) = read_agent_config(&path).await else {
        return Err((INVALID_REQUEST, format!("agent not found: {agent_ref}")));
    };

    normalize_agent_config(&mut config, &file_stem, false);
    attach_effective_security(channel, &mut config).await;

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
        .map(str::to_owned)
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
    let system_prompt = params
        .get("system_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let kind = params
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| matches!(*value, "native" | "terminal"))
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                "missing or invalid 'kind' parameter; expected 'native' or 'terminal'".to_owned(),
            )
        })?;

    let mut agent_config = json!({
        "id": id,
        "name": name,
        "kind": kind,
        "description": description,
        "system_prompt": system_prompt,
        "created_at": chrono::Utc::now().to_rfc3339(),
    });

    if kind == "terminal" {
        let terminal = params.get("terminal").ok_or_else(|| {
            (
                INVALID_REQUEST,
                "missing 'terminal' configuration for terminal agent".to_owned(),
            )
        })?;
        agent_config["terminal"] = terminal.clone();
    } else {
        let native = params.get("native").ok_or_else(|| {
            (
                INVALID_REQUEST,
                "missing 'native' configuration for native agent".to_owned(),
            )
        })?;
        agent_config["native"] = native.clone();
    }
    validate_agent_shape(&agent_config).map_err(|message| (INVALID_REQUEST, message))?;

    // Per-agent config overrides
    for key in &[
        "memory",
        "compaction",
        "sandbox",
        "heartbeat",
        "group_activation",
        "channel_replies",
        "group_keywords",
        "agent_aliases",
        "ingest_policy",
        "external_bot_policy",
        "idle_reply",
        "dm_scope",
        "identity",
        "permission_policy",
        "execution_policy",
        "matrix_auto_user_channels",
    ] {
        if let Some(val) = params.get(*key) {
            agent_config[*key] = val.clone();
        }
    }

    let dir = agents_dir(channel);
    let _ = tokio::fs::create_dir_all(&dir).await;
    let Some(path) = safe_join(&dir, &id, ".json") else {
        return Err((INVALID_PARAMS, format!("invalid agent id: {id}")));
    };
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
    let Some(path) = safe_join(&dir, &resolved_id, ".json") else {
        return Err((
            INVALID_REQUEST,
            format!("invalid agent reference: {agent_ref}"),
        ));
    };
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
    if let Some(prompt) = params.get("system_prompt").and_then(|v| v.as_str()) {
        config["system_prompt"] = json!(prompt);
    }
    if let Some(kind_value) = params.get("kind") {
        let kind = kind_value
            .as_str()
            .map(str::trim)
            .filter(|value| matches!(*value, "native" | "terminal"))
            .ok_or_else(|| {
                (
                    INVALID_REQUEST,
                    "invalid 'kind' parameter; expected 'native' or 'terminal'".to_owned(),
                )
            })?;
        config["kind"] = json!(kind);
        if kind == "native" {
            if let Some(object) = config.as_object_mut() {
                object.remove("terminal");
            }
        } else {
            if let Some(object) = config.as_object_mut() {
                object.remove("native");
            }
        }
    }
    if let Some(native) = params.get("native") {
        config["native"] = native.clone();
    }
    if let Some(terminal) = params.get("terminal") {
        config["terminal"] = terminal.clone();
    }
    // Per-agent config overrides
    for key in &[
        "memory",
        "compaction",
        "sandbox",
        "heartbeat",
        "group_activation",
        "channel_replies",
        "group_keywords",
        "agent_aliases",
        "ingest_policy",
        "external_bot_policy",
        "idle_reply",
        "dm_scope",
        "identity",
        "permission_policy",
        "execution_policy",
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
    validate_agent_shape(&config).map_err(|message| (INVALID_REQUEST, message))?;

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
    let Some(path) = safe_join(&dir, &resolved_id, ".json") else {
        return Err((
            INVALID_REQUEST,
            format!("invalid agent reference: {agent_ref}"),
        ));
    };
    let _ = tokio::fs::remove_file(&path).await;

    // Also remove the agent's files directory. `resolved_id` is already
    // sanitized but we re-validate via safe_join with no suffix so a
    // malformed value cannot escape `dir`.
    if let Some(files_dir) = safe_join(&dir, &resolved_id, "") {
        let _ = tokio::fs::remove_dir_all(&files_dir).await;
    }
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

    let Some(path) = safe_join(&dir, &resolved_id, ".json") else {
        return Err((
            INVALID_REQUEST,
            format!("invalid agent reference: {agent_ref}"),
        ));
    };

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

    // Sanitize path to prevent directory traversal. Reject (rather than
    // silently rewrite) anything that isn't a single safe filename segment —
    // the previous `file_name().unwrap_or(file_path)` fell back to the raw,
    // unsanitized value for inputs like `..`, `foo/..`, or an absolute path.
    let Some(safe_name) = crate::security::path_safety::safe_filename_segment(file_path) else {
        return Err((INVALID_PARAMS, "invalid 'path' parameter".to_owned()));
    };

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

    // Sanitize path to prevent directory traversal. Reject (rather than
    // silently rewrite) anything that isn't a single safe filename segment —
    // the previous `file_name().unwrap_or(file_path)` fell back to the raw,
    // unsanitized value for inputs like `..`, `foo/..`, or an absolute path.
    let Some(safe_name) = crate::security::path_safety::safe_filename_segment(file_path) else {
        return Err((INVALID_PARAMS, "invalid 'path' parameter".to_owned()));
    };

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

    // Sanitize path to prevent directory traversal. Reject (rather than
    // silently rewrite) anything that isn't a single safe filename segment —
    // the previous `file_name().unwrap_or(file_path)` fell back to the raw,
    // unsanitized value for inputs like `..`, `foo/..`, or an absolute path.
    let Some(safe_name) = crate::security::path_safety::safe_filename_segment(file_path) else {
        return Err((INVALID_PARAMS, "invalid 'path' parameter".to_owned()));
    };

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
            let Some(path) = safe_join(&agents_dir(channel), &file_stem, ".json") else {
                return Err((INVALID_REQUEST, format!("agent not found: {agent_ref}")));
            };
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
    let Some(path) = safe_join(&dir, &resolved_id, ".json") else {
        return Err((
            INVALID_REQUEST,
            format!("invalid agent reference: {agent_ref}"),
        ));
    };

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

    let Some(config_path) = safe_join(&dir, agent_id, ".json") else {
        return Err((INVALID_PARAMS, format!("invalid agent id: {agent_id}")));
    };
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

    let Some(config_path) = safe_join(&agents_dir(channel), agent_id, ".json") else {
        return Err((INVALID_PARAMS, format!("invalid agent id: {agent_id}")));
    };
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        normalize_agent_config, parse_pty_size, parse_string_array, parse_string_map,
        terminal_cleanup_targets, terminal_pty_key_from_params, validate_agent_shape,
    };

    fn assert_agents_response_deserializes(config: &serde_json::Value) {
        let response: savfox_gateway_shared::AgentsResponse =
            serde_json::from_value(json!({ "agents": [config.clone()] }))
                .expect("normalized agent should match frontend wire type");
        assert_eq!(response.agents.len(), 1);
    }

    #[test]
    fn normalize_agent_config_fills_legacy_native_defaults() {
        let mut config = json!({
            "id": "019cad1c-d58e-7e20-a16e-dd1b1f0d382f",
            "name": "sdd",
            "status": "active",
            "updated_at": "2026-03-09T12:54:35.359578700+00:00"
        });

        normalize_agent_config(&mut config, "019cad1c-d58e-7e20-a16e-dd1b1f0d382f", false);

        assert_eq!(config["kind"], json!("native"));
        assert_eq!(config["native"]["provider"], json!("default"));
        assert_eq!(config["native"]["model"], json!("default"));
        assert_agents_response_deserializes(&config);
    }

    #[test]
    fn agent_shape_rejects_malformed_permission_policy() {
        let config = json!({
            "kind": "native",
            "native": {
                "provider": "default",
                "model": "default"
            },
            "permission_policy": {
                "sandbox": "workspace-write",
                "approval": "on-request",
                "tool_access": {
                    "allowed": "shell"
                }
            }
        });

        let error = validate_agent_shape(&config).expect_err("invalid tool list should fail");
        assert!(error.starts_with("invalid `permission_policy`:"));
    }

    #[test]
    fn normalize_agent_config_migrates_legacy_model_fields() {
        let mut config = json!({
            "id": "default",
            "name": "Savfox Agent",
            "description": "Default Savfox assistant agent",
            "builtin": true,
            "status": "active",
            "is_default": true,
            "model": "deepseek-chris/deepseek-v4-flash",
            "models": {
                "primary": "deepseek-chris/deepseek-v4-flash",
                "fallback": ["openai/gpt-5-mini"]
            }
        });

        normalize_agent_config(&mut config, "default", true);

        assert_eq!(config["kind"], json!("native"));
        assert_eq!(config["native"]["provider"], json!("deepseek-chris"));
        assert_eq!(
            config["native"]["model"],
            json!("deepseek-chris/deepseek-v4-flash")
        );
        assert_eq!(
            config["native"]["fallback_models"],
            json!(["openai/gpt-5-mini"])
        );
        assert_eq!(config["builtin"], json!(true));
        assert_agents_response_deserializes(&config);
    }

    #[tokio::test]
    async fn terminal_cleanup_targets_can_find_session_across_agents() {
        let root = tempfile::tempdir().expect("create temp dir");
        let session_id = "018f0000-0000-7000-8000-000000000701";
        let alpha = root.path().join("alpha").join("sessions").join(session_id);
        let beta = root.path().join("beta").join("sessions").join(session_id);
        tokio::fs::create_dir_all(&alpha)
            .await
            .expect("create alpha session");
        tokio::fs::create_dir_all(&beta)
            .await
            .expect("create beta session");

        let mut targets = terminal_cleanup_targets(root.path(), None, Some(session_id), false)
            .await
            .expect("plan cleanup targets");
        targets.sort();

        assert_eq!(targets, vec![alpha, beta]);
    }

    #[tokio::test]
    async fn terminal_cleanup_targets_rejects_invalid_agent() {
        let root = tempfile::tempdir().expect("create temp dir");

        let err = terminal_cleanup_targets(root.path(), Some("../agent"), None, false)
            .await
            .expect_err("invalid agent should be rejected");

        assert!(err.contains("invalid agent identifier"));
    }

    #[test]
    fn managed_pty_rpc_parses_size_arrays_maps_and_uuid_aliases() {
        let size = parse_pty_size(&json!({
            "columns": 140,
            "rows": 42
        }));
        assert_eq!(size.cols, 140);
        assert_eq!(size.rows, 42);

        let defaulted_size = parse_pty_size(&json!({
            "cols": 10,
            "rows": 2
        }));
        assert_eq!(defaulted_size.cols, 120);
        assert_eq!(defaulted_size.rows, 30);

        assert_eq!(
            parse_string_array(Some(&json!([" run ", "", 1, "now"]))),
            Some(vec!["run".to_owned(), "now".to_owned()])
        );
        assert_eq!(
            parse_string_map(Some(&json!({
                " FOO ": "bar",
                "COUNT": 3,
                "": "skip"
            }))),
            std::collections::BTreeMap::from([
                ("COUNT".to_owned(), String::new()),
                ("FOO".to_owned(), "bar".to_owned()),
            ])
        );

        let key = terminal_pty_key_from_params(&json!({
            "agent_id": "codex",
            "session": "018f0000-0000-7000-8000-000000000701"
        }))
        .expect("valid UUID v7 session should parse");
        assert_eq!(key.agent_id, "codex");
        assert_eq!(key.session_id, "018f0000-0000-7000-8000-000000000701");
        assert!(
            terminal_pty_key_from_params(&json!({
                "agent": "codex",
                "session_id": "not-a-uuid"
            }))
            .is_err()
        );
    }
}
