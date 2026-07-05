use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use savfox_core::config::Config;
use savfox_gateway_shared::ChatAttachment;
use savfox_protocol::SessionId;
use savfox_protocol::protocol::{
    AgentMessageEvent, EventMsg, RolloutItem, RolloutLine, SessionMeta, SessionMetaLine,
    SessionModel, SessionSource, UserMessageEvent,
};
use savfox_utils::home_dir::AGENTS_SUBDIR;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tracing::warn;
use uuid::Uuid;

use crate::channel::{AgentInvocationResult, GatewayChannel};
use crate::terminal_agent::{
    TerminalAgentEvent, TerminalCommandResolver, TerminalCommandTemplate, TerminalExitReason,
    TerminalOutputCallback, TerminalSupervisor, TerminalSupervisorResult, TerminalTemplateValues,
    TerminalWorkspaceManager, TerminalWorkspaceRequest, TerminalWorkspaceState,
    parse_terminal_output, record_terminal_runtime_metrics, render_terminal_template, resolve_cwd,
};

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MIN_TIMEOUT_SECS: u64 = 5;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const TERMINAL_HISTORY_LIMIT: usize = 20;
const MAX_TERMINAL_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;

pub(crate) type TerminalEventSink = Arc<dyn Fn(TerminalAgentEvent) + Send + Sync>;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AgentTerminalDelegateConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) profile: Option<String>,
    #[serde(default)]
    pub(crate) mode: Option<String>,
    #[serde(default)]
    pub(crate) session_scope: Option<String>,
    #[serde(default)]
    pub(crate) io_protocol: Option<String>,
    #[serde(default)]
    pub(crate) health_check_command: Option<String>,
    #[serde(default)]
    pub(crate) health_check_args: Option<Vec<String>>,
    pub(crate) command: Option<String>,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    pub(crate) stdin: Option<String>,
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) timeout_secs: Option<u64>,
    #[serde(default)]
    pub(crate) cleanup_policy: Option<String>,
    #[serde(default = "default_include_system_prompt")]
    pub(crate) include_system_prompt: bool,
    #[serde(default)]
    pub(crate) interactive_command: Option<String>,
    #[serde(default)]
    pub(crate) interactive_args: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) workspace: AgentTerminalWorkspaceConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct AgentTerminalWorkspaceConfig {
    #[serde(default)]
    pub(crate) mode: Option<String>,
    #[serde(default)]
    pub(crate) base: Option<String>,
    #[serde(default)]
    pub(crate) cleanup_policy: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedTerminalDelegate {
    config: AgentTerminalDelegateConfig,
    agent_id: String,
    agent_name: String,
    system_prompt: String,
}

#[derive(Debug, Clone)]
pub(crate) struct TerminalDelegateInvocation {
    pub(crate) result: AgentInvocationResult,
    pub(crate) model: String,
    pub(crate) provider: String,
    pub(crate) terminal: TerminalInvocationMetadata,
}

#[derive(Debug, Clone)]
struct TerminalSessionContext {
    session_id: String,
    root_dir: PathBuf,
    agent_home: PathBuf,
    workspace_dir: PathBuf,
    log_dir: PathBuf,
    metadata_path: PathBuf,
    created_at: String,
}

#[derive(Debug, Clone)]
struct TerminalSessionMetadataState {
    status: String,
    pid: Option<u32>,
    command: Option<String>,
    cwd: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    exit_code: Option<i32>,
    error: Option<String>,
    rollout_path: Option<String>,
    cleanup_status: Option<String>,
    cleanup_error: Option<String>,
    workspace_mode: Option<String>,
    workspace_base: Option<String>,
    workspace_path: Option<String>,
    workspace_cleanup_status: Option<String>,
    workspace_patch_path: Option<String>,
    workspace_diff_summary_path: Option<String>,
    workspace_diff_summary: Option<String>,
}

#[derive(Debug, Serialize)]
struct TerminalSessionMetadata<'a> {
    agent_id: &'a str,
    agent_name: &'a str,
    session_id: &'a str,
    profile: &'a str,
    mode: &'a str,
    session_scope: &'a str,
    io_protocol: &'a str,
    status: &'a str,
    pid: Option<u32>,
    command: Option<&'a str>,
    cwd: Option<&'a str>,
    root_dir: String,
    agent_home: String,
    workspace_dir: String,
    log_dir: String,
    metadata_path: String,
    created_at: &'a str,
    started_at: Option<&'a str>,
    completed_at: Option<&'a str>,
    exit_code: Option<i32>,
    error: Option<&'a str>,
    rollout_path: Option<&'a str>,
    cleanup_policy: &'a str,
    cleanup_status: Option<&'a str>,
    cleanup_error: Option<&'a str>,
    workspace_mode: Option<&'a str>,
    workspace_base: Option<&'a str>,
    workspace_path: Option<&'a str>,
    workspace_cleanup_status: Option<&'a str>,
    workspace_patch_path: Option<&'a str>,
    workspace_diff_summary_path: Option<&'a str>,
    workspace_diff_summary: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TerminalAttachmentMetadata {
    pub(crate) filename: String,
    pub(crate) mime_type: String,
    pub(crate) path: String,
    pub(crate) size_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
struct TerminalContextMessage {
    role: String,
    text: String,
}

#[derive(Debug, Clone)]
struct TerminalRunResult {
    reply: String,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    pid: Option<u32>,
    exit_code: Option<i32>,
    metadata_state: TerminalSessionMetadataState,
    events: Vec<TerminalAgentEvent>,
    attachments: Vec<TerminalAttachmentMetadata>,
}

#[derive(Debug, Clone)]
pub(crate) struct TerminalInvocationMetadata {
    pub(crate) agent_id: String,
    pub(crate) agent_name: String,
    pub(crate) profile: String,
    pub(crate) mode: String,
    pub(crate) session_scope: String,
    pub(crate) io_protocol: String,
    pub(crate) workspace_dir: PathBuf,
    pub(crate) log_dir: PathBuf,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) stdout_truncated: bool,
    pub(crate) stderr_truncated: bool,
    pub(crate) pid: Option<u32>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) events: Vec<TerminalAgentEvent>,
    pub(crate) attachments: Vec<TerminalAttachmentMetadata>,
}

