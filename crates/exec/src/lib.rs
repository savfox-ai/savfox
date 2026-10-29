// - In the default output mode, it is paramount that the only thing written to stdout is the final
//   message (if any).
// - In --json mode, stdout must be valid JSONL, one event per line.
// For both modes, any other output must be written to stderr.
#![deny(clippy::print_stdout)]
#![allow(missing_debug_implementations)]
#![allow(
    clippy::exit,
    clippy::future_not_send,
    clippy::manual_let_else,
    clippy::needless_continue,
    clippy::needless_pass_by_ref_mut,
    clippy::unused_self
)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod cli;
mod event_processor;
mod event_processor_with_human_output;
pub mod event_processor_with_jsonl_output;
pub mod exec_events;

use std::collections::HashSet;
use std::future::Future;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::sync::Arc;

pub use cli::{Cli, Command, ReviewArgs};
use event_processor_with_human_output::EventProcessorWithHumanOutput;
use event_processor_with_jsonl_output::EventProcessorWithJsonOutput;
use savfox_common::oss::{
    ensure_oss_provider_ready, get_default_model_for_oss_provider, ollama_chat_deprecation_notice,
};
use savfox_core::auth::enforce_login_restrictions;
use savfox_core::config::{
    Config, ConfigBuilder, ConfigOverrides, ConfigToml, resolve_oss_provider,
};
use savfox_core::config_loader::{
    CloudRequirementsLoader, ConfigLoadError, format_config_error_with_source,
};
use savfox_core::default_client::{
    set_default_client_residency_requirement, set_default_originator,
};
use savfox_core::git_info::get_git_repo_root;
use savfox_core::models_manager::manager::RefreshStrategy;
use savfox_core::protocol::{
    AskForApproval, Event, EventMsg, Op, ReviewRequest, ReviewTarget, SessionSource,
    TurnAbortReason,
};
use savfox_core::{
    AuthManager, LMSTUDIO_OSS_PROVIDER_ID, NewSession, OLLAMA_CHAT_PROVIDER_ID,
    OLLAMA_OSS_PROVIDER_ID, SessionManager, find_session_path_by_id_str,
    find_session_path_by_name_str,
};
use savfox_protocol::approvals::ElicitationAction;
use savfox_protocol::config_types::SandboxMode;
use savfox_protocol::user_input::UserInput;
use serde_json::Value;
use supports_color::Stream;
use tokio::sync::{Mutex, oneshot};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use uuid::Uuid;

use crate::cli::Command as ExecCommand;
use crate::event_processor::{EventProcessor, SavfoxStatus};

enum InitialOperation {
    UserTurn {
        items: Vec<UserInput>,
        output_schema: Option<Value>,
    },
    Review {
        review_request: ReviewRequest,
    },
}

#[derive(Clone)]
struct SessionEventEnvelope {
    session_id: savfox_protocol::SessionId,
    session: Arc<savfox_core::SavfoxSession>,
    event: Event,
}

#[derive(Default)]
struct ExecLifecycle {
    initial_submitted: bool,
    interrupted: bool,
    terminal_emitted: bool,
}

impl ExecLifecycle {
    fn can_submit_initial(&self) -> bool {
        !self.initial_submitted && !self.interrupted
    }

    /// The signal is authoritative even if core has no active turn to abort.
    fn interrupt(&mut self) {
        self.interrupted = true;
    }

    /// Idle sessions do not produce TurnAborted. Finish cancellation output
    /// only after shutdown acknowledgements, without duplicating a core event.
    fn missing_cancellation_terminal(&mut self) -> Option<Event> {
        if !self.interrupted || self.terminal_emitted {
            return None;
        }
        self.terminal_emitted = true;
        Some(Event {
            id: String::new(),
            msg: EventMsg::TurnAborted(savfox_core::protocol::TurnAbortedEvent {
                reason: TurnAbortReason::Interrupted,
            }),
        })
    }

    fn accept_event(&mut self, primary: bool, event: &EventMsg) -> bool {
        if !primary && matches!(event, EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_)) {
            return false;
        }
        let terminal = matches!(event, EventMsg::TurnComplete(_))
            || matches!(event, EventMsg::TurnAborted(event) if event.reason == TurnAbortReason::Interrupted);
        if primary && terminal {
            if std::mem::replace(&mut self.terminal_emitted, true) {
                return false;
            }
            if matches!(event, EventMsg::TurnAborted(_)) {
                self.interrupted = true;
            }
        }
        true
    }
}

// Keep streamed events inline instead of allocating a box for every token.
#[allow(clippy::large_enum_variant)]
enum ExecLoopInput {
    Signal(bool),
    Submitted(savfox_core::error::Result<String>),
    Session(Option<SessionEventEnvelope>),
}

#[derive(Default)]
struct ExecShutdown {
    requested: HashSet<savfox_protocol::SessionId>,
    pending: HashSet<savfox_protocol::SessionId>,
    primary_finished: bool,
}

