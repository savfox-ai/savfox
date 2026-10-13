use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use futures_util::stream::{FuturesUnordered, StreamExt};
use savfox_gateway_shared::ClaudeCodeConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::protocol::{self, FRAME_LIMIT, State};
use super::runtime::{Execution, RunOutput, Runtime};
use crate::agent_terminal_delegate::TerminalEventSink;
use crate::terminal_agent::{
    TerminalCommandSpec, TerminalExitReason, TerminalSupervisor, TerminalSupervisorResult,
    TerminalTemplateValues,
};

pub(crate) struct RunOptions {
    pub(crate) spec: TerminalCommandSpec,
    pub(crate) config: ClaudeCodeConfig,
    pub(crate) values: TerminalTemplateValues,
    pub(crate) binding_path: PathBuf,
    pub(crate) resume: bool,
    pub(crate) bridge: String,
    pub(crate) sink: Option<TerminalEventSink>,
    pub(crate) runtime: Runtime,
    pub(crate) execution: Execution,
    pub(crate) run_id: String,
    pub(crate) cancel: watch::Receiver<bool>,
}

#[derive(Serialize, Deserialize)]
struct Binding {
    session_id: String,
    fingerprint: String,
    version: String,
    status: String,
    run_id: String,
}

pub(super) async fn atomic_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let parent = path.parent().context("metadata path has no parent")?;
    tokio::fs::create_dir_all(parent).await?;
    let tmp = parent.join(format!(".{}.tmp", uuid::Uuid::now_v7()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&tmp).await?;
    file.write_all(&serde_json::to_vec(value)?).await?;
    file.sync_all().await?;
    drop(file);
    if let Err(error) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(error.into());
    }
    #[cfg(unix)]
    tokio::fs::File::open(parent).await?.sync_all().await?;
    Ok(())
}

pub(crate) async fn run(mut options: RunOptions) -> RunOutput {
    let mut result = RunOutput {
        supervised: TerminalSupervisorResult {
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            pid: None,
            exit_code: None,
            exit_reason: TerminalExitReason::IoError,
            error: None,
        },
        usage: None,
        vendor_usage: None,
        run_id: options.run_id.clone(),
    };
    let outcome = run_inner(&mut options, &mut result).await;
    if let Err(error) = outcome {
        if result.supervised.exit_reason.is_success() {
            result.supervised.exit_reason = TerminalExitReason::IoError;
        }
        result.supervised.error = Some(error.to_string());
    }
    let status = if result.supervised.exit_reason.is_success() {
        "completed"
    } else if *options.cancel.borrow() {
        "cancelled"
    } else if matches!(result.supervised.exit_reason, TerminalExitReason::Timeout) {
        "timeout"
    } else {
        "error"
    };
    options
        .runtime
        .finish(&options.execution, result.clone(), status)
        .await;
    result
}

async fn preflight(options: &mut RunOptions) -> anyhow::Result<String> {
    super::validate(&options.config, &options.spec.args)?;
    options.spec.cwd = tokio::fs::canonicalize(&options.spec.cwd)
        .await
        .context("Claude workspace directory unavailable")?;
    let program = Path::new(&options.spec.program);
    if program.components().count() > 1 {
        let path = if program.is_absolute() {
            program.to_owned()
        } else {
            options.spec.cwd.join(program)
        };
        options.spec.program = tokio::fs::canonicalize(path)
            .await
            .context("Claude executable unavailable")?
            .to_string_lossy()
            .into_owned();
    } else {
        let path = options
            .spec
            .env
            .get("PATH")
            .cloned()
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        let resolved = std::env::split_paths(&path)
            .map(|directory| directory.join(program))
            .find(|candidate| candidate.is_file())
            .context("Claude executable not found in PATH")?;
        options.spec.program = tokio::fs::canonicalize(resolved)
            .await?
            .to_string_lossy()
            .into_owned();
    }
    ensure!(
        cfg!(unix),
        "claude_stream is currently supported on Unix; Windows process-tree cleanup has not been validated"
    );
    ensure!(
        options.bridge == "prompt" || options.bridge == "required",
        "claude_stream requires prompt or required approval bridge"
    );
    if options.bridge == "required" {
        ensure!(
            options.runtime.can_approve(&options.execution).await,
            "required Claude approval bridge has no authorized client"
        );
    }
    for (arg, required) in [("--version", false), ("--help", true)] {
        let mut spec = options.spec.clone();
        spec.args = vec![arg.to_owned()];
        spec.stdin = None;
        spec.timeout = Duration::from_secs(options.config.initialize_timeout_secs);
        spec.max_output_bytes = 64 * 1024;
        let output = tokio::select! {
            _ = options.cancel.changed() => bail!("Claude execution cancelled during preflight"),
            output = TerminalSupervisor::run(spec) => output,
        };
        ensure!(
            output.exit_reason.is_success(),
            "Claude preflight {arg} failed: {}",
            output.error.unwrap_or_default()
        );
        if required {
            for flag in [
                "--input-format",
                "--output-format",
                "--include-partial-messages",
                "--session-id",
                "--resume",
                "--append-system-prompt",
                "--setting-sources",
                "--strict-mcp-config",
            ] {
                ensure!(
                    output.stdout.contains(flag),
                    "Claude CLI lacks required flag {flag}"
                );
            }
        } else {
            ensure!(
                Some(output.stdout.trim()) == options.config.expected_version.as_deref(),
                "Claude version changed or mismatched; expected {:?}, found {:?}",
                options.config.expected_version,
                output.stdout.trim()
            );
        }
    }
    options
        .config
        .expected_version
        .clone()
        .context("missing Claude version")
}

