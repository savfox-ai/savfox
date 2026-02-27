use std::io::{BufRead, BufReader, Write};
use std::process::ChildStdout;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;

use anyhow::Context;
use savfox_app_server_protocol::{
    CommandExecutionApprovalDecision, CommandExecutionRequestApprovalResponse,
    FileChangeApprovalDecision, FileChangeRequestApprovalResponse, JSONRPCNotification,
    JSONRPCRequest, JSONRPCResponse, JsonRpcMessage, ServerNotification, ServerRequest,
    SessionItem, SessionListResponse, SessionResumeResponse, SessionStartResponse,
};
use serde::Serialize;

use crate::output::{LabelColor, Output};
use crate::state::{PendingRequest, ReaderEvent, State};

pub fn start_reader(
    mut stdout: BufReader<ChildStdout>,
    stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
    state: Arc<Mutex<State>>,
    events: Sender<ReaderEvent>,
    output: Output,
    auto_approve: bool,
    filtered_output: bool,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let command_decision = if auto_approve {
            CommandExecutionApprovalDecision::Accept
        } else {
            CommandExecutionApprovalDecision::Decline
        };
        let file_decision = if auto_approve {
            FileChangeApprovalDecision::Accept
        } else {
            FileChangeApprovalDecision::Decline
        };

        let mut buffer = String::new();

        loop {
            buffer.clear();
            match stdout.read_line(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(err) => {
                    let _ = output.client_line(&format!("failed to read from server: {err}"));
                    break;
                }
            }

            let line = buffer.trim_end_matches(['\n', '\r']);
            if !line.is_empty() && !filtered_output {
                let _ = output.server_line(line);
            }

            let Ok(message) = serde_json::from_str::<JsonRpcMessage>(line) else {
                continue;
            };

            match message {
                JsonRpcMessage::Request(request) => {
                    if let Err(err) = handle_server_request(
                        request,
                        &command_decision,
                        &file_decision,
                        &stdin,
                        &output,
                    ) {
                        let _ =
                            output.client_line(&format!("failed to handle server request: {err}"));
                    }
                }
                JsonRpcMessage::Response(response) => {
                    if let Err(err) = handle_response(response, &state, &events) {
                        let _ = output.client_line(&format!("failed to handle response: {err}"));
                    }
                }
                JsonRpcMessage::Notification(notification) => {
                    if filtered_output
                        && let Err(err) = handle_filtered_notification(notification, &output)
                    {
                        let _ =
                            output.client_line(&format!("failed to filter notification: {err}"));
                    }
                }
                _ => {}
            }
        }
    })
}

fn handle_server_request(
    request: JSONRPCRequest,
    command_decision: &CommandExecutionApprovalDecision,
    file_decision: &FileChangeApprovalDecision,
    stdin: &Arc<Mutex<Option<std::process::ChildStdin>>>,
    output: &Output,
) -> anyhow::Result<()> {
    let server_request = match ServerRequest::try_from(request.clone()) {
        Ok(server_request) => server_request,
        Err(_) => return Ok(()),
    };

    match server_request {
        ServerRequest::CommandExecutionRequestApproval { request_id, params } => {
            let response = CommandExecutionRequestApprovalResponse {
                decision: command_decision.clone(),
            };
            output.client_line(&format!(
                "auto-response for command approval {request_id:?}: {command_decision:?} ({params:?})"
            ))?;
            send_response(stdin, request_id, response)
        }
        ServerRequest::FileChangeRequestApproval { request_id, params } => {
            let response = FileChangeRequestApprovalResponse {
                decision: file_decision.clone(),
            };
            output.client_line(&format!(
                "auto-response for file change approval {request_id:?}: {file_decision:?} ({params:?})"
            ))?;
            send_response(stdin, request_id, response)
        }
        _ => Ok(()),
    }
}

fn handle_response(
    response: JSONRPCResponse,
    state: &Arc<Mutex<State>>,
    events: &Sender<ReaderEvent>,
) -> anyhow::Result<()> {
    let pending = {
        let mut state = state.lock().expect("state lock poisoned");
        state.pending.remove(&response.id)
    };

    let Some(pending) = pending else {
        return Ok(());
    };

    match pending {
        PendingRequest::Start => {
            let parsed = serde_json::from_value::<SessionStartResponse>(response.result)
                .context("decode session/start response")?;
            let session_id = parsed.session.id;
            {
                let mut state = state.lock().expect("state lock poisoned");
                state.session_id = Some(session_id.clone());
                if !state.known_sessions.iter().any(|id| id == &session_id) {
                    state.known_sessions.push(session_id.clone());
                }
            }
            events.send(ReaderEvent::SessionReady { session_id }).ok();
        }
        PendingRequest::Resume => {
            let parsed = serde_json::from_value::<SessionResumeResponse>(response.result)
                .context("decode session/resume response")?;
            let session_id = parsed.session.id;
            {
                let mut state = state.lock().expect("state lock poisoned");
                state.session_id = Some(session_id.clone());
                if !state.known_sessions.iter().any(|id| id == &session_id) {
                    state.known_sessions.push(session_id.clone());
                }
            }
            events.send(ReaderEvent::SessionReady { session_id }).ok();
        }
        PendingRequest::List => {
            let parsed = serde_json::from_value::<SessionListResponse>(response.result)
                .context("decode session/list response")?;
            let session_ids: Vec<String> =
                parsed.data.into_iter().map(|session| session.id).collect();
            {
                let mut state = state.lock().expect("state lock poisoned");
                for session_id in &session_ids {
                    if !state.known_sessions.iter().any(|id| id == session_id) {
                        state.known_sessions.push(session_id.clone());
                    }
                }
            }
            events
                .send(ReaderEvent::SessionList {
                    session_ids,
                    next_cursor: parsed.next_cursor,
                })
                .ok();
        }
    }

    Ok(())
}

