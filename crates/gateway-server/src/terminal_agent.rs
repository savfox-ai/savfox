use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct TerminalProfilePreset {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) command: &'static str,
    pub(crate) args: &'static [&'static str],
    pub(crate) stdin: Option<&'static str>,
    pub(crate) interactive_command: &'static str,
    pub(crate) interactive_args: &'static [&'static str],
    pub(crate) mode: &'static str,
    pub(crate) session_scope: &'static str,
    pub(crate) io_protocol: &'static str,
    pub(crate) version_args: &'static [&'static str],
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TerminalCommandHealth {
    pub(crate) command: String,
    pub(crate) available: bool,
    pub(crate) version: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) exit_code: Option<i32>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TerminalAgentHealth {
    pub(crate) profile: String,
    pub(crate) command: TerminalCommandHealth,
    pub(crate) cwd: String,
    pub(crate) cwd_exists: bool,
    pub(crate) cwd_is_dir: bool,
    pub(crate) terminal_root: String,
    pub(crate) terminal_root_writable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum TerminalAgentEvent {
    Log { stream: String, text: String },
    Message { text: String },
    Status { status: String },
    Error { message: String },
    Completed { exit_code: Option<i32> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedTerminalOutput {
    pub(crate) reply: String,
    pub(crate) events: Vec<TerminalAgentEvent>,
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalCommandSpec {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) stdin: Option<String>,
    pub(crate) timeout: Duration,
    pub(crate) max_output_bytes: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TerminalTemplateValues {
    pub(crate) prompt: String,
    pub(crate) full_prompt: String,
    pub(crate) system_prompt: String,
    pub(crate) agent_id: String,
    pub(crate) agent_name: String,
    pub(crate) model: String,
    pub(crate) session_id: String,
    pub(crate) agent_home: String,
    pub(crate) workspace_dir: String,
    pub(crate) log_dir: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalCommandTemplate {
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) stdin: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) timeout_secs: Option<u64>,
    pub(crate) default_args_to_prompt: bool,
    pub(crate) default_timeout_secs: u64,
    pub(crate) min_timeout_secs: u64,
    pub(crate) max_output_bytes: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedTerminalCommand {
    pub(crate) spec: TerminalCommandSpec,
    pub(crate) command_display: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalCommandResolver {
    base_cwd: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalExitReason {
    Completed,
    NonZero,
    Timeout,
    SpawnError,
    InvalidCwd,
    IoError,
}

impl TerminalExitReason {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            TerminalExitReason::Completed => "completed",
            TerminalExitReason::NonZero => "non_zero",
            TerminalExitReason::Timeout => "timeout",
            TerminalExitReason::SpawnError => "spawn_error",
            TerminalExitReason::InvalidCwd => "invalid_cwd",
            TerminalExitReason::IoError => "io_error",
        }
    }

    pub(crate) fn metadata_status(&self) -> &'static str {
        match self {
            TerminalExitReason::Completed => "completed",
            TerminalExitReason::Timeout => "timeout",
            TerminalExitReason::NonZero
            | TerminalExitReason::SpawnError
            | TerminalExitReason::InvalidCwd
            | TerminalExitReason::IoError => "error",
        }
    }

    pub(crate) fn is_success(&self) -> bool {
        matches!(self, TerminalExitReason::Completed)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalSupervisorResult {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) stdout_truncated: bool,
    pub(crate) stderr_truncated: bool,
    pub(crate) pid: Option<u32>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) exit_reason: TerminalExitReason,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalWorkspaceRequest {
    pub(crate) mode: String,
    pub(crate) base_cwd: PathBuf,
    pub(crate) requested_cwd: PathBuf,
    pub(crate) session_workspace_dir: PathBuf,
    pub(crate) log_dir: PathBuf,
    pub(crate) session_id: String,
    pub(crate) cleanup_policy: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalWorkspaceState {
    pub(crate) mode: String,
    pub(crate) base_cwd: PathBuf,
    pub(crate) command_cwd: PathBuf,
    pub(crate) workspace_path: PathBuf,
    pub(crate) log_dir: PathBuf,
    pub(crate) git_root: Option<PathBuf>,
    pub(crate) worktree_path: Option<PathBuf>,
    pub(crate) cleanup_policy: String,
    pub(crate) cleanup_status: Option<String>,
    pub(crate) patch_path: Option<PathBuf>,
    pub(crate) diff_summary_path: Option<PathBuf>,
    pub(crate) diff_summary: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TerminalWorkspaceFinalization {
    pub(crate) cleanup_status: Option<String>,
    pub(crate) patch_path: Option<PathBuf>,
    pub(crate) diff_summary_path: Option<PathBuf>,
    pub(crate) diff_summary: Option<String>,
    pub(crate) events: Vec<TerminalAgentEvent>,
}

pub(crate) struct TerminalSupervisor;
pub(crate) struct TerminalWorkspaceManager;

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct TerminalRuntimeMetrics {
    pub(crate) spawn_count: u64,
    pub(crate) completed_count: u64,
    pub(crate) error_count: u64,
    pub(crate) timeout_count: u64,
    pub(crate) total_duration_ms: u64,
    pub(crate) last_duration_ms: Option<u64>,
    pub(crate) last_exit_reason: Option<String>,
    pub(crate) exit_reasons: BTreeMap<String, u64>,
}

const CODEX_ARGS: &[&str] = &["exec", "{{prompt}}"];
const CLAUDE_ARGS: &[&str] = &["-p", "{{prompt}}"];
const EMPTY_ARGS: &[&str] = &[];
const VERSION_ARGS: &[&str] = &["--version"];

pub(crate) const TERMINAL_PROFILE_PRESETS: &[TerminalProfilePreset] = &[
    TerminalProfilePreset {
        id: "codex",
        label: "Codex",
        command: "codex",
        args: CODEX_ARGS,
        stdin: None,
        interactive_command: "codex",
        interactive_args: EMPTY_ARGS,
        mode: "one_shot",
        session_scope: "per_session",
        io_protocol: "plain_text",
        version_args: VERSION_ARGS,
    },
    TerminalProfilePreset {
        id: "claude",
        label: "Claude",
        command: "claude",
        args: CLAUDE_ARGS,
        stdin: None,
        interactive_command: "claude",
        interactive_args: EMPTY_ARGS,
        mode: "one_shot",
        session_scope: "per_session",
        io_protocol: "plain_text",
        version_args: VERSION_ARGS,
    },
    TerminalProfilePreset {
        id: "custom",
        label: "Custom CLI",
        command: "",
        args: EMPTY_ARGS,
        stdin: Some("{{prompt}}"),
        interactive_command: "",
        interactive_args: EMPTY_ARGS,
        mode: "one_shot",
        session_scope: "per_session",
        io_protocol: "plain_text",
        version_args: VERSION_ARGS,
    },
    TerminalProfilePreset {
        id: "jsonl_custom",
        label: "JSONL Custom",
        command: "",
        args: EMPTY_ARGS,
        stdin: Some("{{prompt}}"),
        interactive_command: "",
        interactive_args: EMPTY_ARGS,
        mode: "one_shot",
        session_scope: "per_session",
        io_protocol: "jsonl",
        version_args: VERSION_ARGS,
    },
];

pub(crate) fn terminal_profile_presets() -> &'static [TerminalProfilePreset] {
    TERMINAL_PROFILE_PRESETS
}

pub(crate) fn terminal_profile_preset(profile: &str) -> TerminalProfilePreset {
    let profile = profile.trim();
    TERMINAL_PROFILE_PRESETS
        .iter()
        .copied()
        .find(|preset| preset.id.eq_ignore_ascii_case(profile))
        .unwrap_or(TERMINAL_PROFILE_PRESETS[2])
}

fn terminal_runtime_metrics_store() -> &'static Mutex<TerminalRuntimeMetrics> {
    static STORE: OnceLock<Mutex<TerminalRuntimeMetrics>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(TerminalRuntimeMetrics::default()))
}

pub(crate) fn record_terminal_runtime_metrics(
    exit_reason: &TerminalExitReason,
    duration: Duration,
) {
    let reason = exit_reason.as_str().to_owned();
    let duration_ms = duration.as_millis().min(u128::from(u64::MAX)) as u64;
    let mut metrics = terminal_runtime_metrics_store()
        .lock()
        .expect("terminal runtime metrics lock poisoned");
    metrics.spawn_count = metrics.spawn_count.saturating_add(1);
    metrics.total_duration_ms = metrics.total_duration_ms.saturating_add(duration_ms);
    metrics.last_duration_ms = Some(duration_ms);
    metrics.last_exit_reason = Some(reason.clone());
    *metrics.exit_reasons.entry(reason).or_insert(0) += 1;
    match exit_reason {
        TerminalExitReason::Completed => {
            metrics.completed_count = metrics.completed_count.saturating_add(1);
        }
        TerminalExitReason::Timeout => {
            metrics.timeout_count = metrics.timeout_count.saturating_add(1);
            metrics.error_count = metrics.error_count.saturating_add(1);
        }
        TerminalExitReason::NonZero
        | TerminalExitReason::SpawnError
        | TerminalExitReason::InvalidCwd
        | TerminalExitReason::IoError => {
            metrics.error_count = metrics.error_count.saturating_add(1);
        }
    }
}

pub(crate) fn terminal_runtime_metrics_snapshot() -> TerminalRuntimeMetrics {
    terminal_runtime_metrics_store()
        .lock()
        .expect("terminal runtime metrics lock poisoned")
        .clone()
}

pub(crate) async fn command_health(
    command: &str,
    version_args: &[&str],
    timeout: Duration,
) -> TerminalCommandHealth {
    let command = command.trim();
    if command.is_empty() {
        return TerminalCommandHealth {
            command: String::new(),
            available: false,
            version: None,
            error: Some("missing terminal command".to_owned()),
            exit_code: None,
        };
    }

    let mut cmd = Command::new(command);
    cmd.args(version_args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return TerminalCommandHealth {
                command: command.to_owned(),
                available: false,
                version: None,
                error: Some(err.to_string()),
                exit_code: None,
            };
        }
    };

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let version = if !stdout.is_empty() {
                Some(stdout)
            } else if !stderr.is_empty() {
                Some(stderr)
            } else {
                None
            };
            TerminalCommandHealth {
                command: command.to_owned(),
                available: output.status.success(),
                version,
                error: (!output.status.success()).then(|| {
                    format!(
                        "version check exited with {}",
                        output
                            .status
                            .code()
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| "signal".to_owned())
                    )
                }),
                exit_code: output.status.code(),
            }
        }
        Ok(Err(err)) => TerminalCommandHealth {
            command: command.to_owned(),
            available: false,
            version: None,
            error: Some(err.to_string()),
            exit_code: None,
        },
        Err(_) => TerminalCommandHealth {
            command: command.to_owned(),
            available: false,
            version: None,
            error: Some(format!(
                "version check timed out after {}s",
                timeout.as_secs()
            )),
            exit_code: None,
        },
    }
}

