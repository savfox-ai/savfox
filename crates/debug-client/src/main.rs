mod client;
mod commands;
mod output;
mod reader;
mod state;

use std::io;
use std::io::BufRead;
use std::sync::mpsc;

use anyhow::{Context, Result};
use clap::{ArgAction, Parser};
use savfox_app_server_protocol::AskForApproval;

use crate::client::{AppServerClient, build_session_resume_params, build_session_start_params};
use crate::commands::{InputAction, UserCommand, parse_input};
use crate::output::Output;
use crate::state::ReaderEvent;

#[derive(Parser)]
#[command(author = "Savfox", version, about = "Minimal app-server client")]
struct Cli {
    /// Path to the `savfox` CLI binary.
    #[arg(long, default_value = "savfox")]
    savfox_bin: String,

    /// Forwarded to the `savfox` CLI as `--config key=value`. Repeatable.
    #[arg(short = 'c', long = "config", value_name = "key=value", action = ArgAction::Append)]
    config_overrides: Vec<String>,

    /// Resume an existing session instead of starting a new one.
    #[arg(long)]
    session_id: Option<String>,

    /// Set the approval policy for the session.
    #[arg(long, default_value = "on-request")]
    approval_policy: String,

    /// Auto-approve command/file-change approvals.
    #[arg(long, default_value_t = false)]
    auto_approve: bool,

    /// Only show final assistant messages and tool calls.
    #[arg(long, default_value_t = false)]
    final_only: bool,

    /// Optional model override when starting/resuming a session.
    #[arg(long)]
    model: Option<String>,

    /// Optional model provider override when starting/resuming a session.
    #[arg(long)]
    model_provider: Option<String>,

    /// Optional working directory override when starting/resuming a session.
    #[arg(long)]
    cwd: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let output = Output::new();
    let approval_policy = parse_approval_policy(&cli.approval_policy)?;

    let mut client = AppServerClient::spawn(
        &cli.savfox_bin,
        &cli.config_overrides,
        output.clone(),
        cli.final_only,
    )?;
    client.initialize()?;

    let session_id = if let Some(session_id) = cli.session_id.as_ref() {
        client.resume_session(build_session_resume_params(
            session_id.clone(),
            approval_policy,
            cli.model.clone(),
            cli.model_provider.clone(),
            cli.cwd.clone(),
        ))?
    } else {
        client.start_session(build_session_start_params(
            approval_policy,
            cli.model.clone(),
            cli.model_provider.clone(),
            cli.cwd.clone(),
        ))?
    };

    output
        .client_line(&format!("connected to session {session_id}"))
        .ok();
    output.set_prompt(&session_id);

    let (event_tx, event_rx) = mpsc::channel();
    client.start_reader(event_tx, cli.auto_approve, cli.final_only)?;

    print_help(&output);

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
        drain_events(&event_rx, &output);
        let prompt_session = client
            .session_id()
            .unwrap_or_else(|| "no-session".to_string());
        output.prompt(&prompt_session).ok();

        let Some(line) = lines.next() else {
            break;
        };
        let line = line.context("read stdin")?;

        match parse_input(&line) {
            Ok(None) => continue,
            Ok(Some(InputAction::Message(message))) => {
                let Some(active_session) = client.session_id() else {
                    output
                        .client_line("no active session; use :new or :resume <id>")
                        .ok();
                    continue;
                };
                if let Err(err) = client.send_turn(&active_session, message) {
                    output
                        .client_line(&format!("failed to send turn: {err}"))
                        .ok();
                }
            }
            Ok(Some(InputAction::Command(command))) => {
                if !handle_command(command, &client, &output, approval_policy, &cli) {
                    break;
                }
            }
            Err(err) => {
                output.client_line(&err.message()).ok();
            }
        }
    }

    client.shutdown();
    Ok(())
}