fn handle_filtered_notification(
    notification: JSONRPCNotification,
    output: &Output,
) -> anyhow::Result<()> {
    let Ok(server_notification) = ServerNotification::try_from(notification) else {
        return Ok(());
    };

    match server_notification {
        ServerNotification::ItemCompleted(payload) => {
            emit_filtered_item(payload.item, &payload.session_id, output)
        }
        _ => Ok(()),
    }
}

fn emit_filtered_item(item: SessionItem, session_id: &str, output: &Output) -> anyhow::Result<()> {
    let session_label = output.format_label(session_id, LabelColor::Session);
    match item {
        SessionItem::AgentMessage { text, .. } => {
            let label = output.format_label("assistant", LabelColor::Assistant);
            output.server_line(&format!("{session_label} {label}: {text}"))?;
        }
        SessionItem::Plan { text, .. } => {
            let label = output.format_label("assistant", LabelColor::Assistant);
            output.server_line(&format!("{session_label} {label}: plan"))?;
            write_multiline(output, &session_label, &format!("{label}:"), &text)?;
        }
        SessionItem::CommandExecution {
            command,
            status,
            exit_code,
            aggregated_output,
            ..
        } => {
            let label = output.format_label("tool", LabelColor::Tool);
            output.server_line(&format!(
                "{session_label} {label}: command {command} ({status:?})"
            ))?;
            if let Some(exit_code) = exit_code {
                let label = output.format_label("tool exit", LabelColor::ToolMeta);
                output.server_line(&format!("{session_label} {label}: {exit_code}"))?;
            }
            if let Some(aggregated_output) = aggregated_output {
                let label = output.format_label("tool output", LabelColor::ToolMeta);
                write_multiline(
                    output,
                    &session_label,
                    &format!("{label}:"),
                    &aggregated_output,
                )?;
            }
        }
        SessionItem::FileChange {
            changes, status, ..
        } => {
            let label = output.format_label("tool", LabelColor::Tool);
            output.server_line(&format!(
                "{session_label} {label}: file change ({status:?}, {} files)",
                changes.len()
            ))?;
        }
        SessionItem::McpToolCall {
            server,
            tool,
            status,
            arguments,
            result,
            error,
            ..
        } => {
            let label = output.format_label("tool", LabelColor::Tool);
            output.server_line(&format!(
                "{session_label} {label}: {server}.{tool} ({status:?})"
            ))?;
            if !arguments.is_null() {
                let label = output.format_label("tool args", LabelColor::ToolMeta);
                output.server_line(&format!("{session_label} {label}: {arguments}"))?;
            }
            if let Some(result) = result {
                let label = output.format_label("tool result", LabelColor::ToolMeta);
                output.server_line(&format!("{session_label} {label}: {result:?}"))?;
            }
            if let Some(error) = error {
                let label = output.format_label("tool error", LabelColor::ToolMeta);
                output.server_line(&format!("{session_label} {label}: {error:?}"))?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn write_multiline(
    output: &Output,
    session_label: &str,
    header: &str,
    text: &str,
) -> anyhow::Result<()> {
    output.server_line(&format!("{session_label} {header}"))?;
    for line in text.lines() {
        output.server_line(&format!("{session_label}   {line}"))?;
    }
    Ok(())
}

fn send_response<T: Serialize>(
    stdin: &Arc<Mutex<Option<std::process::ChildStdin>>>,
    request_id: savfox_app_server_protocol::RequestId,
    response: T,
) -> anyhow::Result<()> {
    let result = serde_json::to_value(response).context("serialize response")?;
    let message = JSONRPCResponse {
        id: request_id,
        result,
    };
    let json = serde_json::to_string(&message).context("serialize response message")?;
    let mut line = json;
    line.push('\n');

    let mut stdin = stdin.lock().expect("stdin lock poisoned");
    let Some(stdin) = stdin.as_mut() else {
        anyhow::bail!("stdin already closed");
    };
    stdin.write_all(line.as_bytes()).context("write response")?;
    stdin.flush().context("flush response")?;
    Ok(())
}