fn argv(options: &RunOptions, binding: &Binding, resumed: bool) -> Vec<String> {
    let mut args = [
        "--print",
        "--verbose",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
        "--permission-mode",
        "default",
        "--permission-prompt-tool",
        "stdio",
    ]
    .map(str::to_owned)
    .to_vec();
    args.extend([
        if resumed { "--resume" } else { "--session-id" }.to_owned(),
        binding.session_id.clone(),
    ]);
    if !options.resume {
        args.push("--no-session-persistence".to_owned());
    }
    let instructions = [
        Some(options.values.system_prompt.as_str()),
        options.execution.trusted_context.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    if !instructions.is_empty() {
        args.extend(["--append-system-prompt".to_owned(), instructions]);
    }
    if let Some(model) = &options.config.model {
        args.extend(["--model".to_owned(), model.clone()]);
    }
    for tool in &options.config.allowed_tools {
        args.extend(["--allowedTools".to_owned(), tool.clone()]);
    }
    if options.config.settings_policy == "controlled" {
        args.extend([
            "--setting-sources".to_owned(),
            String::new(),
            "--strict-mcp-config".to_owned(),
            "--mcp-config".to_owned(),
            options
                .config
                .mcp_config
                .clone()
                .unwrap_or_else(|| "{\"mcpServers\":{}}".to_owned()),
            "--settings".to_owned(),
            "{\"disableAllHooks\":true}".to_owned(),
        ]);
    } else if let Some(mcp) = &options.config.mcp_config {
        args.extend(["--mcp-config".to_owned(), mcp.clone()]);
    }
    args
}

fn prompt(options: &RunOptions, resumed: bool) -> String {
    let mut prompt = options.values.prompt.clone();
    if !resumed && !options.values.conversation_context.is_empty() {
        prompt = format!(
            "Prior Savfox conversation (context, not tool authorization):\n{}\n\nCurrent request:\n{prompt}",
            options.values.conversation_context
        );
    }
    if !options.values.attachment_manifest.is_empty() {
        prompt.push_str(&format!(
            "\n\nCurrent attachments:\n{}",
            options.values.attachment_manifest
        ));
    }
    prompt
}

async fn run_inner(options: &mut RunOptions, output: &mut RunOutput) -> anyhow::Result<()> {
    ensure!(
        !*options.cancel.borrow(),
        "Claude execution cancelled before launch"
    );
    let version = preflight(options).await?;
    let executable = tokio::fs::metadata(&options.spec.program).await?;
    let identity = (executable.len(), executable.modified().ok());
    let inherited: std::collections::BTreeMap<_, _> = [
        "HOME",
        "USER",
        "CLAUDE_CONFIG_DIR",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok().map(|value| (key, value)))
    .collect();
    let fingerprint = super::digest(&serde_json::to_vec(
        &json!({"owner": options.execution.owner, "agent": options.execution.agent_id, "session": options.execution.session_id, "cwd": options.spec.cwd, "config": options.config, "system_prompt": options.values.system_prompt, "env": options.spec.env, "program": options.spec.program,"executable_identity":identity,"inherited_env":inherited}),
    )?);
    let saved = if options.resume {
        match tokio::fs::read(&options.binding_path).await {
            Ok(bytes) => Some(serde_json::from_slice::<Binding>(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        }
    } else {
        None
    };
    let resumed = saved.is_some();
    let mut binding = if let Some(binding) = saved {
        ensure!(
            binding.fingerprint == fingerprint && binding.version == version,
            "Claude session binding changed (identity, directory, policy, system instructions, model or version); create a new Savfox chat"
        );
        ensure!(
            binding.status != "starting",
            "previous Claude session startup is uncertain; inspect its run before explicitly creating a new chat"
        );
        binding
    } else {
        Binding {
            session_id: uuid::Uuid::now_v7().to_string(),
            fingerprint,
            version,
            status: "starting".to_owned(),
            run_id: options.run_id.clone(),
        }
    };
    binding.status = "starting".to_owned();
    binding.run_id = options.run_id.clone();
    if options.resume {
        atomic_json(&options.binding_path, &binding).await?;
    }
    let mut command = Command::new(&options.spec.program);
    command
        .args(argv(options, &binding, resumed))
        .current_dir(&options.spec.cwd)
        .envs(&options.spec.env);
    command.env("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1");
    if options.config.settings_policy == "controlled" {
        command.env("DISABLE_AUTOUPDATER", "1");
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().context("failed to start Claude Code")?;
    output.supervised.pid = child.id();
    let mut stdin = child.stdin.take().context("Claude stdin unavailable")?;
    let stdout = child.stdout.take().context("Claude stdout unavailable")?;
    let mut stderr = child.stderr.take().context("Claude stderr unavailable")?;
    let stderr_task = tokio::spawn(async move {
        let mut retained = Vec::new();
        let mut buffer = [0_u8; 8192];
        let mut truncated = false;
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) => break,
                Ok(count) => {
                    let take = count.min((64 * 1024_usize).saturating_sub(retained.len()));
                    retained.extend_from_slice(&buffer[..take]);
                    truncated |= take < count;
                }
                Err(_) => break,
            }
        }
        let mut text = String::from_utf8_lossy(&retained).into_owned();
        if text.len() > 64 * 1024 {
            let mut end = 64 * 1024;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            truncated = true;
        }
        (text, truncated)
    });
    let (tx, mut frames) = mpsc::channel(16);
    let reader_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut bytes = Vec::new();
            let read = (&mut reader)
                .take(FRAME_LIMIT as u64 + 1)
                .read_until(b'\n', &mut bytes)
                .await;
            let value = match read {
                Ok(0) => break,
                Ok(count) if count > FRAME_LIMIT => {
                    Err(anyhow::anyhow!("Claude frame exceeds byte limit"))
                }
                Ok(_) if bytes.last() != Some(&b'\n') => {
                    Err(anyhow::anyhow!("truncated Claude JSON frame"))
                }
                Ok(_) => {
                    serde_json::from_slice::<Value>(&bytes).context("invalid Claude JSON frame")
                }
                Err(error) => Err(error.into()),
            };
            let failed = value.is_err();
            if tx.send(value).await.is_err() || failed {
                break;
            }
        }
    });
    let mut state = State::default();
    let session_result = drive(
        options,
        &binding,
        resumed,
        &mut stdin,
        &mut frames,
        &mut state,
    )
    .await;
    // Closing input permits CLI shutdown after finite background tasks finish.
    drop(stdin);
    let grace = Duration::from_secs(options.config.shutdown_timeout_secs);
    let ended = if session_result.is_ok() {
        tokio::time::timeout(grace, child.wait())
            .await
            .ok()
            .and_then(Result::ok)
    } else {
        None
    };
    let forced = ended.is_none();
    if forced {
        terminate_group(child.id());
        let _ = child.start_kill();
    }
    let exit = match ended {
        Some(exit) => Some(exit),
        None => tokio::time::timeout(grace, child.wait())
            .await
            .ok()
            .and_then(Result::ok),
    };
    // Reap remaining in-group children even when the CLI itself exited.
    terminate_group(output.supervised.pid);
    reader_task.abort();
    let _ = reader_task.await;
    let stderr_abort = stderr_task.abort_handle();
    if let Ok(Ok((stderr, truncated))) = tokio::time::timeout(grace, stderr_task).await {
        output.supervised.stderr = stderr;
        output.supervised.stderr_truncated = truncated;
    } else {
        stderr_abort.abort();
    }
    output.supervised.exit_code = exit.as_ref().and_then(std::process::ExitStatus::code);
    output.supervised.stdout = state.reply_snapshot();
    output.usage = state.usage;
    output.vendor_usage = state.vendor_usage;
    match session_result {
        Ok(()) => {
            let cli_error = state
                .result
                .as_ref()
                .is_some_and(|result| result["is_error"] == true || result["subtype"] != "success");
            ensure!(
                exit.is_some(),
                "Claude process cleanup was not confirmed; workspace retained"
            );
            output.supervised.exit_reason =
                if cli_error || exit.as_ref().is_some_and(|exit| !exit.success()) {
                    TerminalExitReason::NonZero
                } else {
                    TerminalExitReason::Completed
                };
            binding.status = if cli_error { "cli_error" } else { "completed" }.to_owned();
            if options.resume {
                atomic_json(&options.binding_path, &binding).await?;
            }
            if cli_error {
                bail!("Claude returned an error result; query the execution for retained output");
            }
            if forced {
                output.supervised.exit_reason = TerminalExitReason::IoError;
                bail!(
                    "Claude returned a result but required forced process cleanup; output retained"
                );
            }
            Ok(())
        }
        Err(error) => {
            binding.status = if *options.cancel.borrow() {
                "cancelled"
            } else {
                "unknown"
            }
            .to_owned();
            if options.resume {
                let _ = atomic_json(&options.binding_path, &binding).await;
            }
            if error.to_string().contains("deadline") {
                output.supervised.exit_reason = TerminalExitReason::Timeout;
            }
            Err(error)
        }
    }
}