fn default_include_system_prompt() -> bool {
    true
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

async fn read_agent_config(path: &Path) -> Option<serde_json::Value> {
    let data = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str(&data).ok()
}

fn agents_dir(config: &Config) -> PathBuf {
    config.savfox_home.join(AGENTS_SUBDIR)
}

pub(crate) async fn resolve_agent_config(
    config: &Config,
    agent_ref: &str,
) -> Option<(String, serde_json::Value)> {
    let dir = agents_dir(config);
    let trimmed = agent_ref.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(safe_ref) = sanitize_agent_file_stem(trimmed) {
        let direct = dir.join(format!("{safe_ref}.json"));
        if let Some(config) = read_agent_config(&direct).await {
            return Some((safe_ref, config));
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
            .and_then(|value| value.to_str())
            .map(str::to_owned)
        else {
            continue;
        };

        if stem.eq_ignore_ascii_case(trimmed)
            && let Some(config) = read_agent_config(&path).await
        {
            return Some((stem, config));
        }

        let Some(config) = read_agent_config(&path).await else {
            continue;
        };

        let id_match = config
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|id| id.eq_ignore_ascii_case(trimmed));
        let name_match = config
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|name| name.eq_ignore_ascii_case(trimmed));
        let identity_match = config
            .get("identity")
            .and_then(|value| value.get("name"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|name| name.eq_ignore_ascii_case(trimmed));

        if id_match || name_match || identity_match {
            return Some((stem, config));
        }
    }

    None
}

fn terminal_delegate_from_agent_config(
    file_stem: String,
    config: serde_json::Value,
) -> Option<ResolvedTerminalDelegate> {
    let delegate_value = config.get("terminal_delegate")?;
    let delegate: AgentTerminalDelegateConfig =
        serde_json::from_value(delegate_value.clone()).ok()?;
    if !delegate.enabled {
        return None;
    }
    if delegate
        .command
        .as_deref()
        .map(str::trim)
        .is_none_or(|value| value.is_empty())
    {
        return None;
    }

    let agent_id = config
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(file_stem.as_str())
        .to_owned();
    let agent_name = config
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(agent_id.as_str())
        .to_owned();
    let system_prompt = config
        .get("system_prompt")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_owned();

    Some(ResolvedTerminalDelegate {
        config: delegate,
        agent_id,
        agent_name,
        system_prompt,
    })
}

fn render_template(template: &str, values: &TerminalTemplateValues) -> String {
    render_terminal_template(template, values)
}

fn build_prompt_values(
    delegate: &ResolvedTerminalDelegate,
    prompt: &str,
    model: &str,
    terminal_context: &TerminalSessionContext,
    history: &[TerminalContextMessage],
    attachments: &[TerminalAttachmentMetadata],
) -> TerminalTemplateValues {
    build_prompt_values_with_workspace(
        delegate,
        prompt,
        model,
        terminal_context,
        &terminal_context.workspace_dir,
        history,
        attachments,
    )
}

fn build_prompt_values_with_workspace(
    delegate: &ResolvedTerminalDelegate,
    prompt: &str,
    model: &str,
    terminal_context: &TerminalSessionContext,
    workspace_dir: &Path,
    history: &[TerminalContextMessage],
    attachments: &[TerminalAttachmentMetadata],
) -> TerminalTemplateValues {
    let conversation_context = format_conversation_context(history);
    let attachment_manifest = format_attachment_manifest(attachments);
    let terminal_input_json = format_terminal_input_json(
        delegate,
        prompt,
        model,
        terminal_context,
        workspace_dir,
        history,
        attachments,
    );
    let full_prompt = format_terminal_full_prompt(
        delegate,
        prompt,
        terminal_context,
        &conversation_context,
        &attachment_manifest,
        &terminal_input_json,
    );

    TerminalTemplateValues {
        prompt: prompt.to_owned(),
        full_prompt,
        system_prompt: delegate.system_prompt.clone(),
        conversation_context,
        attachment_manifest,
        terminal_input_json,
        agent_id: delegate.agent_id.clone(),
        agent_name: delegate.agent_name.clone(),
        model: model.to_owned(),
        session_id: terminal_context.session_id.clone(),
        agent_home: terminal_context.agent_home.to_string_lossy().into_owned(),
        workspace_dir: workspace_dir.to_string_lossy().into_owned(),
        log_dir: terminal_context.log_dir.to_string_lossy().into_owned(),
    }
}

fn format_conversation_context(history: &[TerminalContextMessage]) -> String {
    if history.is_empty() {
        return "No previous turns in this Savfox session.".to_owned();
    }

    let mut out = String::new();
    for (index, message) in history.iter().enumerate() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!(
            "{}. {}:\n{}",
            index + 1,
            message.role,
            message.text.trim()
        ));
    }
    out
}

fn format_attachment_manifest(attachments: &[TerminalAttachmentMetadata]) -> String {
    if attachments.is_empty() {
        return "No attachments for this turn.".to_owned();
    }

    attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| {
            format!(
                "{}. {} ({}, {} bytes)\n   path: {}",
                index + 1,
                attachment.filename,
                attachment.mime_type,
                attachment.size_bytes,
                attachment.path
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_terminal_input_json(
    delegate: &ResolvedTerminalDelegate,
    prompt: &str,
    model: &str,
    terminal_context: &TerminalSessionContext,
    workspace_dir: &Path,
    history: &[TerminalContextMessage],
    attachments: &[TerminalAttachmentMetadata],
) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "session_id": terminal_context.session_id,
        "agent": {
            "id": delegate.agent_id,
            "name": delegate.agent_name,
            "profile": configured_profile(delegate),
            "mode": configured_mode(delegate),
        },
        "model": model,
        "current_user_request": prompt,
        "system_prompt": delegate.system_prompt,
        "workspace_dir": workspace_dir.to_string_lossy(),
        "conversation": history,
        "attachments": attachments,
    }))
    .unwrap_or_else(|_| "{}".to_owned())
}

fn format_terminal_full_prompt(
    delegate: &ResolvedTerminalDelegate,
    prompt: &str,
    terminal_context: &TerminalSessionContext,
    conversation_context: &str,
    attachment_manifest: &str,
    terminal_input_json: &str,
) -> String {
    let mut sections = Vec::new();
    if delegate.config.include_system_prompt && !delegate.system_prompt.trim().is_empty() {
        sections.push(format!(
            "System instructions:\n{}",
            delegate.system_prompt.trim()
        ));
    }
    sections.push(format!(
        "Savfox terminal session:\n- session_id: {}\n- agent_id: {}\n- agent_name: {}",
        terminal_context.session_id, delegate.agent_id, delegate.agent_name
    ));
    sections.push(format!("Previous conversation:\n{conversation_context}"));
    sections.push(format!("Current user request:\n{}", prompt.trim()));
    sections.push(format!(
        "Attachments:\n{attachment_manifest}\n\nUse the listed local file paths when you need to inspect attached images."
    ));
    sections.push(format!(
        "Structured input JSON:\n```json\n{terminal_input_json}\n```"
    ));
    sections.join("\n\n")
}

fn configured_mode(delegate: &ResolvedTerminalDelegate) -> &str {
    delegate
        .config
        .mode
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("one_shot")
}

fn configured_profile(delegate: &ResolvedTerminalDelegate) -> &str {
    delegate
        .config
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("custom")
}

fn configured_session_scope(delegate: &ResolvedTerminalDelegate) -> &str {
    delegate
        .config
        .session_scope
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("per_session")
}

fn configured_cleanup_policy(delegate: &ResolvedTerminalDelegate) -> &str {
    delegate
        .config
        .cleanup_policy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| match configured_session_scope(delegate) {
            "per_turn" => "per_turn",
            "manual" => "manual",
            _ => "per_session",
        })
}

fn configured_workspace_mode(delegate: &ResolvedTerminalDelegate) -> &str {
    delegate
        .config
        .workspace
        .mode
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("shared")
}

fn configured_workspace_cleanup_policy(delegate: &ResolvedTerminalDelegate) -> String {
    if let Some(policy) = delegate
        .config
        .workspace
        .cleanup_policy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return policy.to_owned();
    }
    match configured_cleanup_policy(delegate) {
        "per_turn" => "delete_on_success".to_owned(),
        _ => "preserve".to_owned(),
    }
}

fn configured_io_protocol(delegate: &ResolvedTerminalDelegate) -> &str {
    delegate
        .config
        .io_protocol
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("plain_text")
}

fn validate_terminal_session_id(raw: &str) -> anyhow::Result<String> {
    let value = raw.trim();
    let parsed = Uuid::parse_str(value)
        .map_err(|err| anyhow::anyhow!("invalid session_id `{value}`: {err}"))?;
    if parsed.get_version_num() != 7 {
        return Err(anyhow::anyhow!(
            "invalid session_id `{value}`: UUID v7 required"
        ));
    }
    Ok(parsed.to_string())
}