pub(crate) async fn path_writable(path: &Path) -> bool {
    if tokio::fs::create_dir_all(path).await.is_err() {
        return false;
    }
    let probe = path.join(".savfox-terminal-agent-health");
    match tokio::fs::write(&probe, b"ok").await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(probe).await;
            true
        }
        Err(_) => false,
    }
}

pub(crate) fn resolve_cwd(base_cwd: &Path, raw: Option<&str>) -> PathBuf {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return base_cwd.to_path_buf();
    };
    if raw.contains("{{") {
        return base_cwd.to_path_buf();
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        base_cwd.join(path)
    }
}

pub(crate) fn render_terminal_template(template: &str, values: &TerminalTemplateValues) -> String {
    template
        .replace("{{prompt}}", &values.full_prompt)
        .replace("{{user_prompt}}", &values.prompt)
        .replace("{{system_prompt}}", &values.system_prompt)
        .replace("{{agent_id}}", &values.agent_id)
        .replace("{{agent_name}}", &values.agent_name)
        .replace("{{model}}", &values.model)
        .replace("{{session_id}}", &values.session_id)
        .replace("{{agent_home}}", &values.agent_home)
        .replace("{{workspace_dir}}", &values.workspace_dir)
        .replace("{{log_dir}}", &values.log_dir)
}

impl TerminalCommandResolver {
    pub(crate) fn new(base_cwd: impl Into<PathBuf>) -> Self {
        Self {
            base_cwd: base_cwd.into(),
        }
    }