fn handle_command(
    command: UserCommand,
    client: &AppServerClient,
    output: &Output,
    approval_policy: AskForApproval,
    cli: &Cli,
) -> bool {
    match command {
        UserCommand::Help => {
            print_help(output);
            true
        }
        UserCommand::Quit => false,
        UserCommand::NewSession => {
            match client.request_session_start(build_session_start_params(
                approval_policy,
                cli.model.clone(),
                cli.model_provider.clone(),
                cli.cwd.clone(),
            )) {
                Ok(request_id) => {
                    output
                        .client_line(&format!("requested new session ({request_id:?})"))
                        .ok();
                }
                Err(err) => {
                    output
                        .client_line(&format!("failed to start session: {err}"))
                        .ok();
                }
            }
            true
        }
        UserCommand::Resume(session_id) => {
            match client.request_session_resume(build_session_resume_params(
                session_id,
                approval_policy,
                cli.model.clone(),
                cli.model_provider.clone(),
                cli.cwd.clone(),
            )) {
                Ok(request_id) => {
                    output
                        .client_line(&format!("requested session resume ({request_id:?})"))
                        .ok();
                }
                Err(err) => {
                    output
                        .client_line(&format!("failed to resume session: {err}"))
                        .ok();
                }
            }
            true
        }
        UserCommand::Use(session_id) => {
            let known = client.use_session(session_id.clone());
            output.set_prompt(&session_id);
            if known {
                output
                    .client_line(&format!("switched active session to {session_id}"))
                    .ok();
            } else {
                output
                    .client_line(&format!(
                        "switched active session to {session_id} (unknown; use :resume to load)"
                    ))
                    .ok();
            }
            true
        }
        UserCommand::RefreshSession => {
            match client.request_session_list(None) {
                Ok(request_id) => {
                    output
                        .client_line(&format!("requested session list ({request_id:?})"))
                        .ok();
                }
                Err(err) => {
                    output
                        .client_line(&format!("failed to list sessions: {err}"))
                        .ok();
                }
            }
            true
        }
    }
}

fn parse_approval_policy(value: &str) -> Result<AskForApproval> {
    match value {
        "untrusted" | "unless-trusted" | "unlessTrusted" => Ok(AskForApproval::UnlessTrusted),
        "on-failure" | "onFailure" => Ok(AskForApproval::OnFailure),
        "on-request" | "onRequest" => Ok(AskForApproval::OnRequest),
        "never" => Ok(AskForApproval::Never),
        _ => anyhow::bail!(
            "unknown approval policy: {value}. Expected one of: untrusted, on-failure, on-request, never"
        ),
    }
}

fn drain_events(event_rx: &mpsc::Receiver<ReaderEvent>, output: &Output) {
    while let Ok(event) = event_rx.try_recv() {
        match event {
            ReaderEvent::SessionReady { session_id } => {
                output
                    .client_line(&format!("active session is now {session_id}"))
                    .ok();
                output.set_prompt(&session_id);
            }
            ReaderEvent::SessionList {
                session_ids,
                next_cursor,
            } => {
                if session_ids.is_empty() {
                    output.client_line("sessions: (none)").ok();
                } else {
                    output.client_line("sessions:").ok();
                    for session_id in session_ids {
                        output.client_line(&format!("  {session_id}")).ok();
                    }
                }
                if let Some(next_cursor) = next_cursor {
                    output
                        .client_line(&format!(
                            "more sessions available, next cursor: {next_cursor}"
                        ))
                        .ok();
                }
            }
        }
    }
}

fn print_help(output: &Output) {
    let _ = output.client_line("commands:");
    let _ = output.client_line("  :help                 show this help");
    let _ = output.client_line("  :new                  start a new session");
    let _ = output.client_line("  :resume <session-id>   resume an existing session");
    let _ = output.client_line("  :use <session-id>      switch the active session");
    let _ = output.client_line("  :refresh-session       list available sessions");
    let _ = output.client_line("  :quit                 exit");
    let _ = output.client_line("type a message to send it as a new turn");
}