fn normalize_terminal_session_id(session_id: Option<&str>) -> anyhow::Result<String> {
    match session_id.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => validate_terminal_session_id(value),
        None => Ok(SessionId::new().to_string()),
    }
}

impl TerminalSessionMetadataState {
    fn new(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            pid: None,
            command: None,
            cwd: None,
            started_at: None,
            completed_at: None,
            exit_code: None,
            error: None,
            rollout_path: None,
            cleanup_status: None,
            cleanup_error: None,
            workspace_mode: None,
            workspace_base: None,
            workspace_path: None,
            workspace_cleanup_status: None,
            workspace_patch_path: None,
            workspace_diff_summary_path: None,
            workspace_diff_summary: None,
        }
    }

    fn record_workspace(&mut self, workspace: &TerminalWorkspaceState) {
        self.cwd = Some(workspace.command_cwd.to_string_lossy().into_owned());
        self.workspace_mode = Some(workspace.mode.clone());
        self.workspace_base = Some(workspace.base_cwd.to_string_lossy().into_owned());
        self.workspace_path = Some(workspace.workspace_path.to_string_lossy().into_owned());
        self.workspace_cleanup_status = workspace.cleanup_status.clone();
        self.workspace_patch_path = workspace
            .patch_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        self.workspace_diff_summary_path = workspace
            .diff_summary_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        self.workspace_diff_summary = workspace.diff_summary.clone();
    }
}

async fn write_terminal_session_metadata(
    delegate: &ResolvedTerminalDelegate,
    context: &TerminalSessionContext,
    state: &TerminalSessionMetadataState,
) -> std::io::Result<()> {
    let metadata = TerminalSessionMetadata {
        agent_id: &delegate.agent_id,
        agent_name: &delegate.agent_name,
        session_id: &context.session_id,
        profile: configured_profile(delegate),
        mode: configured_mode(delegate),
        session_scope: configured_session_scope(delegate),
        io_protocol: configured_io_protocol(delegate),
        status: &state.status,
        pid: state.pid,
        command: state.command.as_deref(),
        cwd: state.cwd.as_deref(),
        root_dir: context.root_dir.to_string_lossy().into_owned(),
        agent_home: context.agent_home.to_string_lossy().into_owned(),
        workspace_dir: context.workspace_dir.to_string_lossy().into_owned(),
        log_dir: context.log_dir.to_string_lossy().into_owned(),
        metadata_path: context.metadata_path.to_string_lossy().into_owned(),
        created_at: &context.created_at,
        started_at: state.started_at.as_deref(),
        completed_at: state.completed_at.as_deref(),
        exit_code: state.exit_code,
        error: state.error.as_deref(),
        rollout_path: state.rollout_path.as_deref(),
        cleanup_policy: configured_cleanup_policy(delegate),
        cleanup_status: state.cleanup_status.as_deref(),
        cleanup_error: state.cleanup_error.as_deref(),
        workspace_mode: state.workspace_mode.as_deref(),
        workspace_base: state.workspace_base.as_deref(),
        workspace_path: state.workspace_path.as_deref(),
        workspace_cleanup_status: state.workspace_cleanup_status.as_deref(),
        workspace_patch_path: state.workspace_patch_path.as_deref(),
        workspace_diff_summary_path: state.workspace_diff_summary_path.as_deref(),
        workspace_diff_summary: state.workspace_diff_summary.as_deref(),
    };
    let metadata_json = serde_json::to_vec_pretty(&metadata)?;
    write_atomic(&context.metadata_path, metadata_json).await
}

async fn write_atomic(path: &Path, bytes: Vec<u8>) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    tokio::fs::create_dir_all(parent).await?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("metadata"),
        Uuid::now_v7()
    ));
    tokio::fs::write(&tmp, bytes).await?;
    match tokio::fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        Err(err) if cfg!(windows) && path.exists() => {
            let _ = tokio::fs::remove_file(path).await;
            tokio::fs::rename(&tmp, path).await.or(Err(err))
        }
        Err(err) => Err(err),
    }
}

async fn build_terminal_session_context(
    config: &Config,
    delegate: &ResolvedTerminalDelegate,
    session_id: &str,
) -> anyhow::Result<TerminalSessionContext> {
    let session_id = validate_terminal_session_id(session_id)?;
    let agent_dir =
        sanitize_agent_file_stem(&delegate.agent_id).unwrap_or_else(|| "agent".to_owned());
    let root = config
        .savfox_home
        .join("terminal-agents")
        .join(agent_dir)
        .join("sessions")
        .join(&session_id);
    let agent_home = root.join("home");
    let workspace_dir = root.join("workspace");
    let log_dir = root.join("logs");
    tokio::fs::create_dir_all(&agent_home).await?;
    tokio::fs::create_dir_all(&workspace_dir).await?;
    tokio::fs::create_dir_all(&log_dir).await?;
    let metadata_path = root.join("metadata.json");
    let context = TerminalSessionContext {
        session_id,
        root_dir: root,
        agent_home,
        workspace_dir,
        log_dir,
        metadata_path,
        created_at: rollout_timestamp(),
    };
    write_terminal_session_metadata(
        delegate,
        &context,
        &TerminalSessionMetadataState::new("created"),
    )
    .await?;

    Ok(context)
}

async fn append_terminal_log(log_dir: &Path, name: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    let path = log_dir.join(name);
    let mut file = match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        Ok(file) => file,
        Err(err) => {
            warn!(
                "failed to open terminal delegate log {}: {err}",
                path.display()
            );
            return;
        }
    };
    let entry = format!("\n--- {} ---\n{text}", rollout_timestamp());
    if let Err(err) = file.write_all(entry.as_bytes()).await {
        warn!(
            "failed to write terminal delegate log {}: {err}",
            path.display()
        );
    }
}

async fn apply_terminal_session_cleanup(
    delegate: &ResolvedTerminalDelegate,
    context: &TerminalSessionContext,
    state: &mut TerminalSessionMetadataState,
) {
    match configured_cleanup_policy(delegate) {
        "per_turn" => {
            let mut errors = Vec::new();
            for path in [&context.workspace_dir, &context.agent_home] {
                if !path.exists() {
                    continue;
                }
                if let Err(err) = tokio::fs::remove_dir_all(path).await {
                    errors.push(format!("{}: {err}", path.display()));
                }
            }
            if errors.is_empty() {
                state.cleanup_status = Some("cleaned".to_owned());
                state.cleanup_error = None;
            } else {
                state.cleanup_status = Some("error".to_owned());
                state.cleanup_error = Some(errors.join("; "));
            }
        }
        "manual" => {
            state.cleanup_status = Some("manual".to_owned());
            state.cleanup_error = None;
        }
        _ => {
            state.cleanup_status = Some("retained".to_owned());
            state.cleanup_error = None;
        }
    }

    if let Err(err) = write_terminal_session_metadata(delegate, context, state).await {
        warn!("failed to write terminal delegate cleanup metadata: {err}");
    }
}