    pub(crate) fn resolve(
        &self,
        template: TerminalCommandTemplate,
        values: &TerminalTemplateValues,
    ) -> anyhow::Result<ResolvedTerminalCommand> {
        let command = render_terminal_template(&template.command, values);
        let command = command.trim().to_owned();
        if command.is_empty() {
            anyhow::bail!("missing terminal command");
        }

        let stdin = template
            .stdin
            .as_deref()
            .map(|stdin| render_terminal_template(stdin, values));
        let args = if template.args.is_empty()
            && template.stdin.is_none()
            && template.default_args_to_prompt
        {
            vec![values.full_prompt.clone()]
        } else {
            template
                .args
                .iter()
                .map(|arg| render_terminal_template(arg, values))
                .collect()
        };
        let cwd = template
            .cwd
            .as_deref()
            .map(|cwd| render_terminal_template(cwd, values));
        let cwd = resolve_cwd(&self.base_cwd, cwd.as_deref());
        let env = template
            .env
            .into_iter()
            .filter_map(|(key, value)| {
                let key = key.trim();
                if key.is_empty() {
                    None
                } else {
                    Some((key.to_owned(), render_terminal_template(&value, values)))
                }
            })
            .collect();
        let timeout_secs = template
            .timeout_secs
            .unwrap_or(template.default_timeout_secs)
            .max(template.min_timeout_secs);

        Ok(ResolvedTerminalCommand {
            command_display: command.clone(),
            spec: TerminalCommandSpec {
                program: command,
                args,
                cwd,
                env,
                stdin,
                timeout: Duration::from_secs(timeout_secs),
                max_output_bytes: template.max_output_bytes,
            },
        })
    }
}

impl TerminalWorkspaceManager {
    pub(crate) async fn prepare(
        request: TerminalWorkspaceRequest,
    ) -> anyhow::Result<TerminalWorkspaceState> {
        let mode = normalize_workspace_mode(&request.mode);
        match mode.as_str() {
            "worktree" | "patch_only" => prepare_git_workspace(request, mode).await,
            "read_only" => Err(anyhow::anyhow!(
                "terminal workspace mode `read_only` is not available yet; use `worktree` or `patch_only` for isolated execution"
            )),
            "shared" | "" => Ok(TerminalWorkspaceState {
                mode: "shared".to_owned(),
                base_cwd: request.base_cwd,
                command_cwd: request.requested_cwd.clone(),
                workspace_path: request.requested_cwd,
                log_dir: request.log_dir,
                git_root: None,
                worktree_path: None,
                cleanup_policy: normalize_cleanup_policy(&request.cleanup_policy),
                cleanup_status: Some("not_applicable".to_owned()),
                patch_path: None,
                diff_summary_path: None,
                diff_summary: None,
            }),
            other => Err(anyhow::anyhow!(
                "unsupported terminal workspace mode `{other}`"
            )),
        }
    }

    pub(crate) async fn finalize(
        workspace: &mut TerminalWorkspaceState,
        success: bool,
    ) -> TerminalWorkspaceFinalization {
        let mut finalization = TerminalWorkspaceFinalization::default();

        if workspace.mode == "patch_only" {
            match write_workspace_patch(workspace).await {
                Ok((patch_path, summary_path, summary)) => {
                    workspace.patch_path = Some(patch_path.clone());
                    workspace.diff_summary_path = Some(summary_path.clone());
                    workspace.diff_summary = Some(summary.clone());
                    finalization.patch_path = Some(patch_path.clone());
                    finalization.diff_summary_path = Some(summary_path);
                    finalization.diff_summary = Some(summary.clone());
                    finalization.events.push(TerminalAgentEvent::Log {
                        stream: "workspace".to_owned(),
                        text: format!("patch_only diff written to {}", patch_path.display()),
                    });
                    if !summary.trim().is_empty() {
                        finalization.events.push(TerminalAgentEvent::Log {
                            stream: "workspace".to_owned(),
                            text: summary,
                        });
                    }
                }
                Err(err) => {
                    let message = format!("patch_only diff generation failed: {err}");
                    workspace.diff_summary = Some(message.clone());
                    finalization.diff_summary = Some(message.clone());
                    finalization
                        .events
                        .push(TerminalAgentEvent::Error { message });
                }
            }
        }

        let cleanup_status = cleanup_workspace(workspace, success).await;
        workspace.cleanup_status = Some(cleanup_status.clone());
        finalization.cleanup_status = Some(cleanup_status.clone());
        if cleanup_status != "not_applicable" && !cleanup_status.starts_with("preserved") {
            finalization.events.push(TerminalAgentEvent::Status {
                status: format!("workspace cleanup: {cleanup_status}"),
            });
        }

        finalization
    }
}

async fn prepare_git_workspace(
    request: TerminalWorkspaceRequest,
    mode: String,
) -> anyhow::Result<TerminalWorkspaceState> {
    let git_root = git_toplevel(&request.base_cwd).await?;
    let requested_relative = request
        .requested_cwd
        .strip_prefix(&git_root)
        .map(Path::to_path_buf)
        .map_err(|_| {
            anyhow::anyhow!(
                "terminal workspace cwd `{}` is outside git root `{}`",
                request.requested_cwd.display(),
                git_root.display()
            )
        })?;
    let worktree_path = request.session_workspace_dir.join(&mode);
    if tokio::fs::metadata(&worktree_path).await.is_err() {
        if let Some(parent) = worktree_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let worktree_arg = worktree_path.to_string_lossy().into_owned();
        run_git(
            &git_root,
            &["worktree", "add", "--detach", &worktree_arg, "HEAD"],
        )
        .await?;
    }

    let command_cwd = worktree_path.join(requested_relative);
    tokio::fs::create_dir_all(&command_cwd).await?;

    Ok(TerminalWorkspaceState {
        mode,
        base_cwd: request.base_cwd,
        command_cwd,
        workspace_path: worktree_path.clone(),
        log_dir: request.log_dir,
        git_root: Some(git_root),
        worktree_path: Some(worktree_path),
        cleanup_policy: normalize_cleanup_policy(&request.cleanup_policy),
        cleanup_status: Some("preserved".to_owned()),
        patch_path: None,
        diff_summary_path: None,
        diff_summary: None,
    })
}