// Kept local to drive: writes are serialized by the loop, including decisions
// arriving from independent approval waiters. Never share ChildStdin with UI.

async fn send(stdin: &mut tokio::process::ChildStdin, message: Value) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(&message)?;
    ensure!(
        bytes.len() <= FRAME_LIMIT,
        "Claude input frame exceeds byte limit"
    );
    bytes.push(b'\n');
    tokio::time::timeout(Duration::from_secs(5), async {
        stdin.write_all(&bytes).await?;
        stdin.flush().await
    })
    .await
    .context("Claude write deadline exceeded")??;
    Ok(())
}

async fn drive(
    options: &mut RunOptions,
    binding: &Binding,
    resumed: bool,
    stdin: &mut tokio::process::ChildStdin,
    frames: &mut mpsc::Receiver<anyhow::Result<Value>>,
    state: &mut State,
) -> anyhow::Result<()> {
    let init_id = uuid::Uuid::now_v7().to_string();
    send(stdin, json!({"type":"control_request","request_id":init_id,"request":{"subtype":"initialize","hooks":null}})).await?;
    type ApprovalWait =
        futures_util::future::BoxFuture<'static, (String, String, Value, bool, Option<Value>)>;
    let mut waits: FuturesUnordered<ApprovalWait> = FuturesUnordered::new();
    let mut pending = HashMap::<String, String>::new();
    let mut completed = VecDeque::<String>::new();
    let init_deadline =
        Instant::now() + Duration::from_secs(options.config.initialize_timeout_secs);
    let deadline = Instant::now()
        + options.spec.timeout
        + Duration::from_secs(options.config.approval_timeout_secs);
    let mut settled = None;
    let result: anyhow::Result<()> = async {
        loop {
            if *options.cancel.borrow() {
                let _ = send(stdin, json!({"type":"control_request","request_id":uuid::Uuid::now_v7().to_string(),"request":{"subtype":"interrupt"}})).await;
                let until = Instant::now() + Duration::from_secs(1);
                while tokio::time::timeout_at(until, frames.recv()).await.is_ok_and(|frame| frame.is_some()) {}
                bail!("Claude execution cancelled");
            }
            let next = settled.unwrap_or(deadline).min(deadline);
            tokio::select! {
                biased;
                _ = options.cancel.changed() => {},
                _ = tokio::time::sleep_until(init_deadline), if !state.initialized => bail!("Claude initialize deadline exceeded"),
                _ = tokio::time::sleep_until(next) => {
                    if state.ready() && pending.is_empty() { return Ok(()); }
                    bail!("Claude execution or background-task deadline exceeded");
                }
                decision = waits.next(), if !waits.is_empty() => {
                    let (control_id, approval_id, input, question, decision) = decision.context("approval waiter disappeared")?;
                    if pending.remove(&control_id).is_none() { continue; }
                    options.runtime.remove_approval(&approval_id, "approval timeout or execution ended").await;
                    let response = match decision {
                        Some(decision) if decision["decision"] == "approve-once" => {
                            let updated = if question { protocol::approved_input(&input, &decision["answers"])? } else { input };
                            json!({"behavior":"allow","updatedInput":updated})
                        }
                        Some(decision) if decision["decision"] == "abort" => { options.runtime.cancel(&options.execution.owner, &options.run_id).await; protocol::deny("User stopped this execution") }
                        _ => protocol::deny("Permission denied or approval expired"),
                    };
                    send(stdin, protocol::control_response(&control_id, response)).await?;
                    completed.push_back(control_id);
                    if completed.len() > 128 { completed.pop_front(); }
                }
                frame = frames.recv() => {
                    let Some(frame) = frame else { ensure!(state.ready() && pending.is_empty(), "Claude stdout closed before result and background completion"); return Ok(()); };
                    let message = frame?;
                    if let Some(session) = message["session_id"].as_str() { ensure!(session == binding.session_id, "Claude control message has a different session identity"); }
                    ensure!(message.is_object() && message["type"].is_string(), "invalid Claude protocol envelope");
                    if message["type"] == "control_response" {
                        let response = &message["response"];
                        if response["request_id"] == init_id && !state.initialized {
                            ensure!(response["subtype"] == "success" && response["response"].is_object(), "Claude initialize failed");
                            state.initialized = true;
                            let running = Binding { session_id: binding.session_id.clone(), fingerprint: binding.fingerprint.clone(), version: binding.version.clone(), status: "running".to_owned(), run_id: binding.run_id.clone() };
                            if options.resume { atomic_json(&options.binding_path, &running).await?; }
                            options.runtime.running(&options.execution).await?;
                            send(stdin, json!({"type":"user","message":{"role":"user","content":prompt(options, resumed)},"parent_tool_use_id":null,"session_id":binding.session_id})).await?;
                        } else { bail!("unmatched Claude control response"); }
                    } else if message["type"] == "control_request" {
                        let id = message["request_id"].as_str().filter(|id| !id.is_empty() && id.len() <= 256).context("control request missing identity")?.to_owned();
                        ensure!(!pending.contains_key(&id) && !completed.contains(&id), "duplicate Claude control request");
                        let request = &message["request"];
                        if request["subtype"] != "can_use_tool" {
                            send(stdin, protocol::unsupported(&id)).await?;
                            continue;
                        }
                        ensure!(state.initialized, "permission requested before initialize");
                        let tool = request["tool_name"].as_str().filter(|tool| !tool.is_empty()).context("permission missing tool name")?;
                        let input = request.get("input").filter(|input| input.is_object()).context("permission missing original tool input")?.clone();
                        ensure!(request["tool_use_id"].as_str().is_some_and(|id| !id.is_empty()), "permission missing tool use identity");
                        if tool == "AskUserQuestion" { protocol::questions(&input)?; }
                        if !options.runtime.can_approve(&options.execution).await {
                            send(stdin, protocol::control_response(&id, protocol::deny("No authorized approval client is available"))).await?;
                            completed.push_back(id);
                            if completed.len() > 128 { completed.pop_front(); }
                            continue;
                        }
                        let ttl = Duration::from_secs(options.config.approval_timeout_secs);
                        let (approval_id, receiver) = options.runtime.approval(&options.execution, &options.run_id, &id, tool, &input, &options.spec.cwd, ttl).await?;
                        pending.insert(id.clone(), approval_id.clone());
                        let question = tool == "AskUserQuestion";
                        waits.push(Box::pin(async move {
                            let decision = tokio::time::timeout(ttl, receiver).await.ok().and_then(Result::ok);
                            (id, approval_id, input, question, decision)
                        }));
                    } else if message["type"] == "control_cancel_request" {
                        if let Some(id) = message["request_id"].as_str() && let Some(approval_id) = pending.remove(id) { options.runtime.remove_approval(&approval_id, "CLI cancelled request").await; }
                    } else {
                        state.event(&message, options.sink.as_ref(), &binding.session_id)?;
                    }
                    settled = if state.ready() && pending.is_empty() { Some(Instant::now() + Duration::from_millis(250)) } else { None };
                }
            }
        }
    }.await;
    for approval_id in pending.values() {
        options
            .runtime
            .remove_approval(approval_id, "Claude execution ended")
            .await;
    }
    result
}

#[cfg(unix)]
fn terminate_group(pid: Option<u32>) {
    if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

#[cfg(not(unix))]
fn terminate_group(_pid: Option<u32>) {}