impl ExecShutdown {
    fn acknowledge(&mut self, id: savfox_protocol::SessionId, primary: bool) {
        // A session may close on its own, before exec requests its shutdown.
        // Remember that fact so cleanup does not enqueue another operation on
        // its closed coordinator.
        self.requested.insert(id);
        self.pending.remove(&id);
        self.primary_finished |= primary;
    }

    fn complete(&self) -> bool {
        self.primary_finished && self.pending.is_empty()
    }
}

async fn next_exec_input(
    interrupt: &mut oneshot::Receiver<()>,
    signal_finished: bool,
    submission: &mut (impl Future<Output = savfox_core::error::Result<String>> + Unpin),
    can_submit: bool,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<SessionEventEnvelope>,
) -> ExecLoopInput {
    tokio::select! {
        biased;
        result = interrupt, if !signal_finished => ExecLoopInput::Signal(result.is_ok()),
        result = submission, if can_submit => ExecLoopInput::Submitted(result),
        event = events.recv() => ExecLoopInput::Session(event),
    }
}

async fn request_exec_shutdown(manager: &SessionManager, shutdown: &mut ExecShutdown) {
    for id in manager.list_session_ids().await {
        if !shutdown.requested.insert(id) {
            continue;
        }
        let Ok(session) = manager.get_session(id).await else {
            continue;
        };
        if session.submit(Op::Shutdown).await.is_ok() {
            shutdown.pending.insert(id);
        }
    }
}