async fn write_workspace_patch(
    workspace: &TerminalWorkspaceState,
) -> anyhow::Result<(PathBuf, PathBuf, String)> {
    let worktree_path = workspace
        .worktree_path
        .as_deref()
        .unwrap_or(workspace.workspace_path.as_path());
    let _ = run_git(worktree_path, &["add", "-N", "."]).await;
    let summary = run_git(worktree_path, &["status", "--short"]).await?;
    let unstaged = run_git(worktree_path, &["diff", "--binary"]).await?;
    let staged = run_git(worktree_path, &["diff", "--cached", "--binary"]).await?;
    let mut patch = String::new();
    if !unstaged.trim().is_empty() {
        patch.push_str(&unstaged);
        if !patch.ends_with('\n') {
            patch.push('\n');
        }
    }
    if !staged.trim().is_empty() {
        patch.push_str(&staged);
        if !patch.ends_with('\n') {
            patch.push('\n');
        }
    }

    tokio::fs::create_dir_all(&workspace.log_dir).await?;
    let patch_path = workspace.log_dir.join("workspace.patch");
    let summary_path = workspace.log_dir.join("workspace-diff-summary.txt");
    tokio::fs::write(&patch_path, patch).await?;
    tokio::fs::write(&summary_path, &summary).await?;
    Ok((patch_path, summary_path, summary))
}

async fn cleanup_workspace(workspace: &TerminalWorkspaceState, success: bool) -> String {
    if workspace.mode == "shared" {
        return "not_applicable".to_owned();
    }
    if !should_cleanup(&workspace.cleanup_policy, success) {
        return if success {
            "preserved".to_owned()
        } else {
            "preserved_after_error".to_owned()
        };
    }

    let Some(worktree_path) = workspace.worktree_path.as_deref() else {
        return "not_applicable".to_owned();
    };
    let Some(git_root) = workspace.git_root.as_deref() else {
        return "failed: missing git root".to_owned();
    };
    let worktree_arg = worktree_path.to_string_lossy().into_owned();
    match run_git(git_root, &["worktree", "remove", "--force", &worktree_arg]).await {
        Ok(_) => "removed".to_owned(),
        Err(err) => format!("failed: {err}"),
    }
}

fn normalize_workspace_mode(mode: &str) -> String {
    match mode.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "" => "shared".to_owned(),
        "shared" => "shared".to_owned(),
        "read_only" | "readonly" | "read_only_workspace" => "read_only".to_owned(),
        "worktree" | "git_worktree" => "worktree".to_owned(),
        "patch_only" | "patch" => "patch_only".to_owned(),
        other => other.to_owned(),
    }
}

fn normalize_cleanup_policy(policy: &str) -> String {
    match policy
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "" => "preserve".to_owned(),
        "delete" | "delete_on_success" | "remove" | "remove_on_success" => {
            "delete_on_success".to_owned()
        }
        "per_turn" => "delete_on_success".to_owned(),
        "delete_always" | "remove_always" => "delete_always".to_owned(),
        "preserve" | "per_session" | "manual" => "preserve".to_owned(),
        other => other.to_owned(),
    }
}

fn should_cleanup(policy: &str, success: bool) -> bool {
    matches!(policy, "delete_always") || (success && matches!(policy, "delete_on_success"))
}

async fn git_toplevel(cwd: &Path) -> anyhow::Result<PathBuf> {
    let output = run_git(cwd, &["rev-parse", "--show-toplevel"]).await?;
    let root = output.trim();
    if root.is_empty() {
        return Err(anyhow::anyhow!("git root lookup returned an empty path"));
    }
    Ok(PathBuf::from(root))
}

async fn run_git(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|err| anyhow::anyhow!("failed to run git in `{}`: {err}", cwd.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            "no output".to_owned()
        };
        let code = output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".to_owned());
        return Err(anyhow::anyhow!(
            "git {:?} exited with {code}: {detail}",
            args
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

impl TerminalSupervisor {
    pub(crate) async fn run(spec: TerminalCommandSpec) -> TerminalSupervisorResult {
        Self::run_with_spawn_callback(spec, |_| async {}).await
    }

    pub(crate) async fn run_with_spawn_callback<F, Fut>(
        spec: TerminalCommandSpec,
        on_spawn: F,
    ) -> TerminalSupervisorResult
    where
        F: FnOnce(Option<u32>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let program = spec.program.trim().to_owned();
        if program.is_empty() {
            return TerminalSupervisorResult {
                stdout: String::new(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                pid: None,
                exit_code: None,
                exit_reason: TerminalExitReason::SpawnError,
                error: Some("missing terminal command".to_owned()),
            };
        }

        match tokio::fs::metadata(&spec.cwd).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return TerminalSupervisorResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                    pid: None,
                    exit_code: None,
                    exit_reason: TerminalExitReason::InvalidCwd,
                    error: Some(format!(
                        "invalid cwd `{}`: not a directory",
                        spec.cwd.display()
                    )),
                };
            }
            Err(err) => {
                return TerminalSupervisorResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                    pid: None,
                    exit_code: None,
                    exit_reason: TerminalExitReason::InvalidCwd,
                    error: Some(format!("invalid cwd `{}`: {err}", spec.cwd.display())),
                };
            }
        }

        let mut cmd = Command::new(&program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        if spec.stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }

        for (key, value) in &spec.env {
            let key = key.trim();
            if !key.is_empty() {
                cmd.env(key, value);
            }
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                return TerminalSupervisorResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                    pid: None,
                    exit_code: None,
                    exit_reason: TerminalExitReason::SpawnError,
                    error: Some(format!(
                        "failed to start terminal delegate `{program}`: {err}"
                    )),
                };
            }
        };

        let pid = child.id();
        on_spawn(pid).await;

        if let Some(stdin_text) = spec.stdin
            && let Some(mut stdin) = child.stdin.take()
        {
            tokio::spawn(async move {
                let _ = stdin.write_all(stdin_text.as_bytes()).await;
                let _ = stdin.shutdown().await;
            });
        }

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_task =
            stdout.map(|stdout| tokio::spawn(read_pipe_limited(stdout, spec.max_output_bytes)));
        let stderr_task =
            stderr.map(|stderr| tokio::spawn(read_pipe_limited(stderr, spec.max_output_bytes)));

        let wait_result = tokio::time::timeout(spec.timeout, child.wait()).await;
        let (exit_reason, exit_code, wait_error) = match wait_result {
            Ok(Ok(status)) if status.success() => {
                (TerminalExitReason::Completed, status.code(), None)
            }
            Ok(Ok(status)) => (TerminalExitReason::NonZero, status.code(), None),
            Ok(Err(err)) => (TerminalExitReason::IoError, None, Some(err.to_string())),
            Err(_) => {
                let _ = child.start_kill();
                let status = child.wait().await.ok();
                (
                    TerminalExitReason::Timeout,
                    status.and_then(|status| status.code()),
                    Some(format!(
                        "terminal delegate `{program}` timed out after {}s",
                        spec.timeout.as_secs()
                    )),
                )
            }
        };

        let (mut stdout, stdout_truncated, stdout_error) =
            collect_pipe_output(stdout_task, "stdout").await;
        let (mut stderr, stderr_truncated, stderr_error) =
            collect_pipe_output(stderr_task, "stderr").await;

        if stdout_truncated {
            stdout.push_str("\n[stdout truncated]");
        }
        if stderr_truncated {
            stderr.push_str("\n[stderr truncated]");
        }

        let read_error = stdout_error.or(stderr_error);
        let exit_reason = if read_error.is_some() {
            TerminalExitReason::IoError
        } else {
            exit_reason
        };
        let error = read_error.or_else(|| match &exit_reason {
            TerminalExitReason::Completed => None,
            TerminalExitReason::NonZero => Some(format!(
                "terminal delegate `{program}` exited with {}",
                exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_owned())
            )),
            TerminalExitReason::Timeout | TerminalExitReason::IoError => wait_error,
            TerminalExitReason::SpawnError | TerminalExitReason::InvalidCwd => unreachable!(),
        });

        TerminalSupervisorResult {
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
            pid,
            exit_code,
            exit_reason,
            error,
        }
    }
}