async fn load_terminal_context_history(
    config: &Config,
    session_id: &str,
    limit: usize,
) -> Vec<TerminalContextMessage> {
    let rollout_path = config
        .savfox_home
        .join("sessions")
        .join(format!("{session_id}.jsonl"));
    let Ok(raw) = tokio::fs::read_to_string(&rollout_path).await else {
        return Vec::new();
    };

    let mut messages = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(line) = serde_json::from_str::<RolloutLine>(line) else {
            continue;
        };
        match line.item {
            RolloutItem::EventMsg(EventMsg::UserMessage(message)) => {
                if !message.message.trim().is_empty() {
                    messages.push(TerminalContextMessage {
                        role: "user".to_owned(),
                        text: message.message,
                    });
                }
            }
            RolloutItem::EventMsg(EventMsg::AgentMessage(message)) => {
                if !message.message.trim().is_empty() {
                    messages.push(TerminalContextMessage {
                        role: "assistant".to_owned(),
                        text: message.message,
                    });
                }
            }
            _ => {}
        }
    }
    if messages.len() > limit {
        messages.split_off(messages.len() - limit)
    } else {
        messages
    }
}

async fn persist_terminal_attachments(
    log_dir: &Path,
    attachments: &[ChatAttachment],
) -> anyhow::Result<Vec<TerminalAttachmentMetadata>> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }

    let attachment_dir = log_dir.join("attachments");
    tokio::fs::create_dir_all(&attachment_dir).await?;
    let mut out = Vec::with_capacity(attachments.len());
    for (index, attachment) in attachments.iter().enumerate() {
        let mime_type = attachment.mime_type.trim();
        if !mime_type.starts_with("image/") {
            anyhow::bail!(
                "terminal agents only support image attachments right now; got `{mime_type}`"
            );
        }
        let bytes = BASE64_STANDARD
            .decode(attachment.data_base64.trim())
            .map_err(|err| anyhow::anyhow!("invalid base64 for terminal attachment: {err}"))?;
        if bytes.len() > MAX_TERMINAL_ATTACHMENT_BYTES {
            anyhow::bail!(
                "terminal attachment `{}` is too large: {} bytes (limit {})",
                attachment.filename,
                bytes.len(),
                MAX_TERMINAL_ATTACHMENT_BYTES
            );
        }

        let filename = terminal_attachment_filename(&attachment.filename, mime_type, index);
        let path = attachment_dir.join(&filename);
        tokio::fs::write(&path, &bytes).await?;
        out.push(TerminalAttachmentMetadata {
            filename,
            mime_type: mime_type.to_owned(),
            path: path.to_string_lossy().into_owned(),
            size_bytes: bytes.len(),
        });
    }
    Ok(out)
}

fn terminal_attachment_filename(raw: &str, mime_type: &str, index: usize) -> String {
    let fallback = format!("attachment-{}", index + 1);
    let stem = sanitize_terminal_attachment_filename(raw)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback);
    let extension = terminal_attachment_extension(mime_type);
    if Path::new(&stem).extension().is_some() {
        format!("{:02}-{stem}", index + 1)
    } else {
        format!("{:02}-{stem}.{extension}", index + 1)
    }
}

fn sanitize_terminal_attachment_filename(raw: &str) -> Option<String> {
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

fn terminal_attachment_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "img",
    }
}

fn rendered_args(
    delegate: &ResolvedTerminalDelegate,
    values: &TerminalTemplateValues,
) -> Vec<String> {
    if delegate.config.args.is_empty() && delegate.config.stdin.is_none() {
        return vec![values.full_prompt.clone()];
    }

    delegate
        .config
        .args
        .iter()
        .map(|arg| render_template(arg, values))
        .collect()
}

fn resolve_rendered_cwd(
    config: &Config,
    template: Option<&str>,
    values: &TerminalTemplateValues,
) -> PathBuf {
    let rendered = template.map(|template| render_template(template, values));
    resolve_cwd(&config.cwd, rendered.as_deref())
}

fn insert_workspace_events(
    events: &mut Vec<TerminalAgentEvent>,
    workspace_events: Vec<TerminalAgentEvent>,
) {
    if workspace_events.is_empty() {
        return;
    }
    if let Some(index) = events
        .iter()
        .rposition(|event| matches!(event, TerminalAgentEvent::Completed { .. }))
    {
        events.splice(index..index, workspace_events);
    } else {
        events.extend(workspace_events);
    }
}

async fn run_command(
    config: &Config,
    delegate: &ResolvedTerminalDelegate,
    terminal_context: &TerminalSessionContext,
    prompt: &str,
    model: &str,
    attachments: &[ChatAttachment],
    stream_sink: Option<TerminalEventSink>,
) -> anyhow::Result<TerminalRunResult> {
    let persisted_attachments =
        persist_terminal_attachments(&terminal_context.log_dir, attachments).await?;
    let history =
        load_terminal_context_history(config, &terminal_context.session_id, TERMINAL_HISTORY_LIMIT)
            .await;
    let initial_values = build_prompt_values(
        delegate,
        prompt,
        model,
        terminal_context,
        &history,
        &persisted_attachments,
    );
    let requested_cwd =
        resolve_rendered_cwd(config, delegate.config.cwd.as_deref(), &initial_values);
    let workspace_base = resolve_rendered_cwd(
        config,
        delegate
            .config
            .workspace
            .base
            .as_deref()
            .or(delegate.config.cwd.as_deref()),
        &initial_values,
    );
    let mut workspace_state = TerminalWorkspaceManager::prepare(TerminalWorkspaceRequest {
        mode: configured_workspace_mode(delegate).to_owned(),
        base_cwd: workspace_base,
        requested_cwd,
        session_workspace_dir: terminal_context.workspace_dir.clone(),
        log_dir: terminal_context.log_dir.clone(),
        session_id: terminal_context.session_id.clone(),
        cleanup_policy: configured_workspace_cleanup_policy(delegate),
    })
    .await?;
    let values = build_prompt_values_with_workspace(
        delegate,
        prompt,
        model,
        terminal_context,
        &workspace_state.workspace_path,
        &history,
        &persisted_attachments,
    );
    let command = delegate
        .config
        .command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .expect("resolved terminal delegate command is present");
    let mut resolved_command = TerminalCommandResolver::new(config.cwd.clone()).resolve(
        TerminalCommandTemplate {
            command: command.to_owned(),
            args: delegate.config.args.clone(),
            stdin: delegate.config.stdin.clone(),
            cwd: delegate.config.cwd.clone(),
            env: delegate.config.env.clone(),
            timeout_secs: delegate.config.timeout_secs,
            default_args_to_prompt: true,
            default_timeout_secs: DEFAULT_TIMEOUT_SECS,
            min_timeout_secs: MIN_TIMEOUT_SECS,
            max_output_bytes: MAX_OUTPUT_BYTES,
        },
        &values,
    )?;
    resolved_command.spec.cwd = workspace_state.command_cwd.clone();
    let mut metadata_state = TerminalSessionMetadataState::new("starting");
    metadata_state.command = Some(resolved_command.command_display.clone());
    metadata_state.started_at = Some(rollout_timestamp());
    metadata_state.record_workspace(&workspace_state);
    if let Err(err) =
        write_terminal_session_metadata(delegate, terminal_context, &metadata_state).await
    {
        warn!("failed to write terminal delegate metadata: {err}");
    }

    let delegate_for_spawn = delegate.clone();
    let context_for_spawn = terminal_context.clone();
    let mut running_state = metadata_state.clone();
    running_state.status = "running".to_owned();
    let supervisor_started = Instant::now();
    let output_callback = stream_sink.map(|sink| {
        Arc::new(move |stream: &'static str, text: String| {
            sink(TerminalAgentEvent::OutputDelta {
                stream: stream.to_owned(),
                text,
            });
        }) as TerminalOutputCallback
    });
    let supervised = TerminalSupervisor::run_with_spawn_and_output_callback(
        resolved_command.spec,
        move |pid| {
            let delegate_for_spawn = delegate_for_spawn.clone();
            let context_for_spawn = context_for_spawn.clone();
            let mut running_state = running_state;
            running_state.pid = pid;
            async move {
                if let Err(err) = write_terminal_session_metadata(
                    &delegate_for_spawn,
                    &context_for_spawn,
                    &running_state,
                )
                .await
                {
                    warn!("failed to write terminal delegate metadata: {err}");
                }
            }
        },
        output_callback,
    )
    .await;
    record_terminal_runtime_metrics(&supervised.exit_reason, supervisor_started.elapsed());

    append_terminal_log(&terminal_context.log_dir, "stdout.log", &supervised.stdout).await;
    append_terminal_log(&terminal_context.log_dir, "stderr.log", &supervised.stderr).await;
    metadata_state.pid = supervised.pid;
    metadata_state.exit_code = supervised.exit_code;
    let success = supervised.exit_reason.is_success();
    let workspace_finalization =
        TerminalWorkspaceManager::finalize(&mut workspace_state, success).await;
    metadata_state.record_workspace(&workspace_state);

    if !success {
        let message = terminal_supervisor_error_message(command, &supervised);
        metadata_state.status = supervised.exit_reason.metadata_status().to_owned();
        metadata_state.completed_at = Some(rollout_timestamp());
        metadata_state.error = Some(message.clone());
        if let Err(err) =
            write_terminal_session_metadata(delegate, terminal_context, &metadata_state).await
        {
            warn!("failed to write terminal delegate metadata: {err}");
        }
        apply_terminal_session_cleanup(delegate, terminal_context, &mut metadata_state).await;
        return Err(anyhow::anyhow!(message));
    }

    let parsed_output = parse_terminal_output(
        configured_io_protocol(delegate),
        &supervised.stdout,
        &supervised.stderr,
        supervised.exit_code,
    );
    let mut events = parsed_output.events;
    insert_workspace_events(&mut events, workspace_finalization.events);
    metadata_state.status = "completed".to_owned();
    metadata_state.completed_at = Some(rollout_timestamp());
    if let Err(err) =
        write_terminal_session_metadata(delegate, terminal_context, &metadata_state).await
    {
        warn!("failed to write terminal delegate metadata: {err}");
    }
    apply_terminal_session_cleanup(delegate, terminal_context, &mut metadata_state).await;

    Ok(TerminalRunResult {
        reply: parsed_output.reply,
        stdout: supervised.stdout,
        stderr: supervised.stderr,
        stdout_truncated: supervised.stdout_truncated,
        stderr_truncated: supervised.stderr_truncated,
        pid: metadata_state.pid,
        exit_code: metadata_state.exit_code,
        metadata_state,
        events,
        attachments: persisted_attachments,
    })
}