pub async fn run_main(cli: Cli, savfox_linux_sandbox_exe: Option<PathBuf>) -> anyhow::Result<()> {
    if let Err(err) = set_default_originator("savfox_exec".to_owned()) {
        tracing::warn!(
            ?err,
            "Failed to set savfox exec originator override {err:?}"
        );
    }

    let Cli {
        command,
        images,
        model: model_cli_arg,
        oss,
        oss_provider,
        full_auto,
        dangerously_bypass_approvals_and_sandbox,
        cwd,
        skip_git_repo_check,
        add_dir,
        color,
        last_message_file,
        json: json_mode,
        sandbox_mode: sandbox_mode_cli_arg,
        prompt,
        output_schema: output_schema_path,
    } = cli;

    let (stdout_with_ansi, stderr_with_ansi) = match color {
        cli::Color::Always => (true, true),
        cli::Color::Never => (false, false),
        cli::Color::Auto => (
            supports_color::on_cached(Stream::Stdout).is_some(),
            supports_color::on_cached(Stream::Stderr).is_some(),
        ),
    };

    // Build fmt layer (existing logging) to compose with OTEL layer.
    let default_level = "error";

    // Build env_filter separately and attach via with_filter.
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(stderr_with_ansi)
        .with_writer(std::io::stderr)
        .with_filter(env_filter);

    let sandbox_mode = if full_auto {
        Some(SandboxMode::WorkspaceWrite)
    } else if dangerously_bypass_approvals_and_sandbox {
        Some(SandboxMode::DangerFullAccess)
    } else {
        sandbox_mode_cli_arg.map(Into::<SandboxMode>::into)
    };

    let resolved_cwd = cwd.clone();

    #[allow(clippy::print_stderr)]
    let config_toml: ConfigToml = match Config::load_with_cli_overrides(Vec::new()).await {
        Ok(cfg) => cfg
            .config_layer_stack
            .effective_config()
            .try_into()
            .map_err(|e| anyhow::anyhow!("Failed to deserialize config: {e}"))?,
        Err(err) => {
            let config_error = err
                .get_ref()
                .and_then(|err| err.downcast_ref::<ConfigLoadError>())
                .map(ConfigLoadError::config_error);
            if let Some(config_error) = config_error {
                eprintln!(
                    "Error loading config.toml:\n{}",
                    format_config_error_with_source(config_error)
                );
            } else {
                eprintln!("Error loading config.toml: {err}");
            }
            std::process::exit(1);
        }
    };

    let cloud_requirements = CloudRequirementsLoader::default();

    let model_provider = if oss {
        let resolved = resolve_oss_provider(oss_provider.as_deref(), &config_toml);

        if let Some(provider) = resolved {
            Some(provider)
        } else {
            return Err(anyhow::anyhow!(
                "No default OSS provider configured. Use --local-provider=provider or set oss_provider to one of: {LMSTUDIO_OSS_PROVIDER_ID}, {OLLAMA_OSS_PROVIDER_ID}, {OLLAMA_CHAT_PROVIDER_ID} in config.toml"
            ));
        }
    } else {
        None // No OSS mode enabled
    };

    // When using `--oss`, let the bootstrapper pick the model based on selected provider
    let model = if let Some(model) = model_cli_arg {
        Some(model)
    } else if oss {
        model_provider
            .as_ref()
            .and_then(|provider_id| get_default_model_for_oss_provider(provider_id))
            .map(std::borrow::ToOwned::to_owned)
    } else {
        None // No model specified, will use the default.
    };

    // Load configuration and determine approval policy
    let overrides = ConfigOverrides {
        model,
        review_model: None,
        // Default to never ask for approvals in headless mode. Feature flags can override.
        approval_policy: Some(AskForApproval::Never),
        sandbox_mode,
        cwd: resolved_cwd,
        model_provider: model_provider.clone(),
        savfox_linux_sandbox_exe,
        base_instructions: None,
        developer_instructions: None,
        personality: None,
        compact_prompt: None,
        include_apply_patch_tool: None,
        show_raw_agent_reasoning: oss.then_some(true),
        tools_web_search_request: None,
        ephemeral: None,
        additional_writable_roots: add_dir,
    };

    let config = ConfigBuilder::default()
        .harness_overrides(overrides)
        .cloud_requirements(cloud_requirements)
        .build()
        .await?;
    set_default_client_residency_requirement(config.enforce_residency.value());

    if let Err(err) = enforce_login_restrictions(&config) {
        eprintln!("{err}");
        std::process::exit(1);
    }

    let ollama_chat_support_notice = match ollama_chat_deprecation_notice(&config).await {
        Ok(notice) => notice,
        Err(err) => {
            tracing::warn!(?err, "Failed to detect Ollama wire API");
            None
        }
    };

    let otel = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        savfox_core::otel_init::build_provider(&config, env!("CARGO_PKG_VERSION"), None, false)
    })) {
        Ok(Ok(otel)) => otel,
        Ok(Err(e)) => {
            eprintln!("Could not create otel exporter: {e}");
            None
        }
        Err(_) => {
            eprintln!("Could not create otel exporter: panicked during initialization");
            None
        }
    };

    let otel_logger_layer = otel.as_ref().and_then(|o| o.logger_layer());

    let otel_tracing_layer = otel.as_ref().and_then(|o| o.tracing_layer());

    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(otel_tracing_layer)
        .with(otel_logger_layer)
        .try_init();

    let mut event_processor: Box<dyn EventProcessor> = match json_mode {
        true => Box::new(EventProcessorWithJsonOutput::new(last_message_file.clone())),
        _ => Box::new(EventProcessorWithHumanOutput::create_with_ansi(
            stdout_with_ansi,
            &config,
            last_message_file.clone(),
        )),
    };
    if let Some(notice) = ollama_chat_support_notice {
        event_processor.process_event(Event {
            id: String::new(),
            msg: EventMsg::DeprecationNotice(notice),
        });
    }

    if oss {
        // We're in the oss section, so provider_id should be Some
        // Let's handle None case gracefully though just in case
        let provider_id = if let Some(id) = model_provider.as_ref() {
            id
        } else {
            error!("OSS provider unexpectedly not set when oss flag is used");
            return Err(anyhow::anyhow!(
                "OSS provider not set but oss flag was used"
            ));
        };
        ensure_oss_provider_ready(provider_id, &config)
            .await
            .map_err(|e| anyhow::anyhow!("OSS setup failed: {e}"))?;
    }

    let default_cwd = config.cwd.clone();
    let default_approval_policy = config.approval_policy.value();
    let default_sandbox_policy = config.sandbox_policy.get();
    let default_effort = config.model_reasoning_effort;
    let default_summary = config.model_reasoning_summary;

    // When --yolo (dangerously_bypass_approvals_and_sandbox) is set, also skip the git repo check
    // since the user is explicitly running in an externally sandboxed environment.
    if !skip_git_repo_check
        && !dangerously_bypass_approvals_and_sandbox
        && get_git_repo_root(&default_cwd).is_none()
    {
        eprintln!("Not inside a trusted directory and --skip-git-repo-check was not specified.");
        std::process::exit(1);
    }

    let auth_manager = AuthManager::shared(
        config.savfox_home.clone(),
        true,
        config.cli_auth_credentials_store_mode,
    );
    let session_manager = Arc::new(SessionManager::new(
        config.savfox_home.clone(),
        auth_manager.clone(),
        SessionSource::Exec,
    ));
    let default_model = session_manager
        .get_models_manager()
        .get_default_model(&config.model, &config, RefreshStrategy::OnlineIfUncached)
        .await?;

    // Handle resume subcommand by resolving a rollout path and using explicit resume API.
    let NewSession {
        session_id: primary_session_id,
        session,
        session_configured,
    } = if let Some(ExecCommand::Resume(args)) = command.as_ref() {
        let resume_path = resolve_resume_path(&config, args).await?;

        session_manager
            .resume_session_from_rollout(config.clone(), resume_path, auth_manager.clone())
            .await?
    } else {
        session_manager.start_session(config.clone()).await?
    };
    let (initial_operation, prompt_summary) = match (command, prompt, images) {
        (Some(ExecCommand::Review(review_cli)), ..) => {
            let review_request = build_review_request(review_cli)?;
            let summary = savfox_core::review_prompts::user_facing_hint(&review_request.target);
            (InitialOperation::Review { review_request }, summary)
        }
        (Some(ExecCommand::Resume(args)), root_prompt, imgs) => {
            let prompt_arg = args
                .prompt
                .clone()
                .or_else(|| {
                    if args.last {
                        args.session_id.clone()
                    } else {
                        None
                    }
                })
                .or(root_prompt);
            let prompt_text = resolve_prompt(prompt_arg);
            let mut items: Vec<UserInput> = imgs
                .into_iter()
                .chain(args.images)
                .map(|path| UserInput::LocalImage { path })
                .collect();
            items.push(UserInput::Text {
                text: prompt_text.clone(),
                // CLI input doesn't track UI element ranges, so none are available here.
                text_elements: Vec::new(),
            });
            let output_schema = load_output_schema(output_schema_path.clone());
            (
                InitialOperation::UserTurn {
                    items,
                    output_schema,
                },
                prompt_text,
            )
        }
        (None, root_prompt, imgs) => {
            let prompt_text = resolve_prompt(root_prompt);
            let mut items: Vec<UserInput> = imgs
                .into_iter()
                .map(|path| UserInput::LocalImage { path })
                .collect();
            items.push(UserInput::Text {
                text: prompt_text.clone(),
                // CLI input doesn't track UI element ranges, so none are available here.
                text_elements: Vec::new(),
            });
            let output_schema = load_output_schema(output_schema_path);
            (
                InitialOperation::UserTurn {
                    items,
                    output_schema,
                },
                prompt_text,
            )
        }
    };

    // Print the effective configuration and initial request so users can see what Savfox
    // is using.
    event_processor.print_config_summary(&config, &prompt_summary, &session_configured);

    info!("Savfox initialized with event: {session_configured:?}");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SessionEventEnvelope>();
    let attached_sessions = Arc::new(Mutex::new(HashSet::from([primary_session_id])));
    spawn_session_listener(primary_session_id, session.clone(), tx.clone());

    let (interrupt_tx, mut interrupt_rx) = oneshot::channel();
    let (signal_ready_tx, signal_ready_rx) = oneshot::channel();
    tokio::spawn(async move {
        let signal = tokio::signal::ctrl_c();
        tokio::pin!(signal);
        let mut signal_ready_tx = Some(signal_ready_tx);
        let result = std::future::poll_fn(|context| {
            // ctrl_c installs the OS listener on its first poll. Do that
            // before initial submission can start any model/tool work.
            let result = signal.as_mut().poll(context);
            if let Some(ready) = signal_ready_tx.take() {
                let _ = ready.send(());
            }
            result
        })
        .await;
        if result.is_ok() {
            let _ = interrupt_tx.send(());
        }
    });
    let _ = signal_ready_rx.await;

    {
        let session_manager = Arc::clone(&session_manager);
        let attached_sessions = Arc::clone(&attached_sessions);
        let tx = tx.clone();
        let mut session_created_rx = session_manager.subscribe_session_created();
        tokio::spawn(async move {
            loop {
                match session_created_rx.recv().await {
                    Ok(session_id) => {
                        if attached_sessions.lock().await.contains(&session_id) {
                            continue;
                        }
                        match session_manager.get_session(session_id).await {
                            Ok(session) => {
                                attached_sessions.lock().await.insert(session_id);
                                spawn_session_listener(session_id, session, tx.clone());
                            }
                            Err(err) => {
                                warn!("failed to attach listener for session {session_id}: {err}")
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        warn!("session_created receiver lagged; skipping resync");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    let mut initial_submission = Box::pin(async {
        match initial_operation {
            InitialOperation::UserTurn {
                items,
                output_schema,
            } => {
                session
                    .submit(Op::UserTurn {
                        items,
                        cwd: default_cwd,
                        approval_policy: default_approval_policy,
                        sandbox_policy: default_sandbox_policy.clone(),
                        model: default_model,
                        effort: default_effort,
                        summary: default_summary,
                        final_output_json_schema: output_schema,
                        collaboration_mode: None,
                        personality: None,
                    })
                    .await
            }
            InitialOperation::Review { review_request } => {
                session.submit(Op::Review { review_request }).await
            }
        }
    });

    // Run the loop until the task is complete.
    // Track whether a fatal error was reported by the server so we can
    // exit with a non-zero status for automation-friendly signaling.
    let mut error_seen = false;
    let mut lifecycle = ExecLifecycle::default();
    let mut signal_finished = false;
    let mut shutdown_state = ExecShutdown::default();
    loop {
        let envelope = match next_exec_input(
            &mut interrupt_rx,
            signal_finished,
            &mut initial_submission,
            lifecycle.can_submit_initial(),
            &mut rx,
        )
        .await
        {
            ExecLoopInput::Signal(received) => {
                signal_finished = true;
                if received {
                    tracing::debug!("Keyboard interrupt");
                    lifecycle.interrupt();
                    request_exec_shutdown(&session_manager, &mut shutdown_state).await;
                    if shutdown_state.pending.is_empty() {
                        break;
                    }
                }
                continue;
            }
            ExecLoopInput::Submitted(result) => {
                let task_id = result?;
                lifecycle.initial_submitted = true;
                info!("Sent initial request with event ID: {task_id}");
                continue;
            }
            ExecLoopInput::Session(Some(envelope)) => envelope,
            ExecLoopInput::Session(None) => break,
        };
        let SessionEventEnvelope {
            session_id,
            session,
            mut event,
        } = envelope;
        if !lifecycle.interrupted
            && let EventMsg::ElicitationRequest(ev) = &event.msg
        {
            // Automatically cancel elicitation requests in exec mode.
            session
                .submit(Op::ResolveElicitation {
                    server_name: ev.server_name.clone(),
                    request_id: ev.id.clone(),
                    decision: ElicitationAction::Cancel,
                })
                .await?;
        }
        if matches!(event.msg, EventMsg::Error(_)) {
            error_seen = true;
        }
        if lifecycle.interrupted
            && session_id == primary_session_id
            && matches!(event.msg, EventMsg::TurnComplete(_))
        {
            // A completed core turn may already be queued when the signal
            // arrives. The exec operation was cancelled before delivering it.
            event.msg = EventMsg::TurnAborted(savfox_core::protocol::TurnAbortedEvent {
                reason: TurnAbortReason::Interrupted,
            });
        }
        if !lifecycle.accept_event(session_id == primary_session_id, &event.msg) {
            continue;
        }
        let shutdown = event_processor.process_event(event);
        if session_id != primary_session_id && matches!(shutdown, SavfoxStatus::InitiateShutdown) {
            continue;
        }
        match shutdown {
            SavfoxStatus::Running => continue,
            SavfoxStatus::InitiateShutdown => {
                if lifecycle.interrupted {
                    // Wait for every session's cleanup before exiting, including
                    // shell processes owned by sub-agents.
                    request_exec_shutdown(&session_manager, &mut shutdown_state).await;
                    if shutdown_state.pending.is_empty() {
                        break;
                    }
                } else {
                    shutdown_state.requested.insert(session_id);
                    shutdown_state.pending.insert(session_id);
                    session.submit(Op::Shutdown).await?;
                }
            }
            SavfoxStatus::Shutdown => {
                let primary = session_id == primary_session_id;
                shutdown_state.acknowledge(session_id, primary);
                if primary {
                    // Preserve spontaneous primary shutdown and clean up any
                    // remaining sub-agent sessions before returning.
                    request_exec_shutdown(&session_manager, &mut shutdown_state).await;
                }
                if shutdown_state.complete() {
                    break;
                }
            }
        }
    }
    if let Some(event) = lifecycle.missing_cancellation_terminal() {
        event_processor.process_event(event);
    }
    event_processor.print_final_output();
    if lifecycle.interrupted {
        std::process::exit(130);
    }
    if error_seen {
        std::process::exit(1);
    }

    Ok(())
}

fn spawn_session_listener(
    session_id: savfox_protocol::SessionId,
    session: Arc<savfox_core::SavfoxSession>,
    tx: tokio::sync::mpsc::UnboundedSender<SessionEventEnvelope>,
) {
    tokio::spawn(async move {
        loop {
            match session.next_event().await {
                Ok(event) => {
                    debug!("Received event: {event:?}");

                    let is_shutdown_complete = matches!(event.msg, EventMsg::ShutdownComplete);
                    if let Err(err) = tx.send(SessionEventEnvelope {
                        session_id,
                        session: Arc::clone(&session),
                        event,
                    }) {
                        error!("Error sending event: {err:?}");
                        break;
                    }
                    if is_shutdown_complete {
                        info!(
                            "Received shutdown event for session {session_id}, exiting event loop."
                        );
                        break;
                    }
                }
                Err(err) => {
                    error!("Error receiving event: {err:?}");
                    break;
                }
            }
        }
    });
}

async fn resolve_resume_path(
    config: &Config,
    args: &crate::cli::ResumeArgs,
) -> anyhow::Result<PathBuf> {
    let path = if args.last {
        let default_provider_filter = vec![config.model_provider_id.clone()];
        let filter_cwd = if args.all {
            None
        } else {
            Some(config.cwd.as_path())
        };
        savfox_core::RolloutRecorder::find_latest_session_path(
            &config.savfox_home,
            1,
            None,
            savfox_core::SessionSortKey::UpdatedAt,
            &[],
            Some(default_provider_filter.as_slice()),
            &config.model_provider_id,
            filter_cwd,
        )
        .await
        .map_err(|err| anyhow::anyhow!("failed to list sessions for resume: {err}"))?
    } else if let Some(id_str) = args.session_id.as_deref() {
        if Uuid::parse_str(id_str).is_ok() {
            find_session_path_by_id_str(&config.savfox_home, id_str).await?
        } else {
            find_session_path_by_name_str(&config.savfox_home, id_str).await?
        }
    } else {
        anyhow::bail!("Specify a session id or name, or use --last, to resume a session");
    };
    path.ok_or_else(|| {
        if args.last {
            anyhow::anyhow!(
                "No recorded session found matching --last; start a new session with savfox exec"
            )
        } else {
            anyhow::anyhow!(
                "No recorded session found for {}; check the session id or name",
                args.session_id.as_deref().unwrap_or_default()
            )
        }
    })
}

fn load_output_schema(path: Option<PathBuf>) -> Option<Value> {
    let path = path?;

    let schema_str = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) => {
            eprintln!(
                "Failed to read output schema file {}: {err}",
                path.display()
            );
            std::process::exit(1);
        }
    };

    match serde_json::from_str::<Value>(&schema_str) {
        Ok(value) => Some(value),
        Err(err) => {
            eprintln!(
                "Output schema file {} is not valid JSON: {err}",
                path.display()
            );
            std::process::exit(1);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptDecodeError {
    InvalidUtf8 { valid_up_to: usize },
    InvalidUtf16 { encoding: &'static str },
    UnsupportedBom { encoding: &'static str },
}

impl std::fmt::Display for PromptDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUtf8 { valid_up_to } => write!(
                f,
                "input is not valid UTF-8 (invalid byte at offset {valid_up_to}). Convert it to UTF-8 and retry (e.g., `iconv -f <ENC> -t UTF-8 prompt.txt`)."
            ),
            Self::InvalidUtf16 { encoding } => write!(
                f,
                "input looked like {encoding} but could not be decoded. Convert it to UTF-8 and retry."
            ),
            Self::UnsupportedBom { encoding } => write!(
                f,
                "input appears to be {encoding}. Convert it to UTF-8 and retry."
            ),
        }
    }
}

fn decode_prompt_bytes(input: &[u8]) -> Result<String, PromptDecodeError> {
    let input = input.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(input);

    if input.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return Err(PromptDecodeError::UnsupportedBom {
            encoding: "UTF-32LE",
        });
    }

    if input.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return Err(PromptDecodeError::UnsupportedBom {
            encoding: "UTF-32BE",
        });
    }

    if let Some(rest) = input.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, "UTF-16LE", u16::from_le_bytes);
    }

    if let Some(rest) = input.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, "UTF-16BE", u16::from_be_bytes);
    }

    std::str::from_utf8(input)
        .map(str::to_owned)
        .map_err(|e| PromptDecodeError::InvalidUtf8 {
            valid_up_to: e.valid_up_to(),
        })
}

fn decode_utf16(
    input: &[u8],
    encoding: &'static str,
    decode_unit: fn([u8; 2]) -> u16,
) -> Result<String, PromptDecodeError> {
    if !input.len().is_multiple_of(2) {
        return Err(PromptDecodeError::InvalidUtf16 { encoding });
    }

    let units: Vec<u16> = input
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| decode_unit([chunk[0], chunk[1]]))
        .collect();

    String::from_utf16(&units).map_err(|_| PromptDecodeError::InvalidUtf16 { encoding })
}