async fn read_pipe_limited<R>(reader: R, max_output_bytes: usize) -> std::io::Result<(String, bool)>
where
    R: AsyncRead + Unpin,
{
    let mut reader = reader.take((max_output_bytes + 1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await?;
    let truncated = bytes.len() > max_output_bytes;
    if truncated {
        bytes.truncate(max_output_bytes);
    }
    Ok((String::from_utf8_lossy(&bytes).to_string(), truncated))
}

async fn collect_pipe_output(
    task: Option<tokio::task::JoinHandle<std::io::Result<(String, bool)>>>,
    stream: &str,
) -> (String, bool, Option<String>) {
    let Some(task) = task else {
        return (String::new(), false, None);
    };

    match task.await {
        Ok(Ok((text, truncated))) => (text, truncated, None),
        Ok(Err(err)) => (
            String::new(),
            false,
            Some(format!("failed to read terminal delegate {stream}: {err}")),
        ),
        Err(err) => (
            String::new(),
            false,
            Some(format!("terminal delegate {stream} task failed: {err}")),
        ),
    }
}

pub(crate) fn parse_terminal_output(
    io_protocol: &str,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> ParsedTerminalOutput {
    match io_protocol.trim() {
        "jsonl" => parse_jsonl_output(stdout, stderr, exit_code),
        "sentinel" => parse_sentinel_output(stdout, stderr, exit_code),
        "profile" => parse_plain_text_output(stdout, stderr, exit_code),
        _ => parse_plain_text_output(stdout, stderr, exit_code),
    }
}

fn parse_plain_text_output(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> ParsedTerminalOutput {
    let mut events = Vec::new();
    if !stderr.trim().is_empty() {
        events.push(TerminalAgentEvent::Log {
            stream: "stderr".to_owned(),
            text: stderr.to_owned(),
        });
    }

    let reply = if !stdout.trim().is_empty() {
        stdout.trim_end().to_owned()
    } else if !stderr.trim().is_empty() {
        stderr.trim_end().to_owned()
    } else {
        "(no response from terminal delegate)".to_owned()
    };
    events.push(TerminalAgentEvent::Message {
        text: reply.clone(),
    });
    events.push(TerminalAgentEvent::Completed { exit_code });

    ParsedTerminalOutput { reply, events }
}

fn parse_jsonl_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> ParsedTerminalOutput {
    let mut events = Vec::new();
    let mut messages = Vec::new();

    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            events.push(TerminalAgentEvent::Log {
                stream: "stdout".to_owned(),
                text: raw_line.to_owned(),
            });
            continue;
        };
        let kind = value
            .get("event")
            .or_else(|| value.get("type"))
            .or_else(|| value.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("message");
        match kind {
            "message" | "assistant_message" | "delta" => {
                let Some(text) = value
                    .get("text")
                    .or_else(|| value.get("message"))
                    .or_else(|| value.get("content"))
                    .or_else(|| value.get("delta"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                messages.push(text.to_owned());
                events.push(TerminalAgentEvent::Message {
                    text: text.to_owned(),
                });
            }
            "log" => {
                let stream = value
                    .get("stream")
                    .and_then(Value::as_str)
                    .unwrap_or("stdout")
                    .to_owned();
                let text = value
                    .get("text")
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if !text.is_empty() {
                    events.push(TerminalAgentEvent::Log { stream, text });
                }
            }
            "status" => {
                if let Some(status) = value
                    .get("status")
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                {
                    events.push(TerminalAgentEvent::Status {
                        status: status.to_owned(),
                    });
                }
            }
            "error" => {
                if let Some(message) = value
                    .get("message")
                    .or_else(|| value.get("error"))
                    .and_then(Value::as_str)
                {
                    events.push(TerminalAgentEvent::Error {
                        message: message.to_owned(),
                    });
                }
            }
            "completed" | "complete" => {
                events.push(TerminalAgentEvent::Completed {
                    exit_code: value
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .and_then(|code| i32::try_from(code).ok())
                        .or(exit_code),
                });
            }
            _ => events.push(TerminalAgentEvent::Log {
                stream: "stdout".to_owned(),
                text: raw_line.to_owned(),
            }),
        }
    }

    if !stderr.trim().is_empty() {
        events.push(TerminalAgentEvent::Log {
            stream: "stderr".to_owned(),
            text: stderr.to_owned(),
        });
    }
    if !events
        .iter()
        .any(|event| matches!(event, TerminalAgentEvent::Completed { .. }))
    {
        events.push(TerminalAgentEvent::Completed { exit_code });
    }

    let reply = if !messages.is_empty() {
        messages.join("\n")
    } else if !stdout.trim().is_empty() {
        stdout.trim_end().to_owned()
    } else if !stderr.trim().is_empty() {
        stderr.trim_end().to_owned()
    } else {
        "(no response from terminal delegate)".to_owned()
    };

    ParsedTerminalOutput { reply, events }
}

fn parse_sentinel_output(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> ParsedTerminalOutput {
    let mut events = Vec::new();
    let mut messages = Vec::new();
    for raw_line in stdout.lines() {
        let line = raw_line.trim_end();
        if let Some(text) = line.strip_prefix("::savfox-message ") {
            messages.push(text.to_owned());
            events.push(TerminalAgentEvent::Message {
                text: text.to_owned(),
            });
        } else if let Some(text) = line.strip_prefix("::savfox-log ") {
            events.push(TerminalAgentEvent::Log {
                stream: "stdout".to_owned(),
                text: text.to_owned(),
            });
        } else if let Some(message) = line.strip_prefix("::savfox-error ") {
            events.push(TerminalAgentEvent::Error {
                message: message.to_owned(),
            });
        } else if let Some(status) = line.strip_prefix("::savfox-status ") {
            events.push(TerminalAgentEvent::Status {
                status: status.to_owned(),
            });
        } else if line == "::savfox-complete" {
            events.push(TerminalAgentEvent::Completed { exit_code });
        } else if !line.trim().is_empty() {
            messages.push(line.to_owned());
            events.push(TerminalAgentEvent::Message {
                text: line.to_owned(),
            });
        }
    }

    if !stderr.trim().is_empty() {
        events.push(TerminalAgentEvent::Log {
            stream: "stderr".to_owned(),
            text: stderr.to_owned(),
        });
    }
    if !events
        .iter()
        .any(|event| matches!(event, TerminalAgentEvent::Completed { .. }))
    {
        events.push(TerminalAgentEvent::Completed { exit_code });
    }
    let reply = if !messages.is_empty() {
        messages.join("\n")
    } else if !stderr.trim().is_empty() {
        stderr.trim_end().to_owned()
    } else {
        "(no response from terminal delegate)".to_owned()
    };

    ParsedTerminalOutput { reply, events }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::{
        TerminalAgentEvent, TerminalCommandResolver, TerminalCommandSpec, TerminalCommandTemplate,
        TerminalExitReason, TerminalSupervisor, TerminalTemplateValues, TerminalWorkspaceManager,
        TerminalWorkspaceRequest, command_health, parse_terminal_output,
        record_terminal_runtime_metrics, run_git, terminal_profile_preset,
        terminal_profile_presets, terminal_runtime_metrics_snapshot,
    };

    fn supervisor_spec(
        program: &str,
        args: Vec<&str>,
        cwd: std::path::PathBuf,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> TerminalCommandSpec {
        TerminalCommandSpec {
            program: program.to_owned(),
            args: args.into_iter().map(str::to_owned).collect(),
            cwd,
            env: BTreeMap::new(),
            stdin: None,
            timeout,
            max_output_bytes,
        }
    }

    #[cfg(windows)]
    fn shell_output_spec(cwd: std::path::PathBuf, script: &str) -> TerminalCommandSpec {
        supervisor_spec(
            "powershell",
            vec!["-NoProfile", "-Command", script],
            cwd,
            Duration::from_secs(5),
            1024,
        )
    }

    #[cfg(not(windows))]
    fn shell_output_spec(cwd: std::path::PathBuf, script: &str) -> TerminalCommandSpec {
        supervisor_spec("sh", vec!["-c", script], cwd, Duration::from_secs(5), 1024)
    }

    async fn create_test_git_repo() -> Option<tempfile::TempDir> {
        let repo = tempfile::tempdir().expect("create git repo temp dir");
        if run_git(repo.path(), &["init"]).await.is_err() {
            return None;
        }
        run_git(
            repo.path(),
            &["config", "user.email", "savfox@example.test"],
        )
        .await
        .expect("configure git email");
        run_git(repo.path(), &["config", "user.name", "Savfox Test"])
            .await
            .expect("configure git name");
        tokio::fs::create_dir_all(repo.path().join("src"))
            .await
            .expect("create src");
        tokio::fs::write(repo.path().join("README.md"), "base\n")
            .await
            .expect("write readme");
        tokio::fs::write(repo.path().join("src").join("lib.rs"), "pub fn base() {}\n")
            .await
            .expect("write lib");
        run_git(repo.path(), &["add", "."]).await.expect("git add");
        run_git(repo.path(), &["commit", "-m", "initial"])
            .await
            .expect("git commit");
        Some(repo)
    }

    #[test]
    fn terminal_profile_registry_contains_vendor_defaults() {
        let profiles = terminal_profile_presets();
        assert!(profiles.iter().any(|profile| profile.id == "codex"));
        assert!(profiles.iter().any(|profile| profile.id == "claude"));
        assert_eq!(terminal_profile_preset("codex").command, "codex");
        assert_eq!(
            terminal_profile_preset("claude").args,
            &["-p", "{{prompt}}"]
        );
        assert_eq!(terminal_profile_preset("unknown").id, "custom");
    }

    #[test]
    fn terminal_command_resolver_renders_templates_and_defaults() {
        let cwd = tempfile::tempdir().expect("create temp dir");
        let values = TerminalTemplateValues {
            prompt: "hello".to_owned(),
            full_prompt: "system\n\nUser request:\nhello".to_owned(),
            agent_id: "agent-1".to_owned(),
            agent_name: "Agent One".to_owned(),
            model: "default".to_owned(),
            session_id: "018f0000-0000-7000-8000-000000000001".to_owned(),
            agent_home: cwd.path().join("home").to_string_lossy().into_owned(),
            workspace_dir: cwd.path().join("workspace").to_string_lossy().into_owned(),
            log_dir: cwd.path().join("logs").to_string_lossy().into_owned(),
            ..Default::default()
        };
        let mut env = BTreeMap::new();
        env.insert("AGENT".to_owned(), "{{agent_id}}".to_owned());

        let resolved = TerminalCommandResolver::new(cwd.path())
            .resolve(
                TerminalCommandTemplate {
                    command: "tool".to_owned(),
                    args: Vec::new(),
                    stdin: None,
                    cwd: Some("{{workspace_dir}}".to_owned()),
                    env,
                    timeout_secs: Some(1),
                    default_args_to_prompt: true,
                    default_timeout_secs: 300,
                    min_timeout_secs: 5,
                    max_output_bytes: 128,
                },
                &values,
            )
            .expect("resolve command");

        assert_eq!(resolved.spec.program, "tool");
        assert_eq!(resolved.spec.args, vec![values.full_prompt]);
        assert_eq!(resolved.spec.cwd, cwd.path().join("workspace"));
        assert_eq!(
            resolved.spec.env.get("AGENT").map(String::as_str),
            Some("agent-1")
        );
        assert_eq!(resolved.spec.timeout, Duration::from_secs(5));
        assert_eq!(resolved.spec.max_output_bytes, 128);
    }

    #[tokio::test]
    async fn workspace_manager_shared_keeps_requested_cwd() {
        let base = tempfile::tempdir().expect("create base temp dir");
        let requested = base.path().join("nested");
        tokio::fs::create_dir_all(&requested)
            .await
            .expect("create requested cwd");
        let session = tempfile::tempdir().expect("create session temp dir");
        let mut workspace = TerminalWorkspaceManager::prepare(TerminalWorkspaceRequest {
            mode: "shared".to_owned(),
            base_cwd: base.path().to_path_buf(),
            requested_cwd: requested.clone(),
            session_workspace_dir: session.path().join("workspace"),
            log_dir: session.path().join("logs"),
            session_id: "018f0000-0000-7000-8000-000000000701".to_owned(),
            cleanup_policy: "delete_on_success".to_owned(),
        })
        .await
        .expect("prepare shared workspace");

        assert_eq!(workspace.mode, "shared");
        assert_eq!(workspace.command_cwd, requested);
        assert_eq!(workspace.cleanup_status.as_deref(), Some("not_applicable"));
        let finalization = TerminalWorkspaceManager::finalize(&mut workspace, true).await;
        assert_eq!(
            finalization.cleanup_status.as_deref(),
            Some("not_applicable")
        );
    }

    #[tokio::test]
    async fn workspace_manager_read_only_reports_capability_gap() {
        let base = tempfile::tempdir().expect("create base temp dir");
        let session = tempfile::tempdir().expect("create session temp dir");
        let err = TerminalWorkspaceManager::prepare(TerminalWorkspaceRequest {
            mode: "read-only".to_owned(),
            base_cwd: base.path().to_path_buf(),
            requested_cwd: base.path().to_path_buf(),
            session_workspace_dir: session.path().join("workspace"),
            log_dir: session.path().join("logs"),
            session_id: "018f0000-0000-7000-8000-000000000704".to_owned(),
            cleanup_policy: "preserve".to_owned(),
        })
        .await
        .expect_err("read_only is not supported yet");

        assert!(err.to_string().contains("read_only"));
    }

    #[tokio::test]
    async fn workspace_manager_worktree_creates_session_worktree_and_can_cleanup() {
        let Some(repo) = create_test_git_repo().await else {
            return;
        };
        let session = tempfile::tempdir().expect("create session temp dir");
        let mut workspace = TerminalWorkspaceManager::prepare(TerminalWorkspaceRequest {
            mode: "worktree".to_owned(),
            base_cwd: repo.path().to_path_buf(),
            requested_cwd: repo.path().join("src"),
            session_workspace_dir: session.path().join("workspace"),
            log_dir: session.path().join("logs"),
            session_id: "018f0000-0000-7000-8000-000000000702".to_owned(),
            cleanup_policy: "delete_on_success".to_owned(),
        })
        .await
        .expect("prepare worktree workspace");

        assert_eq!(workspace.mode, "worktree");
        assert!(workspace.workspace_path.starts_with(session.path()));
        assert!(workspace.workspace_path.join("README.md").is_file());
        assert!(workspace.command_cwd.ends_with("src"));
        assert!(workspace.command_cwd.starts_with(&workspace.workspace_path));

        let finalization = TerminalWorkspaceManager::finalize(&mut workspace, true).await;

        assert_eq!(finalization.cleanup_status.as_deref(), Some("removed"));
        assert!(!workspace.workspace_path.exists());
    }

    #[tokio::test]
    async fn workspace_manager_patch_only_writes_patch_and_summary() {
        let Some(repo) = create_test_git_repo().await else {
            return;
        };
        let session = tempfile::tempdir().expect("create session temp dir");
        let mut workspace = TerminalWorkspaceManager::prepare(TerminalWorkspaceRequest {
            mode: "patch_only".to_owned(),
            base_cwd: repo.path().to_path_buf(),
            requested_cwd: repo.path().to_path_buf(),
            session_workspace_dir: session.path().join("workspace"),
            log_dir: session.path().join("logs"),
            session_id: "018f0000-0000-7000-8000-000000000703".to_owned(),
            cleanup_policy: "preserve".to_owned(),
        })
        .await
        .expect("prepare patch_only workspace");
        tokio::fs::write(workspace.command_cwd.join("README.md"), "changed\n")
            .await
            .expect("modify tracked file");
        tokio::fs::write(workspace.command_cwd.join("new.txt"), "new file\n")
            .await
            .expect("write untracked file");

        let finalization = TerminalWorkspaceManager::finalize(&mut workspace, true).await;

        let patch_path = finalization.patch_path.expect("patch path");
        let summary = finalization.diff_summary.expect("diff summary");
        let patch = tokio::fs::read_to_string(patch_path)
            .await
            .expect("read patch");
        assert!(patch.contains("+changed"));
        assert!(patch.contains("new.txt"));
        assert!(summary.contains("README.md"));
        assert!(summary.contains("new.txt"));
        assert_eq!(finalization.cleanup_status.as_deref(), Some("preserved"));
        assert!(finalization.events.iter().any(|event| {
            matches!(
                event,
                TerminalAgentEvent::Log { stream, text }
                    if stream == "workspace" && text.contains("patch_only diff")
            )
        }));
    }

    #[tokio::test]
    async fn terminal_supervisor_captures_stdout_and_stderr() {
        let cwd = tempfile::tempdir().expect("create temp dir");
        #[cfg(windows)]
        let spec = shell_output_spec(
            cwd.path().to_path_buf(),
            "[Console]::Out.Write('hello'); [Console]::Error.Write('warn')",
        );
        #[cfg(not(windows))]
        let spec = shell_output_spec(cwd.path().to_path_buf(), "printf hello; printf warn >&2");

        let result = TerminalSupervisor::run(spec).await;

        assert_eq!(result.exit_reason, TerminalExitReason::Completed);
        assert_eq!(result.stdout, "hello");
        assert_eq!(result.stderr, "warn");
        assert_eq!(result.exit_code, Some(0));
        assert!(result.pid.is_some());
    }

    #[tokio::test]
    async fn terminal_supervisor_truncates_large_output() {
        let cwd = tempfile::tempdir().expect("create temp dir");
        #[cfg(windows)]
        let mut spec =
            shell_output_spec(cwd.path().to_path_buf(), "[Console]::Out.Write('abcdef')");
        #[cfg(not(windows))]
        let mut spec = shell_output_spec(cwd.path().to_path_buf(), "printf abcdef");
        spec.max_output_bytes = 3;

        let result = TerminalSupervisor::run(spec).await;

        assert_eq!(result.exit_reason, TerminalExitReason::Completed);
        assert!(result.stdout_truncated);
        assert_eq!(result.stdout, "abc\n[stdout truncated]");
    }

    #[tokio::test]
    async fn terminal_supervisor_reports_non_zero_exit() {
        let cwd = tempfile::tempdir().expect("create temp dir");
        #[cfg(windows)]
        let spec = shell_output_spec(
            cwd.path().to_path_buf(),
            "[Console]::Error.Write('fail'); exit 7",
        );
        #[cfg(not(windows))]
        let spec = shell_output_spec(cwd.path().to_path_buf(), "printf fail >&2; exit 7");

        let result = TerminalSupervisor::run(spec).await;

        assert_eq!(result.exit_reason, TerminalExitReason::NonZero);
        assert_eq!(result.exit_code, Some(7));
        assert!(result.stderr.contains("fail"));
        assert!(result.error.as_deref().is_some_and(|err| err.contains("7")));
    }

    #[tokio::test]
    async fn terminal_supervisor_reports_invalid_cwd() {
        let cwd = tempfile::tempdir().expect("create temp dir");
        let spec = supervisor_spec(
            "__not_started__",
            Vec::new(),
            cwd.path().join("missing"),
            Duration::from_secs(1),
            1024,
        );

        let result = TerminalSupervisor::run(spec).await;

        assert_eq!(result.exit_reason, TerminalExitReason::InvalidCwd);
        assert!(result.pid.is_none());
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|err| err.contains("invalid cwd"))
        );
    }

    #[tokio::test]
    async fn terminal_supervisor_reports_timeout() {
        let cwd = tempfile::tempdir().expect("create temp dir");
        #[cfg(windows)]
        let mut spec = shell_output_spec(cwd.path().to_path_buf(), "Start-Sleep -Seconds 2");
        #[cfg(not(windows))]
        let mut spec = shell_output_spec(cwd.path().to_path_buf(), "sleep 2");
        spec.timeout = Duration::from_millis(100);

        let result = TerminalSupervisor::run(spec).await;

        assert_eq!(result.exit_reason, TerminalExitReason::Timeout);
        assert!(result.pid.is_some());
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|err| err.contains("timed out"))
        );
    }

    #[tokio::test]
    async fn command_health_reports_missing_command() {
        let health = command_health(
            "__savfox_missing_terminal_command__",
            &["--version"],
            Duration::from_secs(1),
        )
        .await;

        assert!(!health.available);
        assert!(health.error.is_some());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn command_health_reports_available_command() {
        let health = command_health("cmd", &["/C", "echo savfox-ok"], Duration::from_secs(1)).await;

        assert!(health.available);
        assert_eq!(health.version.as_deref(), Some("savfox-ok"));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn command_health_reports_available_command() {
        let health =
            command_health("sh", &["-c", "printf savfox-ok"], Duration::from_secs(1)).await;

        assert!(health.available);
        assert_eq!(health.version.as_deref(), Some("savfox-ok"));
    }

    #[test]
    fn plain_text_adapter_returns_stdout_reply_and_stderr_log() {
        let parsed = parse_terminal_output("plain_text", "hello\n", "warning\n", Some(0));

        assert_eq!(parsed.reply, "hello");
        assert_eq!(
            parsed.events[0],
            TerminalAgentEvent::Log {
                stream: "stderr".to_owned(),
                text: "warning\n".to_owned(),
            }
        );
        assert!(matches!(
            parsed.events.last(),
            Some(TerminalAgentEvent::Completed { exit_code: Some(0) })
        ));
    }

    #[test]
    fn profile_adapter_currently_falls_back_to_plain_text() {
        let parsed = parse_terminal_output("profile", "hello\n", "", Some(0));

        assert_eq!(parsed.reply, "hello");
        assert!(matches!(
            parsed.events.last(),
            Some(TerminalAgentEvent::Completed { exit_code: Some(0) })
        ));
    }

    #[test]
    fn jsonl_adapter_extracts_messages_and_preserves_unknown_logs() {
        let parsed = parse_terminal_output(
            "jsonl",
            "{\"event\":\"status\",\"status\":\"thinking\"}\n{\"event\":\"message\",\"text\":\"hello\"}\n{\"event\":\"future\",\"raw\":true}\n",
            "",
            Some(0),
        );

        assert_eq!(parsed.reply, "hello");
        assert!(parsed.events.iter().any(|event| {
            matches!(
                event,
                TerminalAgentEvent::Status { status } if status == "thinking"
            )
        }));
        assert!(parsed.events.iter().any(|event| {
            matches!(
                event,
                TerminalAgentEvent::Log { stream, text }
                    if stream == "stdout" && text.contains("\"future\"")
            )
        }));
    }

    #[test]
    fn sentinel_adapter_uses_markers_and_plain_lines() {
        let parsed = parse_terminal_output(
            "sentinel",
            "::savfox-status running\nplain reply\n::savfox-message final\n::savfox-complete\n",
            "",
            Some(0),
        );

        assert_eq!(parsed.reply, "plain reply\nfinal");
        assert!(parsed.events.iter().any(|event| {
            matches!(
                event,
                TerminalAgentEvent::Status { status } if status == "running"
            )
        }));
        assert!(matches!(
            parsed.events.last(),
            Some(TerminalAgentEvent::Completed { exit_code: Some(0) })
        ));
    }

    #[test]
    fn terminal_runtime_metrics_record_exit_reasons() {
        let before = terminal_runtime_metrics_snapshot();
        let before_timeout_count = before.exit_reasons.get("timeout").copied().unwrap_or(0);

        record_terminal_runtime_metrics(&TerminalExitReason::Timeout, Duration::from_millis(12));

        let after = terminal_runtime_metrics_snapshot();
        assert!(after.spawn_count >= before.spawn_count.saturating_add(1));
        assert!(after.timeout_count >= before.timeout_count.saturating_add(1));
        assert!(after.error_count >= before.error_count.saturating_add(1));
        assert!(
            after.exit_reasons.get("timeout").copied().unwrap_or(0)
                >= before_timeout_count.saturating_add(1)
        );
        assert!(after.last_duration_ms.is_some());
    }
}