fn terminal_supervisor_error_message(
    command: &str,
    supervised: &TerminalSupervisorResult,
) -> String {
    match &supervised.exit_reason {
        TerminalExitReason::NonZero => {
            let code = supervised
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "terminated by signal".to_owned());
            let stderr_preview = supervised.stderr.trim();
            let stdout_preview = supervised.stdout.trim();
            let detail = if !stderr_preview.is_empty() {
                stderr_preview
            } else if !stdout_preview.is_empty() {
                stdout_preview
            } else {
                "no output"
            };
            format!("terminal delegate `{command}` exited with {code}: {detail}")
        }
        TerminalExitReason::Timeout
        | TerminalExitReason::SpawnError
        | TerminalExitReason::InvalidCwd
        | TerminalExitReason::IoError => supervised
            .error
            .clone()
            .unwrap_or_else(|| format!("terminal delegate `{command}` failed")),
        TerminalExitReason::Completed => format!("terminal delegate `{command}` completed"),
    }
}

fn rollout_timestamp() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn rollout_line(item: RolloutItem) -> RolloutLine {
    RolloutLine {
        timestamp: rollout_timestamp(),
        item,
    }
}

async fn write_rollout_line(file: &mut tokio::fs::File, item: RolloutItem) -> std::io::Result<()> {
    let mut line = serde_json::to_string(&rollout_line(item))?;
    line.push('\n');
    file.write_all(line.as_bytes()).await?;
    file.flush().await
}

async fn persist_terminal_delegate_rollout(
    config: &Config,
    prompt: &str,
    reply: &str,
    session_id: &str,
    model: &str,
    provider: &str,
    attachments: &[TerminalAttachmentMetadata],
) -> std::io::Result<PathBuf> {
    let session_id_string = validate_terminal_session_id(session_id)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
    let session_id = SessionId::from_string(&session_id_string)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
    let sessions_dir = config.savfox_home.join("sessions");
    tokio::fs::create_dir_all(&sessions_dir).await?;
    let rollout_path = sessions_dir.join(format!("{session_id_string}.jsonl"));
    let needs_meta = tokio::fs::metadata(&rollout_path)
        .await
        .map(|metadata| metadata.len() == 0)
        .unwrap_or(true);
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rollout_path)
        .await?;

    if needs_meta {
        write_rollout_line(
            &mut file,
            RolloutItem::SessionMeta(SessionMetaLine {
                meta: SessionMeta {
                    id: session_id,
                    forked_from_id: None,
                    timestamp: rollout_timestamp(),
                    cwd: config.cwd.clone(),
                    originator: "savfox-gateway".to_owned(),
                    cli_version: env!("CARGO_PKG_VERSION").to_owned(),
                    source: SessionSource::VSCode,
                    model: Some(SessionModel {
                        provider: provider.to_owned(),
                        model_slug: model.to_owned(),
                    }),
                    model_provider: None,
                    base_instructions: None,
                    dynamic_tools: None,
                },
                git: None,
            }),
        )
        .await?;
    }

    write_rollout_line(
        &mut file,
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: prompt.to_owned(),
            images: None,
            local_images: attachments
                .iter()
                .map(|attachment| PathBuf::from(&attachment.path))
                .collect(),
            text_elements: Vec::new(),
        })),
    )
    .await?;
    write_rollout_line(
        &mut file,
        RolloutItem::EventMsg(EventMsg::AgentMessage(AgentMessageEvent {
            message: reply.to_owned(),
        })),
    )
    .await?;

    Ok(rollout_path)
}

impl GatewayChannel {
    pub(crate) async fn resolve_terminal_delegate(
        &self,
        agent_ref: &str,
    ) -> Option<ResolvedTerminalDelegate> {
        let (file_stem, config) = resolve_agent_config(self.config(), agent_ref).await?;
        terminal_delegate_from_agent_config(file_stem, config)
    }