fn resolve_prompt(prompt_arg: Option<String>) -> String {
    match prompt_arg {
        Some(p) if p != "-" => p,
        maybe_dash => {
            let force_stdin = matches!(maybe_dash.as_deref(), Some("-"));

            if std::io::stdin().is_terminal() && !force_stdin {
                eprintln!(
                    "No prompt provided. Either specify one as an argument or pipe the prompt into stdin."
                );
                std::process::exit(1);
            }

            if !force_stdin {
                eprintln!("Reading prompt from stdin...");
            }

            let mut bytes = Vec::new();
            if let Err(e) = std::io::stdin().read_to_end(&mut bytes) {
                eprintln!("Failed to read prompt from stdin: {e}");
                std::process::exit(1);
            }

            let buffer = match decode_prompt_bytes(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to read prompt from stdin: {e}");
                    std::process::exit(1);
                }
            };

            if buffer.trim().is_empty() {
                eprintln!("No prompt provided via stdin.");
                std::process::exit(1);
            }
            buffer
        }
    }
}

fn build_review_request(args: ReviewArgs) -> anyhow::Result<ReviewRequest> {
    let target = if args.uncommitted {
        ReviewTarget::UncommittedChanges
    } else if let Some(branch) = args.base {
        ReviewTarget::BaseBranch { branch }
    } else if let Some(sha) = args.commit {
        ReviewTarget::Commit {
            sha,
            title: args.commit_title,
        }
    } else if let Some(prompt_arg) = args.prompt {
        let prompt = resolve_prompt(Some(prompt_arg)).trim().to_owned();
        if prompt.is_empty() {
            anyhow::bail!("Review prompt cannot be empty");
        }
        ReviewTarget::Custom {
            instructions: prompt,
        }
    } else {
        anyhow::bail!(
            "Specify --uncommitted, --base, --commit, or provide custom review instructions"
        );
    };

    Ok(ReviewRequest {
        target,
        user_facing_hint: None,
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn spontaneous_primary_shutdown_finishes_without_a_prior_request() {
        let primary = savfox_protocol::SessionId::new();
        let mut shutdown = ExecShutdown::default();
        shutdown.acknowledge(primary, true);
        assert!(shutdown.complete());
        assert!(shutdown.requested.contains(&primary));
        assert!(
            !shutdown.requested.insert(primary),
            "the closed session must not be requested again"
        );
    }

    #[test]
    fn spontaneous_primary_shutdown_waits_for_remaining_child_cleanup() {
        let primary = savfox_protocol::SessionId::new();
        let child = savfox_protocol::SessionId::new();
        let mut shutdown = ExecShutdown::default();
        shutdown.acknowledge(primary, true);
        // The production loop discovers and requests child cleanup after the
        // spontaneous primary acknowledgement, before checking completion.
        shutdown.requested.insert(child);
        shutdown.pending.insert(child);
        assert!(!shutdown.complete());
        shutdown.acknowledge(child, false);
        assert!(shutdown.complete());
    }

    #[test]
    fn spontaneous_child_shutdown_does_not_finish_the_primary() {
        let child = savfox_protocol::SessionId::new();
        let mut shutdown = ExecShutdown::default();
        shutdown.acknowledge(child, false);
        assert!(!shutdown.complete());
    }

    #[tokio::test]
    async fn interrupt_before_submission_never_polls_the_initial_turn() {
        let (signal_tx, mut signal_rx) = oneshot::channel();
        signal_tx.send(()).unwrap();
        let polled = std::cell::Cell::new(false);
        let mut submission = std::future::poll_fn(|_| {
            polled.set(true);
            std::task::Poll::Ready(Ok("initial-turn".to_owned()))
        });
        let (_events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        let input =
            next_exec_input(&mut signal_rx, false, &mut submission, true, &mut events).await;
        assert!(matches!(input, ExecLoopInput::Signal(true)));
        assert!(
            !polled.get(),
            "a pending cancellation must win before submission"
        );
        let mut lifecycle = ExecLifecycle::default();
        lifecycle.interrupt();
        assert!(!lifecycle.can_submit_initial());
        assert!(lifecycle.missing_cancellation_terminal().is_some());
        assert!(lifecycle.missing_cancellation_terminal().is_none());
    }

    #[tokio::test]
    async fn interrupt_after_submission_is_observed_without_a_core_abort_event() {
        let (signal_tx, mut signal_rx) = oneshot::channel();
        let mut submission = std::future::ready(Ok("initial-turn".to_owned()));
        let (_events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut lifecycle = ExecLifecycle::default();
        let input =
            next_exec_input(&mut signal_rx, false, &mut submission, true, &mut events).await;
        assert!(matches!(input, ExecLoopInput::Submitted(Ok(_))));
        lifecycle.initial_submitted = true;
        signal_tx.send(()).unwrap();
        let input = next_exec_input(
            &mut signal_rx,
            false,
            &mut submission,
            lifecycle.can_submit_initial(),
            &mut events,
        )
        .await;
        assert!(matches!(input, ExecLoopInput::Signal(true)));
        lifecycle.interrupt();
        let terminal = lifecycle
            .missing_cancellation_terminal()
            .expect("idle cancellation terminal");
        let mut processor = EventProcessorWithJsonOutput::new(None);
        let output = processor.collect_session_events(&terminal);
        assert_eq!(output.len(), 1);
        assert!(
            matches!(&output[0], exec_events::SessionEvent::TurnFailed(event)
            if event.error.message == "Turn interrupted")
        );
    }

    #[test]
    fn interrupt_terminals_are_unique_across_signals_and_late_core_events() {
        let aborted = EventMsg::TurnAborted(savfox_core::protocol::TurnAbortedEvent {
            reason: TurnAbortReason::Interrupted,
        });
        let completed = EventMsg::TurnComplete(savfox_core::protocol::TurnCompleteEvent {
            last_agent_message: None,
        });
        let mut lifecycle = ExecLifecycle::default();
        assert!(!lifecycle.accept_event(false, &aborted));
        assert!(!lifecycle.interrupted);
        lifecycle.interrupt();
        assert!(lifecycle.accept_event(true, &aborted));
        assert!(!lifecycle.accept_event(true, &completed));
        assert!(lifecycle.missing_cancellation_terminal().is_none());
        lifecycle.interrupt();
        assert!(lifecycle.missing_cancellation_terminal().is_none());

        let mut completed_lifecycle = ExecLifecycle::default();
        assert!(completed_lifecycle.accept_event(true, &completed));
        completed_lifecycle.interrupt();
        assert!(completed_lifecycle.interrupted);
        assert!(
            completed_lifecycle
                .missing_cancellation_terminal()
                .is_none(),
            "a signal during shutdown cannot append a second terminal after turn.completed"
        );
    }

    #[tokio::test]
    async fn interrupted_turn_starts_shutdown_in_both_output_modes() {
        let home = tempfile::tempdir().unwrap();
        let config = core_test_support::load_default_config_for_test(&home).await;
        let mut processors: Vec<Box<dyn EventProcessor>> = vec![
            Box::new(EventProcessorWithHumanOutput::create_with_ansi(
                false, &config, None,
            )),
            Box::new(EventProcessorWithJsonOutput::new(None)),
        ];
        for processor in &mut processors {
            let status = processor.process_event(Event {
                id: "interrupted-turn".to_owned(),
                msg: EventMsg::TurnAborted(savfox_core::protocol::TurnAbortedEvent {
                    reason: TurnAbortReason::Interrupted,
                }),
            });
            assert!(matches!(status, SavfoxStatus::InitiateShutdown));
        }
    }

    #[test]
    fn builds_uncommitted_review_request() {
        let request = build_review_request(ReviewArgs {
            uncommitted: true,
            base: None,
            commit: None,
            commit_title: None,
            prompt: None,
        })
        .expect("builds uncommitted review request");

        let expected = ReviewRequest {
            target: ReviewTarget::UncommittedChanges,
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn builds_commit_review_request_with_title() {
        let request = build_review_request(ReviewArgs {
            uncommitted: false,
            base: None,
            commit: Some("123456789".to_owned()),
            commit_title: Some("Add review command".to_owned()),
            prompt: None,
        })
        .expect("builds commit review request");

        let expected = ReviewRequest {
            target: ReviewTarget::Commit {
                sha: "123456789".to_owned(),
                title: Some("Add review command".to_owned()),
            },
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn builds_custom_review_request_trims_prompt() {
        let request = build_review_request(ReviewArgs {
            uncommitted: false,
            base: None,
            commit: None,
            commit_title: None,
            prompt: Some("  custom review instructions  ".to_owned()),
        })
        .expect("builds custom review request");

        let expected = ReviewRequest {
            target: ReviewTarget::Custom {
                instructions: "custom review instructions".to_owned(),
            },
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn decode_prompt_bytes_strips_utf8_bom() {
        let input = [0xEF, 0xBB, 0xBF, b'h', b'i', b'\n'];

        let out = decode_prompt_bytes(&input).expect("decode utf-8 with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_decodes_utf16le_bom() {
        // UTF-16LE BOM + "hi\n"
        let input = [0xFF, 0xFE, b'h', 0x00, b'i', 0x00, b'\n', 0x00];

        let out = decode_prompt_bytes(&input).expect("decode utf-16le with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_decodes_utf16be_bom() {
        // UTF-16BE BOM + "hi\n"
        let input = [0xFE, 0xFF, 0x00, b'h', 0x00, b'i', 0x00, b'\n'];

        let out = decode_prompt_bytes(&input).expect("decode utf-16be with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_rejects_utf32le_bom() {
        // UTF-32LE BOM + "hi\n"
        let input = [
            0xFF, 0xFE, 0x00, 0x00, b'h', 0x00, 0x00, 0x00, b'i', 0x00, 0x00, 0x00, b'\n', 0x00,
            0x00, 0x00,
        ];

        let err = decode_prompt_bytes(&input).expect_err("utf-32le should be rejected");

        assert_eq!(
            err,
            PromptDecodeError::UnsupportedBom {
                encoding: "UTF-32LE"
            }
        );
    }

    #[test]
    fn decode_prompt_bytes_rejects_utf32be_bom() {
        // UTF-32BE BOM + "hi\n"
        let input = [
            0x00, 0x00, 0xFE, 0xFF, 0x00, 0x00, 0x00, b'h', 0x00, 0x00, 0x00, b'i', 0x00, 0x00,
            0x00, b'\n',
        ];

        let err = decode_prompt_bytes(&input).expect_err("utf-32be should be rejected");

        assert_eq!(
            err,
            PromptDecodeError::UnsupportedBom {
                encoding: "UTF-32BE"
            }
        );
    }

    #[test]
    fn decode_prompt_bytes_rejects_invalid_utf8() {
        // Invalid UTF-8 sequence: 0xC3 0x28
        let input = [0xC3, 0x28];

        let err = decode_prompt_bytes(&input).expect_err("invalid utf-8 should fail");

        assert_eq!(err, PromptDecodeError::InvalidUtf8 { valid_up_to: 0 });
    }
}