    pub(crate) async fn invoke_terminal_delegate_agent(
        &self,
        prompt: &str,
        agent_ref: &str,
        model: &str,
        session_id: Option<&str>,
        attachments: &[ChatAttachment],
        stream_sink: Option<TerminalEventSink>,
    ) -> anyhow::Result<Option<TerminalDelegateInvocation>> {
        let Some(delegate) = self.resolve_terminal_delegate(agent_ref).await else {
            return Ok(None);
        };

        let session_id = normalize_terminal_session_id(session_id)?;
        let terminal_context =
            build_terminal_session_context(self.config(), &delegate, &session_id).await?;
        let run = run_command(
            self.config(),
            &delegate,
            &terminal_context,
            prompt,
            model,
            attachments,
            stream_sink,
        )
        .await?;
        let model = format!("terminal/{}", delegate.agent_id);
        let provider = "terminal".to_owned();
        let rollout_path = match persist_terminal_delegate_rollout(
            self.config(),
            prompt,
            &run.reply,
            &session_id,
            &model,
            &provider,
            &run.attachments,
        )
        .await
        {
            Ok(path) => Some(path),
            Err(err) => {
                warn!("failed to persist terminal delegate rollout: {err}");
                None
            }
        };
        if let Some(path) = rollout_path.as_deref() {
            let mut metadata_state = run.metadata_state.clone();
            metadata_state.rollout_path = Some(path.to_string_lossy().into_owned());
            if let Err(err) =
                write_terminal_session_metadata(&delegate, &terminal_context, &metadata_state).await
            {
                warn!("failed to write terminal delegate rollout metadata: {err}");
            }
        }
        let result = AgentInvocationResult {
            reply: run.reply,
            session_id: session_id.clone(),
            thread_id: session_id,
            rollout_path,
            last_token_usage: None,
        };
        let terminal = TerminalInvocationMetadata {
            agent_id: delegate.agent_id.clone(),
            agent_name: delegate.agent_name.clone(),
            profile: configured_profile(&delegate).to_owned(),
            mode: configured_mode(&delegate).to_owned(),
            session_scope: configured_session_scope(&delegate).to_owned(),
            io_protocol: configured_io_protocol(&delegate).to_owned(),
            workspace_dir: terminal_context.workspace_dir,
            log_dir: terminal_context.log_dir,
            stdout: run.stdout,
            stderr: run.stderr,
            stdout_truncated: run.stdout_truncated,
            stderr_truncated: run.stderr_truncated,
            pid: run.pid,
            exit_code: run.exit_code,
            events: run.events,
            attachments: run.attachments,
        };

        Ok(Some(TerminalDelegateInvocation {
            result,
            model,
            provider,
            terminal,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use savfox_core::config::ConfigBuilder;
    use savfox_gateway_shared::ChatAttachment;
    use savfox_protocol::protocol::{AgentMessageEvent, EventMsg, RolloutItem};
    use serde_json::json;

    use super::{
        ResolvedTerminalDelegate, TerminalSessionMetadataState, build_terminal_session_context,
        configured_mode, configured_profile, persist_terminal_delegate_rollout,
        resolve_agent_config, run_command, terminal_delegate_from_agent_config,
        validate_terminal_session_id, write_rollout_line, write_terminal_session_metadata,
    };
    use crate::agent_terminal_delegate::{
        AgentTerminalDelegateConfig, render_template, rendered_args,
    };
    use crate::terminal_agent::TerminalTemplateValues;

    #[cfg(windows)]
    fn fake_terminal_echo_command() -> (String, Vec<String>, Option<String>) {
        (
            "powershell".to_owned(),
            vec![
                "-NoProfile".to_owned(),
                "-Command".to_owned(),
                "$text = [Console]::In.ReadToEnd(); Write-Output \"reply: $text\"".to_owned(),
            ],
            Some("{{prompt}}".to_owned()),
        )
    }

    #[cfg(not(windows))]
    fn fake_terminal_echo_command() -> (String, Vec<String>, Option<String>) {
        (
            "sh".to_owned(),
            vec![
                "-c".to_owned(),
                "text=$(cat); printf 'reply: %s' \"$text\"".to_owned(),
            ],
            Some("{{prompt}}".to_owned()),
        )
    }

    #[test]
    fn terminal_delegate_requires_enabled_command() {
        let config = json!({
            "id": "cli",
            "name": "CLI",
            "terminal_delegate": {
                "enabled": true,
                "command": "claude"
            }
        });

        let delegate = terminal_delegate_from_agent_config("cli".to_owned(), config)
            .expect("enabled delegate with command should resolve");

        assert_eq!(delegate.agent_id, "cli");
        assert_eq!(delegate.agent_name, "CLI");
    }

    #[test]
    fn terminal_delegate_ignores_disabled_config() {
        let config = json!({
            "id": "cli",
            "terminal_delegate": {
                "enabled": false,
                "command": "claude"
            }
        });

        assert!(terminal_delegate_from_agent_config("cli".to_owned(), config).is_none());
    }

    #[test]
    fn render_template_supports_prompt_and_metadata() {
        let agent_home = PathBuf::from("/tmp/savfox-agent-home");
        let workspace_dir = PathBuf::from("/tmp/savfox-workspace");
        let log_dir = PathBuf::from("/tmp/savfox-logs");
        let values = TerminalTemplateValues {
            prompt: "hello".to_owned(),
            full_prompt: "system\n\nUser request:\nhello".to_owned(),
            system_prompt: "system".to_owned(),
            conversation_context: "No previous turns.".to_owned(),
            attachment_manifest: "No attachments.".to_owned(),
            terminal_input_json: "{}".to_owned(),
            agent_id: "cli".to_owned(),
            agent_name: "CLI".to_owned(),
            model: "default".to_owned(),
            session_id: "018f0000-0000-7000-8000-000000000000".to_owned(),
            agent_home: agent_home.to_string_lossy().into_owned(),
            workspace_dir: workspace_dir.to_string_lossy().into_owned(),
            log_dir: log_dir.to_string_lossy().into_owned(),
        };

        assert_eq!(
            render_template("{{agent_name}}/{{agent_id}} {{model}} {{prompt}}", &values),
            "CLI/cli default system\n\nUser request:\nhello"
        );
        assert_eq!(
            render_template("{{user_prompt}} -- {{system_prompt}}", &values),
            "hello -- system"
        );
        assert_eq!(
            render_template(
                "{{session_id}} {{agent_home}} {{workspace_dir}} {{log_dir}} {{conversation_context}} {{attachment_manifest}} {{terminal_input_json}}",
                &values
            ),
            format!(
                "018f0000-0000-7000-8000-000000000000 {} {} {} No previous turns. No attachments. {{}}",
                agent_home.display(),
                workspace_dir.display(),
                log_dir.display()
            )
        );
    }

    #[test]
    fn rendered_args_default_to_prompt_when_no_stdin_or_args() {
        let delegate = ResolvedTerminalDelegate {
            config: AgentTerminalDelegateConfig {
                enabled: true,
                profile: None,
                mode: None,
                session_scope: None,
                io_protocol: None,
                health_check_command: None,
                health_check_args: None,
                command: Some("tool".to_owned()),
                args: Vec::new(),
                stdin: None,
                cwd: None,
                env: Default::default(),
                timeout_secs: None,
                cleanup_policy: None,
                include_system_prompt: true,
                interactive_command: None,
                interactive_args: None,
                workspace: Default::default(),
            },
            agent_id: "cli".to_owned(),
            agent_name: "CLI".to_owned(),
            system_prompt: String::new(),
        };
        let values = TerminalTemplateValues {
            prompt: "hello".to_owned(),
            full_prompt: "hello".to_owned(),
            system_prompt: String::new(),
            conversation_context: String::new(),
            attachment_manifest: String::new(),
            terminal_input_json: String::new(),
            agent_id: "cli".to_owned(),
            agent_name: "CLI".to_owned(),
            model: "default".to_owned(),
            session_id: "018f0000-0000-7000-8000-000000000000".to_owned(),
            agent_home: String::new(),
            workspace_dir: String::new(),
            log_dir: String::new(),
        };

        assert_eq!(rendered_args(&delegate, &values), vec!["hello"]);
    }

    #[tokio::test]
    async fn build_terminal_session_context_creates_isolated_dirs_and_metadata() {
        let home = tempfile::tempdir().expect("create temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(home.path().to_path_buf())
            .build()
            .await
            .expect("load config");
        config.cwd = home.path().to_path_buf();
        let delegate = ResolvedTerminalDelegate {
            config: AgentTerminalDelegateConfig {
                enabled: true,
                profile: Some("claude".to_owned()),
                mode: Some("one_shot".to_owned()),
                session_scope: Some("per_session".to_owned()),
                io_protocol: Some("plain_text".to_owned()),
                health_check_command: None,
                health_check_args: None,
                command: Some("claude".to_owned()),
                args: Vec::new(),
                stdin: None,
                cwd: None,
                env: Default::default(),
                timeout_secs: None,
                cleanup_policy: None,
                include_system_prompt: true,
                interactive_command: None,
                interactive_args: None,
                workspace: Default::default(),
            },
            agent_id: "Claude:Worker".to_owned(),
            agent_name: "Claude Worker".to_owned(),
            system_prompt: String::new(),
        };
        let session_id = "018f0000-0000-7000-8000-000000000001";

        let context = build_terminal_session_context(&config, &delegate, session_id)
            .await
            .expect("build terminal context");

        assert!(context.agent_home.is_dir());
        assert!(context.workspace_dir.is_dir());
        assert!(context.log_dir.is_dir());
        assert!(
            context
                .agent_home
                .to_string_lossy()
                .contains("Claude-Worker")
        );
        let metadata = tokio::fs::read_to_string(&context.metadata_path)
            .await
            .expect("read metadata");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata json");
        assert_eq!(metadata["agent_id"], "Claude:Worker");
        assert_eq!(metadata["session_id"], session_id);
        assert_eq!(metadata["profile"], "claude");
        assert_eq!(metadata["mode"], "one_shot");
        assert_eq!(metadata["status"], "created");
        assert!(metadata["root_dir"].as_str().is_some_and(|value| {
            value.contains("terminal-agents") && value.contains("sessions")
        }));
        assert!(
            metadata["metadata_path"]
                .as_str()
                .is_some_and(|value| { value.ends_with("metadata.json") })
        );
    }

    #[tokio::test]
    async fn build_terminal_session_context_keeps_sessions_isolated() {
        let home = tempfile::tempdir().expect("create temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(home.path().to_path_buf())
            .build()
            .await
            .expect("load config");
        config.cwd = home.path().to_path_buf();
        let delegate = ResolvedTerminalDelegate {
            config: AgentTerminalDelegateConfig {
                enabled: true,
                profile: None,
                mode: None,
                session_scope: None,
                io_protocol: None,
                health_check_command: None,
                health_check_args: None,
                command: Some("codex".to_owned()),
                args: Vec::new(),
                stdin: None,
                cwd: None,
                env: Default::default(),
                timeout_secs: None,
                cleanup_policy: None,
                include_system_prompt: true,
                interactive_command: None,
                interactive_args: None,
                workspace: Default::default(),
            },
            agent_id: "worker".to_owned(),
            agent_name: "Worker".to_owned(),
            system_prompt: String::new(),
        };

        let first = build_terminal_session_context(
            &config,
            &delegate,
            "018f0000-0000-7000-8000-000000000101",
        )
        .await
        .expect("build first terminal context");
        let second = build_terminal_session_context(
            &config,
            &delegate,
            "018f0000-0000-7000-8000-000000000102",
        )
        .await
        .expect("build second terminal context");

        assert_ne!(first.root_dir, second.root_dir);
        assert_ne!(first.agent_home, second.agent_home);
        assert_ne!(first.workspace_dir, second.workspace_dir);
        assert_ne!(first.log_dir, second.log_dir);
        assert!(first.metadata_path.is_file());
        assert!(second.metadata_path.is_file());
    }

    #[tokio::test]
    async fn metadata_writes_remain_parseable_when_concurrent() {
        let home = tempfile::tempdir().expect("create temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(home.path().to_path_buf())
            .build()
            .await
            .expect("load config");
        config.cwd = home.path().to_path_buf();
        let delegate = ResolvedTerminalDelegate {
            config: AgentTerminalDelegateConfig {
                enabled: true,
                profile: None,
                mode: None,
                session_scope: None,
                io_protocol: None,
                health_check_command: None,
                health_check_args: None,
                command: Some("codex".to_owned()),
                args: Vec::new(),
                stdin: None,
                cwd: None,
                env: Default::default(),
                timeout_secs: None,
                cleanup_policy: None,
                include_system_prompt: true,
                interactive_command: None,
                interactive_args: None,
                workspace: Default::default(),
            },
            agent_id: "worker".to_owned(),
            agent_name: "Worker".to_owned(),
            system_prompt: String::new(),
        };
        let context = build_terminal_session_context(
            &config,
            &delegate,
            "018f0000-0000-7000-8000-000000000111",
        )
        .await
        .expect("build terminal context");

        let mut tasks = Vec::new();
        for idx in 0..8_u32 {
            let delegate = delegate.clone();
            let context = context.clone();
            tasks.push(tokio::spawn(async move {
                let mut state = TerminalSessionMetadataState::new("running");
                state.pid = Some(idx);
                write_terminal_session_metadata(&delegate, &context, &state)
                    .await
                    .expect("write metadata");
            }));
        }
        for task in tasks {
            task.await.expect("metadata writer task");
        }

        let metadata = tokio::fs::read_to_string(&context.metadata_path)
            .await
            .expect("read metadata");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata json");
        assert_eq!(metadata["status"], "running");
        assert!(metadata["pid"].as_u64().is_some());
    }

    #[tokio::test]
    async fn per_turn_cleanup_removes_runtime_work_dirs_and_keeps_metadata() {
        let home = tempfile::tempdir().expect("create temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(home.path().to_path_buf())
            .build()
            .await
            .expect("load config");
        config.cwd = home.path().to_path_buf();
        let (command, args, stdin) = fake_terminal_echo_command();
        let delegate = terminal_delegate_from_agent_config(
            "cli".to_owned(),
            json!({
                "id": "cli",
                "name": "CLI",
                "terminal_delegate": {
                    "enabled": true,
                    "command": command,
                    "args": args,
                    "stdin": stdin,
                    "timeout_secs": 5,
                    "cleanup_policy": "per_turn"
                }
            }),
        )
        .expect("terminal delegate config should resolve");
        let context = build_terminal_session_context(
            &config,
            &delegate,
            "018f0000-0000-7000-8000-000000000112",
        )
        .await
        .expect("build terminal context");
        let workspace_dir = context.workspace_dir.clone();
        let agent_home = context.agent_home.clone();
        assert!(workspace_dir.is_dir());
        assert!(agent_home.is_dir());

        let result = run_command(
            &config,
            &delegate,
            &context,
            "cleanup",
            "default",
            &[],
            None,
        )
        .await
        .expect("run fake terminal delegate");

        assert!(result.reply.contains("cleanup"));
        assert!(!workspace_dir.exists());
        assert!(!agent_home.exists());
        assert!(context.metadata_path.is_file());
        let metadata = tokio::fs::read_to_string(&context.metadata_path)
            .await
            .expect("read metadata");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata json");
        assert_eq!(metadata["cleanup_policy"], "per_turn");
        assert_eq!(metadata["cleanup_status"], "cleaned");
    }

    #[test]
    fn terminal_session_id_requires_uuid_v7() {
        assert!(validate_terminal_session_id("018f0000-0000-7000-8000-000000000001").is_ok());
        let err = validate_terminal_session_id("123e4567-e89b-42d3-a456-426614174000")
            .expect_err("uuid v4 should be rejected");
        assert!(err.to_string().contains("UUID v7 required"));
    }

    #[tokio::test]
    async fn run_command_supports_old_config_and_updates_metadata() {
        let home = tempfile::tempdir().expect("create temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(home.path().to_path_buf())
            .build()
            .await
            .expect("load config");
        config.cwd = home.path().to_path_buf();
        let (command, args, stdin) = fake_terminal_echo_command();
        let delegate = terminal_delegate_from_agent_config(
            "cli".to_owned(),
            json!({
                "id": "cli",
                "name": "CLI",
                "terminal_delegate": {
                    "enabled": true,
                    "command": command,
                    "args": args,
                    "stdin": stdin,
                    "timeout_secs": 5
                }
            }),
        )
        .expect("old terminal delegate config should resolve");
        assert_eq!(configured_profile(&delegate), "custom");
        assert_eq!(configured_mode(&delegate), "one_shot");
        let context = build_terminal_session_context(
            &config,
            &delegate,
            "018f0000-0000-7000-8000-000000000201",
        )
        .await
        .expect("build terminal context");

        let result = run_command(
            &config,
            &delegate,
            &context,
            "hello terminal",
            "default",
            &[],
            None,
        )
        .await
        .expect("run fake terminal delegate");

        assert!(
            result
                .reply
                .contains("Current user request:\nhello terminal")
        );
        assert_eq!(result.exit_code, Some(0));
        assert!(context.log_dir.join("stdout.log").is_file());
        let metadata = tokio::fs::read_to_string(&context.metadata_path)
            .await
            .expect("read metadata");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata json");
        assert_eq!(metadata["profile"], "custom");
        assert_eq!(metadata["mode"], "one_shot");
        assert_eq!(metadata["status"], "completed");
        assert_eq!(metadata["exit_code"], 0);
        assert!(metadata["pid"].as_u64().is_some());
        assert!(metadata["started_at"].as_str().is_some());
        assert!(metadata["completed_at"].as_str().is_some());
    }

    #[tokio::test]
    async fn one_shot_runtime_runs_fake_cli_from_agent_config_file() {
        let home = tempfile::tempdir().expect("create temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(home.path().to_path_buf())
            .build()
            .await
            .expect("load config");
        config.cwd = home.path().to_path_buf();
        let (command, args, stdin) = fake_terminal_echo_command();
        let agents_dir = config.savfox_home.join("agents");
        tokio::fs::create_dir_all(&agents_dir)
            .await
            .expect("create agents dir");
        tokio::fs::write(
            agents_dir.join("cli.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "cli",
                "name": "CLI",
                "terminal_delegate": {
                    "enabled": true,
                    "command": command,
                    "args": args,
                    "stdin": stdin,
                    "timeout_secs": 5
                }
            }))
            .expect("serialize agent config"),
        )
        .await
        .expect("write agent config");

        let session_id = "018f0000-0000-7000-8000-000000000301";
        let (file_stem, raw_config) = resolve_agent_config(&config, "cli")
            .await
            .expect("resolve agent config");
        let delegate = terminal_delegate_from_agent_config(file_stem, raw_config)
            .expect("terminal delegate config");
        let context = build_terminal_session_context(&config, &delegate, session_id)
            .await
            .expect("build terminal context");

        let result = run_command(
            &config,
            &delegate,
            &context,
            "hello from gateway",
            "default",
            &[],
            None,
        )
        .await
        .expect("run fake terminal delegate");
        let rollout_path = persist_terminal_delegate_rollout(
            &config,
            "hello from gateway",
            &result.reply,
            session_id,
            "terminal/cli",
            "terminal",
            &result.attachments,
        )
        .await
        .expect("persist rollout");
        let mut metadata_state = result.metadata_state.clone();
        metadata_state.rollout_path = Some(rollout_path.to_string_lossy().into_owned());
        write_terminal_session_metadata(&delegate, &context, &metadata_state)
            .await
            .expect("write rollout metadata");

        assert!(
            result
                .reply
                .contains("Current user request:\nhello from gateway")
        );
        assert!(rollout_path.is_file());
        let metadata_path = config
            .savfox_home
            .join("terminal-agents")
            .join("cli")
            .join("sessions")
            .join(session_id)
            .join("metadata.json");
        let metadata = tokio::fs::read_to_string(metadata_path)
            .await
            .expect("read metadata");
        let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata json");
        assert_eq!(metadata["status"], "completed");
        let rollout_path = rollout_path.to_string_lossy();
        assert_eq!(
            metadata["rollout_path"].as_str(),
            Some(rollout_path.as_ref())
        );
    }

    #[tokio::test]
    async fn run_command_packages_history_and_image_attachments() {
        let home = tempfile::tempdir().expect("create temp dir");
        let mut config = ConfigBuilder::default()
            .savfox_home(home.path().to_path_buf())
            .build()
            .await
            .expect("load config");
        config.cwd = home.path().to_path_buf();
        let (command, args, stdin) = fake_terminal_echo_command();
        let delegate = terminal_delegate_from_agent_config(
            "cli".to_owned(),
            json!({
                "id": "cli",
                "name": "CLI",
                "system_prompt": "Be precise.",
                "terminal_delegate": {
                    "enabled": true,
                    "command": command,
                    "args": args,
                    "stdin": stdin,
                    "timeout_secs": 5
                }
            }),
        )
        .expect("terminal delegate config should resolve");
        let session_id = "018f0000-0000-7000-8000-000000000401";
        let previous_rollout = persist_terminal_delegate_rollout(
            &config,
            "previous question",
            "previous answer",
            session_id,
            "terminal/cli",
            "terminal",
            &[],
        )
        .await
        .expect("persist previous rollout");
        assert!(previous_rollout.is_file());
        let context = build_terminal_session_context(&config, &delegate, session_id)
            .await
            .expect("build terminal context");
        let attachments = vec![ChatAttachment {
            filename: "screen.png".to_owned(),
            mime_type: "image/png".to_owned(),
            data_base64: "iVBORw0KGgo=".to_owned(),
        }];

        let result = run_command(
            &config,
            &delegate,
            &context,
            "what changed?",
            "default",
            &attachments,
            None,
        )
        .await
        .expect("run terminal delegate with attachments");

        assert!(result.reply.contains("System instructions:\nBe precise."));
        assert!(result.reply.contains("previous question"));
        assert!(result.reply.contains("previous answer"));
        assert!(
            result
                .reply
                .contains("Current user request:\nwhat changed?")
        );
        assert!(result.reply.contains("screen.png"));
        assert_eq!(result.attachments.len(), 1);
        let attachment_path = PathBuf::from(&result.attachments[0].path);
        assert!(attachment_path.is_file());
        assert!(attachment_path.ends_with("01-screen.png"));
    }

    #[tokio::test]
    async fn write_rollout_line_persists_parseable_event() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("session.jsonl");
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .expect("open rollout file");

        write_rollout_line(
            &mut file,
            RolloutItem::EventMsg(EventMsg::AgentMessage(AgentMessageEvent {
                message: "from cli".to_owned(),
            })),
        )
        .await
        .expect("write rollout line");

        let text = tokio::fs::read_to_string(path)
            .await
            .expect("read rollout file");
        let line: savfox_protocol::protocol::RolloutLine =
            serde_json::from_str(text.trim()).expect("parse rollout line");
        match line.item {
            RolloutItem::EventMsg(EventMsg::AgentMessage(message)) => {
                assert_eq!(message.message, "from cli");
            }
            other => panic!("unexpected rollout item: {other:?}"),
        }
    }
}
