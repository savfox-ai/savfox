#![allow(unused_imports)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use savfox_protocol::SessionId;
use serde_json::{Value, json};
use tracing::debug;

use super::super::types::{INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, RpcResult};
use super::agent::apply_agent_permission_policy_to_config;
use crate::channel::GatewayChannel;
use crate::channels::policy::{
    DmScopePolicyConfig, load_dm_scope_policy, parse_dm_scope, write_dm_scope_policy,
};
use crate::channels::runtime::{
    cleanup_session_runtime_state, get_idle_reply_status, remove_idle_reply_session,
};
use crate::chat_session::{
    abort_all_active_threads, abort_first_active_candidate, persist_chat_session_metadata,
    provider_from_model, resolve_abort_candidate_ids, validate_uuid_v7_session_id,
};
use crate::identity_links::{
    canonical_for_peer, load_identity_links, save_identity_links, upsert_link,
};
use crate::media_store::MediaStore;
use crate::session::{
    GatewaySessionManager, SessionEntry, SessionOverrides, SessionStore, build_history_payload,
    build_routing_id, derive_session_label_from_history, peek_ambient_messages,
    remove_ambient_session,
};
use crate::terminal_agent::TerminalAgentEvent;

// ── Chat ────────────────────────────────────────────────────────────────────

fn format_model_footer(model: &str, provider: &str, profile: Option<&str>) -> String {
    match profile {
        Some(p) if !p.trim().is_empty() => {
            format!("model: {model} | provider: {provider} | profile: {p}")
        }
        _ => format!("model: {model} | provider: {provider}"),
    }
}

fn append_footer(reply: &str, footer: &str) -> String {
    let reply = reply.trim_end();
    if reply.is_empty() {
        footer.to_owned()
    } else {
        format!("{reply}\n\n---\n{footer}")
    }
}

fn next_stream_block_id(request_id: &str, block_kind: &str, counter: &mut u64) -> String {
    *counter += 1;
    format!("{request_id}:{block_kind}:{counter}")
}

async fn emit_stream_block_start(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    block_id: &str,
    block_kind: &str,
) {
    session_mgr
        .broadcast_to_all(
            "agent.stream.block",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "phase": "start",
                "block_id": block_id,
                "block_kind": block_kind,
            }),
        )
        .await;
}

async fn emit_stream_block_delta(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    block_id: &str,
    block_kind: &str,
    delta: &str,
) {
    session_mgr
        .broadcast_to_all(
            "agent.stream.block",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "phase": "delta",
                "block_id": block_id,
                "block_kind": block_kind,
                "delta": delta,
            }),
        )
        .await;
}

async fn emit_stream_block_progress(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    block_id: &str,
    block_kind: &str,
    completed: usize,
    status: &str,
) {
    session_mgr
        .broadcast_to_all(
            "agent.stream.progress",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "block_id": block_id,
                "block_kind": block_kind,
                "completed": completed,
                "status": status,
            }),
        )
        .await;
}

async fn emit_stream_block_end(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    block_id: &str,
    block_kind: &str,
    status: &str,
) {
    session_mgr
        .broadcast_to_all(
            "agent.stream.block",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "phase": "end",
                "block_id": block_id,
                "block_kind": block_kind,
                "status": status,
            }),
        )
        .await;
}

async fn emit_typing_heartbeat(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    agent_id: &str,
) {
    session_mgr
        .broadcast_to_all(
            "typing.start",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "agent_id": agent_id,
                "heartbeat": true,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;
}

async fn emit_typing_stop(
    session_mgr: &Arc<GatewaySessionManager>,
    request_id: &str,
    session_id: &str,
    agent_id: &str,
) {
    session_mgr
        .broadcast_to_all(
            "typing.stop",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "agent_id": agent_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;
}

fn terminal_event_stream_payload(
    request_id: &str,
    session_id: &str,
    thread_id: &str,
    event: &TerminalAgentEvent,
    stdout_truncated: bool,
    stderr_truncated: bool,
    log_dir: &str,
) -> Option<(&'static str, Value)> {
    match event {
        TerminalAgentEvent::Log { stream, text } => Some((
            "agent.stream",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "thread_id": thread_id,
                "kind": "log",
                "stream": stream,
                "terminal_event": "log",
                "delta": text,
                "truncated": match stream.as_str() {
                    "stdout" => stdout_truncated,
                    "stderr" => stderr_truncated,
                    _ => false,
                },
                "log_dir": log_dir,
            }),
        )),
        TerminalAgentEvent::Status { status } => Some((
            "agent.stream",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "thread_id": thread_id,
                "kind": "status",
                "terminal_event": "status",
                "status": status,
            }),
        )),
        TerminalAgentEvent::Error { message } => Some((
            "agent.error",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "thread_id": thread_id,
                "terminal_event": "error",
                "error": message,
            }),
        )),
        TerminalAgentEvent::Message { .. } | TerminalAgentEvent::Completed { .. } => None,
    }
}

pub(crate) async fn handle_chat_send(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_mgr: &Arc<GatewaySessionManager>,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    use savfox_core::models_manager::manager::RefreshStrategy;
    use savfox_protocol::protocol::{EventMsg, Op};
    use savfox_protocol::user_input::UserInput;

    use crate::auto_reply::directives::{
        DirectiveKind, fuzzy_match_model_name, parse_directives, parse_model_target,
    };

    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let agent = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let request_id = params
        .get("request_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let requested_session_id =
        validate_uuid_v7_session_id(params.get("session_id").and_then(|v| v.as_str()))
            .map_err(|message| (INVALID_PARAMS, message))?;

    if message.is_empty() {
        return Err((INVALID_REQUEST, "missing 'message' parameter".to_owned()));
    }

    let parsed = parse_directives(message);
    let prompt = if parsed.directives.is_empty() {
        message.trim().to_owned()
    } else {
        parsed.cleaned_text.trim().to_owned()
    };

    // Strip "[user]:" prefix if present (some clients add this prefix)
    let prompt = prompt
        .strip_prefix("[user]:")
        .map(|s| s.trim())
        .unwrap_or(&prompt)
        .to_owned();
    if prompt.is_empty() {
        return Err((
            INVALID_REQUEST,
            "message is empty after parsing directives".to_owned(),
        ));
    }

    // Deterministic mock path for integration/e2e testing without external model dependencies.
    // Enabled only when the caller explicitly passes `mock_response`.
    if let Some(mock_response) = params.get("mock_response") {
        let raw_response = if let Some(text) = mock_response.as_str() {
            text.to_owned()
        } else if mock_response.as_bool().unwrap_or(false) {
            format!("echo: {prompt}")
        } else {
            "(mock response)".to_owned()
        };
        let model = "mock/echo";
        let provider = "mock";
        let footer = format_model_footer(model, provider, None);
        let response = append_footer(&raw_response, &footer);
        let session_id = requested_session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());

        session_mgr
            .broadcast_to_all(
                "agent.stream",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "phase": "start",
                    "model": model,
                    "provider": provider,
                }),
            )
            .await;
        session_mgr
            .broadcast_to_all(
                "agent.complete",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "thread_id": session_id,
                    "response": response,
                    "raw_response": raw_response,
                    "model": model,
                    "provider": provider,
                    "profile": Value::Null,
                    "aborted": false,
                }),
            )
            .await;

        persist_chat_session_metadata(
            session_store.as_ref(),
            &channel.config().savfox_home,
            &session_id,
            &session_id,
            model,
            provider,
            None,
            None,
        )
        .await;

        return Ok(json!({
            "response": response,
            "raw_response": raw_response,
            "footer": footer,
            "model": model,
            "provider": provider,
            "profile": Value::Null,
            "session_id": session_id,
            "thread_id": session_id,
            "aborted": false,
            "mock": true,
        }));
    }

    let model_directive = parsed
        .directives
        .iter()
        .rev()
        .find(|d| d.kind == DirectiveKind::Model)
        .map(|d| d.value.clone());

    let (effective_model, model_profile) = if let Some(raw_model) = model_directive {
        let parsed_target = parse_model_target(&raw_model);
        let candidates: Vec<String> = channel
            .session_manager()
            .list_models(channel.config(), RefreshStrategy::Offline)
            .await
            .into_iter()
            .map(|m| m.id)
            .collect();
        let resolved = fuzzy_match_model_name(&parsed_target.model, &candidates)
            .unwrap_or(parsed_target.model);
        (resolved, parsed_target.profile)
    } else {
        (agent.to_owned(), None)
    };

    let provider = provider_from_model(&effective_model);

    if channel.resolve_terminal_delegate(agent).await.is_some() {
        let logical_session_id = requested_session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let thread_id = logical_session_id.clone();
        let terminal_model_for_start = format!("terminal/{agent}");

        session_mgr
            .broadcast_to_all(
                "agent.stream",
                json!({
                    "request_id": request_id,
                    "session_id": logical_session_id,
                    "thread_id": thread_id,
                    "phase": "start",
                    "terminal_event": "started",
                    "model": terminal_model_for_start,
                    "provider": "terminal",
                    "profile": model_profile,
                }),
            )
            .await;

        let invocation = match channel
            .invoke_terminal_delegate_agent(
                &prompt,
                agent,
                &effective_model,
                Some(logical_session_id.as_str()),
            )
            .await
        {
            Ok(Some(invocation)) => invocation,
            Ok(None) => {
                let message = "terminal delegate disappeared during invocation".to_owned();
                session_mgr
                    .broadcast_to_all(
                        "agent.error",
                        json!({
                            "request_id": request_id,
                            "session_id": logical_session_id,
                            "terminal_event": "error",
                            "error": message,
                        }),
                    )
                    .await;
                return Err((INTERNAL_ERROR, message));
            }
            Err(err) => {
                let message = format!("terminal delegate error: {err}");
                session_mgr
                    .broadcast_to_all(
                        "agent.error",
                        json!({
                            "request_id": request_id,
                            "session_id": logical_session_id,
                            "terminal_event": "error",
                            "error": message,
                        }),
                    )
                    .await;
                return Err((INTERNAL_ERROR, message));
            }
        };
        let reply = invocation.result.reply;
        let rollout_path = invocation.result.rollout_path;
        let model = invocation.model;
        let provider = invocation.provider;
        let terminal = invocation.terminal;

        let terminal_log_dir = terminal.log_dir.to_string_lossy().to_string();
        for event in &terminal.events {
            if let Some((topic, payload)) = terminal_event_stream_payload(
                &request_id,
                &logical_session_id,
                &thread_id,
                event,
                terminal.stdout_truncated,
                terminal.stderr_truncated,
                &terminal_log_dir,
            ) {
                session_mgr.broadcast_to_all(topic, payload).await;
            }
        }

        session_mgr
            .broadcast_to_all(
                "agent.stream",
                json!({
                    "request_id": request_id,
                    "session_id": logical_session_id,
                    "thread_id": thread_id,
                    "kind": "text",
                    "terminal_event": "message",
                    "delta": reply,
                }),
            )
            .await;

        let mut block_counter = 0_u64;
        let block_id = next_stream_block_id(&request_id, "text", &mut block_counter);
        emit_stream_block_start(
            session_mgr,
            &request_id,
            &logical_session_id,
            &block_id,
            "text",
        )
        .await;
        emit_stream_block_delta(
            session_mgr,
            &request_id,
            &logical_session_id,
            &block_id,
            "text",
            &reply,
        )
        .await;
        emit_stream_block_progress(
            session_mgr,
            &request_id,
            &logical_session_id,
            &block_id,
            "text",
            reply.chars().count(),
            "complete",
        )
        .await;
        emit_stream_block_end(
            session_mgr,
            &request_id,
            &logical_session_id,
            &block_id,
            "text",
            "complete",
        )
        .await;

        let footer = format_model_footer(&model, &provider, model_profile.as_deref());
        let response = append_footer(&reply, &footer);

        persist_chat_session_metadata(
            session_store.as_ref(),
            &channel.config().savfox_home,
            &logical_session_id,
            &thread_id,
            &model,
            &provider,
            rollout_path.as_deref(),
            None,
        )
        .await;

        session_mgr
            .broadcast_to_all(
                "agent.complete",
                json!({
                    "request_id": request_id,
                    "session_id": logical_session_id,
                    "thread_id": thread_id,
                    "response": response,
                    "raw_response": reply,
                    "model": model,
                    "provider": provider,
                    "profile": model_profile,
                    "terminal_event": "completed",
                    "terminal": {
                        "agent_id": terminal.agent_id,
                        "agent_name": terminal.agent_name,
                        "profile": terminal.profile,
                        "mode": terminal.mode,
                        "session_scope": terminal.session_scope,
                        "io_protocol": terminal.io_protocol,
                        "workspace_dir": terminal.workspace_dir.to_string_lossy().to_string(),
                        "log_dir": terminal.log_dir.to_string_lossy().to_string(),
                        "stdout_truncated": terminal.stdout_truncated,
                        "stderr_truncated": terminal.stderr_truncated,
                        "pid": terminal.pid,
                        "exit_code": terminal.exit_code,
                        "events": terminal.events,
                    },
                    "aborted": false,
                }),
            )
            .await;

        return Ok(json!({
            "response": response,
            "raw_response": reply,
            "footer": footer,
            "model": model,
            "provider": provider,
            "profile": model_profile,
            "terminal": {
                "agent_id": terminal.agent_id,
                "agent_name": terminal.agent_name,
                "profile": terminal.profile,
                "mode": terminal.mode,
                "session_scope": terminal.session_scope,
                "io_protocol": terminal.io_protocol,
                "workspace_dir": terminal.workspace_dir.to_string_lossy().to_string(),
                "log_dir": terminal.log_dir.to_string_lossy().to_string(),
                "stdout_truncated": terminal.stdout_truncated,
                "stderr_truncated": terminal.stderr_truncated,
                "pid": terminal.pid,
                "exit_code": terminal.exit_code,
                "events": terminal.events,
            },
            "session_id": logical_session_id,
            "thread_id": thread_id,
            "aborted": false,
        }));
    }

    let mut config = (**channel.config()).clone();
    if !effective_model.trim().is_empty() {
        config.model = Some(effective_model.clone());
    }

    // Apply the agent's permission policy to the session config.
    apply_agent_permission_policy_to_config(&mut config, channel, agent).await;

    let resolved_session = channel
        .resolve_agent_session(config, requested_session_id.as_deref())
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("failed to resolve thread: {err}")))?;
    let thread_session_id = resolved_session.session_id;
    let logical_session_id = requested_session_id
        .clone()
        .unwrap_or_else(|| thread_session_id.to_string());

    if session_store.get(&logical_session_id).await.is_none()
        && let Some(mut entry) = session_store.get_or_create(&logical_session_id).await
    {
        entry.model = Some(effective_model.clone());
        entry.provider = Some(provider.clone());
        entry.touch();
        session_store.upsert(entry).await;
    }

    let session_id_obj = thread_session_id;
    let thread_id = session_id_obj.to_string();
    let session_id = logical_session_id.clone();

    session_mgr
        .broadcast_to_all(
            "agent.stream",
            json!({
                "request_id": request_id,
                "session_id": logical_session_id,
                "thread_id": thread_id,
                "phase": "start",
                "model": effective_model,
                "provider": provider,
                "profile": model_profile,
            }),
        )
        .await;

    let thread = channel
        .session_manager()
        .get_session(session_id_obj)
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("failed to load thread: {err}")))?;
    let rollout_path = thread.rollout_path();

    thread
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: prompt.clone(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .map_err(|err| (INTERNAL_ERROR, format!("failed to submit prompt: {err}")))?;

    let mut reply = String::new();
    let mut aborted = false;
    let timeout = tokio::time::Duration::from_secs(120);
    let deadline = tokio::time::Instant::now() + timeout;
    let mut fatal_error: Option<(i64, String)> = None;
    let mut last_token_usage: Option<savfox_protocol::protocol::TokenUsage> = None;

    let mut block_counter = 0_u64;
    let mut text_block_id: Option<String> = None;
    let mut thinking_block_id: Option<String> = None;
    let mut tool_blocks: HashMap<String, String> = HashMap::new();
    let mut block_progress: HashMap<String, usize> = HashMap::new();

    let typing_interval = Duration::from_secs(3);
    let mut next_typing_heartbeat = tokio::time::Instant::now();
    emit_typing_heartbeat(session_mgr, &request_id, &session_id, agent).await;
    next_typing_heartbeat += typing_interval;

    loop {
        let now = tokio::time::Instant::now();
        if now >= next_typing_heartbeat {
            emit_typing_heartbeat(session_mgr, &request_id, &session_id, agent).await;
            next_typing_heartbeat = now + typing_interval;
        }

        let wait_until = if next_typing_heartbeat < deadline {
            next_typing_heartbeat
        } else {
            deadline
        };

        match tokio::time::timeout_at(wait_until, thread.next_event()).await {
            Ok(Ok(event)) => {
                debug!(event = ?event.msg, "Gateway event received");
                match &event.msg {
                    EventMsg::TokenCount(token_count) => {
                        if let Some(info) = &token_count.info {
                            last_token_usage = Some(info.last_token_usage.clone());
                        }
                    }
                    EventMsg::AgentMessage(msg) if !msg.message.is_empty() => {
                        reply.push_str(&msg.message);

                        session_mgr
                            .broadcast_to_all(
                                "agent.stream",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "kind": "text",
                                    "delta": msg.message,
                                }),
                            )
                            .await;

                        if text_block_id.is_none() {
                            let id = next_stream_block_id(&request_id, "text", &mut block_counter);
                            emit_stream_block_start(
                                session_mgr,
                                &request_id,
                                &session_id,
                                &id,
                                "text",
                            )
                            .await;
                            text_block_id = Some(id);
                        }

                        if let Some(block_id) = text_block_id.as_ref() {
                            emit_stream_block_delta(
                                session_mgr,
                                &request_id,
                                &session_id,
                                block_id,
                                "text",
                                &msg.message,
                            )
                            .await;
                            let completed = {
                                let entry = block_progress.entry(block_id.clone()).or_insert(0);
                                *entry += msg.message.chars().count();
                                *entry
                            };
                            emit_stream_block_progress(
                                session_mgr,
                                &request_id,
                                &session_id,
                                block_id,
                                "text",
                                completed,
                                "streaming",
                            )
                            .await;
                        }
                    }
                    EventMsg::AgentMessageDelta(delta) if !delta.delta.is_empty() => {
                        reply.push_str(&delta.delta);
                        session_mgr
                            .broadcast_to_all(
                                "agent.stream",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "kind": "text",
                                    "delta": delta.delta,
                                }),
                            )
                            .await;

                        if text_block_id.is_none() {
                            let id = next_stream_block_id(&request_id, "text", &mut block_counter);
                            emit_stream_block_start(
                                session_mgr,
                                &request_id,
                                &session_id,
                                &id,
                                "text",
                            )
                            .await;
                            text_block_id = Some(id);
                        }

                        if let Some(block_id) = text_block_id.as_ref() {
                            emit_stream_block_delta(
                                session_mgr,
                                &request_id,
                                &session_id,
                                block_id,
                                "text",
                                &delta.delta,
                            )
                            .await;
                            let completed = {
                                let entry = block_progress.entry(block_id.clone()).or_insert(0);
                                *entry += delta.delta.chars().count();
                                *entry
                            };
                            emit_stream_block_progress(
                                session_mgr,
                                &request_id,
                                &session_id,
                                block_id,
                                "text",
                                completed,
                                "streaming",
                            )
                            .await;
                        }
                    }
                    EventMsg::AgentReasoningDelta(delta) if !delta.delta.is_empty() => {
                        if thinking_block_id.is_none() {
                            let id =
                                next_stream_block_id(&request_id, "thinking", &mut block_counter);
                            emit_stream_block_start(
                                session_mgr,
                                &request_id,
                                &session_id,
                                &id,
                                "thinking",
                            )
                            .await;
                            thinking_block_id = Some(id);
                        }

                        session_mgr
                            .broadcast_to_all(
                                "agent.stream",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "kind": "reasoning",
                                    "delta": delta.delta,
                                }),
                            )
                            .await;

                        if let Some(block_id) = thinking_block_id.as_ref() {
                            emit_stream_block_delta(
                                session_mgr,
                                &request_id,
                                &session_id,
                                block_id,
                                "thinking",
                                &delta.delta,
                            )
                            .await;
                            let completed = {
                                let entry = block_progress.entry(block_id.clone()).or_insert(0);
                                *entry += delta.delta.chars().count();
                                *entry
                            };
                            emit_stream_block_progress(
                                session_mgr,
                                &request_id,
                                &session_id,
                                block_id,
                                "thinking",
                                completed,
                                "streaming",
                            )
                            .await;
                        }
                    }
                    EventMsg::AgentReasoningRawContentDelta(delta) if !delta.delta.is_empty() => {
                        session_mgr
                            .broadcast_to_all(
                                "agent.stream",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "kind": "reasoning_raw",
                                    "delta": delta.delta,
                                }),
                            )
                            .await;
                    }
                    EventMsg::McpToolCallBegin(begin) => {
                        let block_id =
                            next_stream_block_id(&request_id, "tool_call", &mut block_counter);
                        tool_blocks.insert(begin.call_id.clone(), block_id.clone());

                        session_mgr
                            .broadcast_to_all(
                                "agent.stream.block",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "phase": "start",
                                    "block_id": block_id,
                                    "block_kind": "tool_call",
                                    "call_id": begin.call_id,
                                    "server": begin.invocation.server,
                                    "tool": begin.invocation.tool,
                                }),
                            )
                            .await;
                        emit_stream_block_progress(
                            session_mgr,
                            &request_id,
                            &session_id,
                            &block_id,
                            "tool_call",
                            0,
                            "running",
                        )
                        .await;

                        session_mgr
                            .broadcast_to_all(
                                "tool.call",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "call_id": begin.call_id,
                                    "server": begin.invocation.server,
                                    "tool": begin.invocation.tool,
                                    "arguments": begin.invocation.arguments,
                                }),
                            )
                            .await;
                    }
                    EventMsg::McpToolCallEnd(end) => {
                        let result = serde_json::to_value(&end.result).unwrap_or(Value::Null);

                        let block_id = tool_blocks.remove(&end.call_id).unwrap_or_else(|| {
                            next_stream_block_id(&request_id, "tool_call", &mut block_counter)
                        });
                        emit_stream_block_progress(
                            session_mgr,
                            &request_id,
                            &session_id,
                            &block_id,
                            "tool_call",
                            1,
                            if end.is_success() {
                                "complete"
                            } else {
                                "error"
                            },
                        )
                        .await;
                        emit_stream_block_end(
                            session_mgr,
                            &request_id,
                            &session_id,
                            &block_id,
                            "tool_call",
                            if end.is_success() {
                                "complete"
                            } else {
                                "error"
                            },
                        )
                        .await;

                        session_mgr
                            .broadcast_to_all(
                                "tool.result",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "call_id": end.call_id,
                                    "server": end.invocation.server,
                                    "tool": end.invocation.tool,
                                    "duration_ms": end.duration.as_millis() as u64,
                                    "success": end.is_success(),
                                    "result": result,
                                }),
                            )
                            .await;
                    }
                    EventMsg::TurnAborted(reason) => {
                        aborted = true;
                        session_mgr
                            .broadcast_to_all(
                                "agent.error",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "error": format!("turn aborted: {:?}", reason.reason),
                                }),
                            )
                            .await;
                        break;
                    }
                    EventMsg::TurnComplete(_) => break,
                    EventMsg::Error(err) => {
                        session_mgr
                            .broadcast_to_all(
                                "agent.error",
                                json!({
                                    "request_id": request_id,
                                    "session_id": session_id,
                                    "error": err.message,
                                }),
                            )
                            .await;
                        if reply.is_empty() {
                            fatal_error =
                                Some((INTERNAL_ERROR, format!("chat error: {}", err.message)));
                        }
                        break;
                    }
                    _ => {}
                }
            }
            Ok(Err(err)) => {
                session_mgr
                    .broadcast_to_all(
                        "agent.error",
                        json!({
                            "request_id": request_id,
                            "session_id": session_id,
                            "error": format!("thread error: {err}"),
                        }),
                    )
                    .await;
                if reply.is_empty() {
                    fatal_error = Some((INTERNAL_ERROR, format!("thread error: {err}")));
                }
                break;
            }
            Err(_) => {
                let now = tokio::time::Instant::now();
                if now < deadline {
                    emit_typing_heartbeat(session_mgr, &request_id, &session_id, agent).await;
                    next_typing_heartbeat = now + typing_interval;
                    continue;
                }

                session_mgr
                    .broadcast_to_all(
                        "agent.error",
                        json!({
                            "request_id": request_id,
                            "session_id": session_id,
                            "error": format!("timed out waiting for response ({}s)", timeout.as_secs()),
                        }),
                    )
                    .await;
                if reply.is_empty() {
                    fatal_error = Some((
                        INTERNAL_ERROR,
                        format!("timed out waiting for response ({}s)", timeout.as_secs()),
                    ));
                }
                break;
            }
        }
    }

    let final_stream_status = if fatal_error.is_some() {
        "error"
    } else if aborted {
        "aborted"
    } else {
        "complete"
    };

    if let Some(block_id) = text_block_id.as_ref() {
        emit_stream_block_end(
            session_mgr,
            &request_id,
            &session_id,
            block_id,
            "text",
            final_stream_status,
        )
        .await;
    }
    if let Some(block_id) = thinking_block_id.as_ref() {
        emit_stream_block_end(
            session_mgr,
            &request_id,
            &session_id,
            block_id,
            "thinking",
            final_stream_status,
        )
        .await;
    }
    for block_id in tool_blocks.values() {
        emit_stream_block_end(
            session_mgr,
            &request_id,
            &session_id,
            block_id,
            "tool_call",
            final_stream_status,
        )
        .await;
    }
    emit_typing_stop(session_mgr, &request_id, &session_id, agent).await;

    if resolved_session.cleanup_after_turn {
        let _ = channel
            .session_manager()
            .remove_session(&session_id_obj)
            .await;
    }

    if let Some((code, message)) = fatal_error {
        return Err((code, message));
    }

    if reply.is_empty() {
        reply = "(no response from agent)".to_owned();
    }

    let footer = format_model_footer(&effective_model, &provider, model_profile.as_deref());
    let response = append_footer(&reply, &footer);

    persist_chat_session_metadata(
        session_store.as_ref(),
        &channel.config().savfox_home,
        &session_id,
        &thread_id,
        &effective_model,
        &provider,
        rollout_path.as_deref(),
        last_token_usage.as_ref(),
    )
    .await;

    session_mgr
        .broadcast_to_all(
            "agent.complete",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "thread_id": thread_id,
                "response": response,
                "raw_response": reply,
                "model": effective_model,
                "provider": provider,
                "profile": model_profile,
                "aborted": aborted,
            }),
        )
        .await;

    Ok(json!({
        "response": response,
        "raw_response": reply,
        "footer": footer,
        "model": effective_model,
        "provider": provider,
        "profile": model_profile,
        "session_id": session_id,
        "thread_id": thread_id,
        "aborted": aborted,
    }))
}

pub(crate) async fn handle_chat_history(
    params: &Value,
    session_store: &Arc<SessionStore>,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
    let source_channel = params
        .get("source_channel")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty());

    debug!(
        session_id = %session_id,
        limit = limit,
        "chat.history RPC called"
    );

    if session_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'session_id' parameter".to_owned()));
    }

    Ok(build_history_payload(session_id, limit, source_channel, session_store, channel).await)
}

pub(crate) async fn handle_chat_abort(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let thread_id_param = params
        .get("thread_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    let session_id_param = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);

    let candidates = resolve_abort_candidate_ids(
        session_store.as_ref(),
        thread_id_param.as_deref(),
        session_id_param.as_deref(),
    )
    .await;
    if let Some(thread_id) = abort_first_active_candidate(channel.as_ref(), &candidates).await {
        return Ok(json!({ "status": "aborted", "thread_id": thread_id }));
    }

    let aborted = abort_all_active_threads(channel.as_ref()).await;
    Ok(json!({ "status": "aborted", "aborted_count": aborted }))
}

/// Inject a message into a session's history without triggering the agent.
pub(crate) async fn handle_chat_inject(
    params: &Value,
    session_store: &Arc<crate::session::SessionStore>,
) -> RpcResult {
    let session_id = params["session_id"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing session_id".to_owned()))?;
    let content = params["content"]
        .as_str()
        .ok_or((INVALID_PARAMS, "missing content".to_owned()))?;
    let role = params["role"].as_str().unwrap_or("system");

    // Validate role
    if !matches!(role, "system" | "user" | "assistant") {
        return Err((
            INVALID_PARAMS,
            format!("invalid role: {role} (expected system, user, or assistant)"),
        ));
    }

    // Check session exists and touch its timestamp
    let entry = session_store
        .get(session_id)
        .await
        .or(session_store.get_by_session_id(session_id).await)
        .ok_or((INVALID_PARAMS, format!("session not found: {session_id}")))?;

    // Touch session's updated_at via update
    session_store.update(&entry.session_id, |e| e.touch()).await;

    Ok(json!({
        "status": "injected",
        "session_id": entry.session_id,
        "role": role,
        "content_length": content.len(),
    }))
}

// ── Sessions ────────────────────────────────────────────────────────────────

pub(crate) async fn handle_sessions_list(
    session_mgr: &Arc<GatewaySessionManager>,
    session_store: &Arc<SessionStore>,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let ws_ids = session_mgr.session_ids().await;
    let persistent = session_store.list().await;
    let mut sorted_sessions = persistent.clone();
    sorted_sessions.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));

    let persistent_keys: Vec<String> = sorted_sessions
        .iter()
        .map(|e| e.session_id.clone())
        .collect();
    let mut entries: Vec<Value> = Vec::with_capacity(sorted_sessions.len());
    for entry in sorted_sessions {
        let mut label = entry
            .label
            .clone()
            .or_else(|| entry.subject.clone())
            .or_else(|| entry.sender.as_ref().and_then(|s| s.name.clone()));
        if label.is_none() {
            label =
                derive_session_label_from_history(&entry.session_id, session_store, channel).await;
        }

        let last_activity =
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(entry.updated_at as i64)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

        // Resolve the session's bound agent: explicit `agent_id` (web UI
        // sessions) takes precedence, falling back to the agent encoded in
        // the routing id (channel sessions).
        let agent_id = entry.agent_id.clone().or_else(|| {
            entry
                .routing_id
                .as_deref()
                .and_then(agent_from_routing_id)
        });

        entries.push(json!({
            "session_id": entry.session_id,
            "id": entry.session_id,
            "scope": entry.session_id,
            "label": label,
            "subject": entry.subject,
            "sender": entry.sender,
            "identity": entry.identity,
            "provenance_count": entry.provenance.len(),
            "last_activity": last_activity,
            "message_count": Value::Null,
            "messages": Value::Null,
            "model": entry.model,
            "provider": entry.provider,
            "thread_id": entry.thread_id,
            "group_activation": entry.group_activation,
            "agent_id": agent_id,
        }));
    }
    let stats = session_store.stats().await;
    Ok(json!({
        "active_connections": ws_ids,
        "persistent_sessions": persistent_keys,
        "entries": entries,
        "total_persistent": stats.total_sessions,
        "total_tokens": stats.total_tokens,
    }))
}

pub(crate) async fn handle_sessions_ambient_get(
    params: &Value,
    session_store: &Arc<SessionStore>,
    channel: &GatewayChannel,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if session_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'session_id' parameter".to_owned()));
    }
    if session_store.get(session_id).await.is_none() {
        return Err((INVALID_REQUEST, format!("session not found: {session_id}")));
    }

    let messages = peek_ambient_messages(&channel.config().savfox_home, session_id).await;
    Ok(json!({
        "session_id": session_id,
        "messages": messages,
    }))
}

pub(crate) async fn handle_sessions_idle_reply_get(
    params: &Value,
    session_store: &Arc<SessionStore>,
    channel: &GatewayChannel,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if session_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'session_id' parameter".to_owned()));
    }
    if session_store.get(session_id).await.is_none() {
        return Err((INVALID_REQUEST, format!("session not found: {session_id}")));
    }

    let status = get_idle_reply_status(&channel.config().savfox_home, session_id).await;
    Ok(serde_json::to_value(status).unwrap_or_else(|_| {
        json!({
            "session_id": session_id,
            "generation": 0,
            "pending": null,
            "recent_sent_count": 0,
            "recent_sent_at_ms": [],
            "last_suppressed_at_ms": null,
            "suppressed_reason": null,
        })
    }))
}

pub(crate) async fn handle_sessions_preview(
    params: &Value,
    session_store: &Arc<SessionStore>,
    channel: &GatewayChannel,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'session_id' parameter".to_owned()));
    }
    let links = load_identity_links(&channel.config().savfox_home).await;
    match session_store.get(session_id).await {
        Some(entry) => {
            let identity = entry.identity.clone().or_else(|| {
                entry
                    .to
                    .as_deref()
                    .and_then(|peer| canonical_for_peer(&links, peer))
            });
            let linked_identities = identity
                .as_ref()
                .and_then(|id| links.get(id).cloned())
                .unwrap_or_default();
            let value = serde_json::to_value(&entry).unwrap_or(Value::Null);
            Ok(json!({
                "session_id": session_id,
                "entry": value,
                "identity": identity,
                "linked_identities": linked_identities,
            }))
        }
        None => Ok(json!({
            "session_id": session_id,
            "entry": null,
            "identity": null,
            "linked_identities": [],
        })),
    }
}

pub(crate) async fn handle_sessions_patch(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let requested_session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let session_id = if requested_session_id.trim().is_empty() {
        uuid::Uuid::now_v7().to_string()
    } else {
        requested_session_id.to_owned()
    };
    let patch = params
        .get("patch")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    let mut updated = session_store.get_or_create(&session_id).await.ok_or((
        INVALID_REQUEST,
        "invalid 'session_id' parameter (UUID v7 required)".to_owned(),
    ))?;

    // Apply known patch fields.
    if let Some(model) = patch_str(params, &patch, "model") {
        updated.model = Some(model.to_owned());
    }
    if let Some(provider) = patch_str(params, &patch, "provider") {
        updated.provider = Some(provider.to_owned());
    }
    // Agent binding. A session's agent is fixed once set, so only record it
    // when the session does not already have one (e.g. on first creation).
    if updated.agent_id.is_none()
        && let Some(agent) = patch_str(params, &patch, "agent")
    {
        let normalized = agent.trim();
        if !normalized.is_empty() {
            updated.agent_id = Some(normalized.to_owned());
        }
    }
    if let Some(label) = patch_str(params, &patch, "label") {
        let normalized = label.trim();
        updated.label = if normalized.is_empty() {
            None
        } else {
            Some(normalized.to_owned())
        };
    }
    if let Some(channel) = patch_str(params, &patch, "channel") {
        updated.last_channel = updated.channel.take();
        updated.channel = Some(channel.to_owned());
    }
    // Threading support: thread_id and parent_message_id
    if let Some(thread_id) = patch_str(params, &patch, "thread_id") {
        updated.thread_id = Some(thread_id.to_owned());
    }
    if let Some(parent_id) = patch_str(params, &patch, "parent_message_id") {
        updated.parent_message_id = Some(parent_id.to_owned());
    }
    // Group activation mode
    if let Some(ga) = patch_str(params, &patch, "group_activation") {
        let normalized = ga.trim();
        updated.group_activation = if normalized.is_empty() {
            None
        } else {
            Some(normalized.to_owned())
        };
    }

    // Apply overrides from the patch object or from the top-level params.
    let overrides_value = patch.get("overrides").or_else(|| params.get("overrides"));
    if let Some(ov) = overrides_value
        && let Ok(incoming) = serde_json::from_value::<SessionOverrides>(ov.clone())
    {
        updated.patch_overrides(incoming);
    }
    updated.touch();
    session_store.upsert(updated.clone()).await;

    Ok(json!({ "session_id": updated.session_id, "status": "patched" }))
}

fn patch_str<'a>(params: &'a Value, patch: &'a Value, key: &str) -> Option<&'a str> {
    patch
        .get(key)
        .and_then(|v| v.as_str())
        .or_else(|| params.get(key).and_then(|v| v.as_str()))
}

pub(crate) async fn handle_sessions_reset(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
    session_store: &Arc<SessionStore>,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'session_id' parameter".to_owned()));
    }
    let session_id_obj = SessionId::from_string(session_id)
        .map_err(|_| (INVALID_REQUEST, "invalid 'session_id' parameter".to_owned()))?;
    // Remove from WS session manager.
    session_mgr.remove_session(&session_id_obj).await;
    // Remove from persistent store.
    session_store.remove(session_id).await;
    remove_ambient_session(&channel.config().savfox_home, session_id).await;
    remove_idle_reply_session(&channel.config().savfox_home, session_id).await;
    let staging_cleaned = MediaStore::from_home(&channel.config().savfox_home)
        .cleanup_staging_for_session(session_id)
        .await;
    Ok(json!({
        "session_id": session_id,
        "status": "reset",
        "staging_cleaned": staging_cleaned,
    }))
}

pub(crate) async fn handle_sessions_delete(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
    session_store: &Arc<SessionStore>,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'session_id' parameter".to_owned()));
    }
    let session_id_obj = SessionId::from_string(session_id)
        .map_err(|_| (INVALID_REQUEST, "invalid 'session_id' parameter".to_owned()))?;
    session_mgr.remove_session(&session_id_obj).await;
    session_store.remove(session_id).await;
    remove_ambient_session(&channel.config().savfox_home, session_id).await;
    remove_idle_reply_session(&channel.config().savfox_home, session_id).await;
    let staging_cleaned = MediaStore::from_home(&channel.config().savfox_home)
        .cleanup_staging_for_session(session_id)
        .await;
    Ok(json!({ "status": "deleted", "staging_cleaned": staging_cleaned }))
}

pub(crate) async fn handle_sessions_compact(
    params: &Value,
    session_store: &Arc<SessionStore>,
    channel: &GatewayChannel,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        // Global compaction  - prune stale entries.
        let report = session_store.prune_report().await;
        cleanup_session_runtime_state(&channel.config().savfox_home, &report.session_ids).await;
        let pruned = report.pruned;
        return Ok(json!({ "status": "compacted", "pruned": pruned }));
    }
    // Per-session compaction: increment counter.
    match session_store
        .update(session_id, |entry| {
            entry.compaction_count += 1;
        })
        .await
    {
        Some(entry) => Ok(json!({
            "session_id": session_id,
            "status": "compacted",
            "compaction_count": entry.compaction_count,
            "memory_flush_count": entry.memory_flush_count,
            "memory_flush_bytes": entry.memory_flush_bytes,
            "memory_flush_tokens_saved": entry.memory_flush_tokens_saved,
        })),
        None => Err((INVALID_REQUEST, format!("session '{session_id}' not found"))),
    }
}

// ── Session Overrides ────────────────────────────────────────────────────────

pub(crate) async fn handle_sessions_overrides_get(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .or_else(|| params.get("key"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'session_id' parameter".to_owned()));
    }

    let entry = session_store.get(session_id).await;
    match entry {
        Some(e) => {
            let overrides = e.overrides.unwrap_or_default();
            Ok(json!({
                "session_id": session_id,
                "overrides": serde_json::to_value(overrides).unwrap_or(Value::Null),
            }))
        }
        None => Err((INVALID_REQUEST, format!("session '{session_id}' not found"))),
    }
}

pub(crate) async fn handle_sessions_overrides_set(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .or_else(|| params.get("key"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'session_id' parameter".to_owned()));
    }

    let overrides_value = params
        .get("overrides")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));
    let incoming: SessionOverrides = serde_json::from_value(overrides_value)
        .map_err(|e| (INVALID_REQUEST, format!("invalid 'overrides': {e}")))?;

    let mut updated = session_store.get_or_create(session_id).await.ok_or((
        INVALID_REQUEST,
        "invalid 'session_id' parameter (UUID v7 required)".to_owned(),
    ))?;
    updated.patch_overrides(incoming);
    session_store.upsert(updated.clone()).await;

    let overrides = updated.overrides.unwrap_or_default();
    Ok(json!({
        "session_id": updated.session_id,
        "overrides": serde_json::to_value(overrides).unwrap_or(Value::Null),
        "status": "updated",
    }))
}

// ── Identity Linking ────────────────────────────────────────────────────────

pub(crate) async fn handle_identity_links_get(channel: &GatewayChannel) -> RpcResult {
    let links = load_identity_links(&channel.config().savfox_home).await;
    Ok(json!({ "links": links }))
}

pub(crate) async fn handle_identity_links_set(
    params: &Value,
    channel: &GatewayChannel,
) -> RpcResult {
    let input: HashMap<String, Vec<String>> = serde_json::from_value(
        params
            .get("links")
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new())),
    )
    .map_err(|e| (INVALID_PARAMS, format!("invalid links format: {e}")))?;

    let mut merged = HashMap::new();
    for (canonical, peers) in input {
        let _ = upsert_link(&mut merged, &canonical, &peers);
    }

    save_identity_links(&channel.config().savfox_home, &merged)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "status": "updated", "count": merged.len(), "links": merged }))
}

pub(crate) async fn handle_identity_link(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let canonical = params
        .get("canonical")
        .or_else(|| params.get("identity"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if canonical.is_empty() {
        return Err((INVALID_PARAMS, "missing 'canonical' parameter".to_owned()));
    }

    let mut peers: Vec<String> = params
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(single) = params.get("id").and_then(|v| v.as_str()) {
        peers.push(single.to_owned());
    }
    if peers.is_empty() {
        return Err((
            INVALID_PARAMS,
            "missing 'ids' (or 'id') parameter".to_owned(),
        ));
    }

    let mut links = load_identity_links(&channel.config().savfox_home).await;
    let summary = upsert_link(&mut links, canonical, &peers)
        .ok_or_else(|| (INVALID_PARAMS, "invalid canonical or ids".to_owned()))?;
    save_identity_links(&channel.config().savfox_home, &links)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({
        "status": "linked",
        "summary": summary,
        "links": links,
    }))
}

pub(crate) async fn handle_dm_scope_policy_get(channel: &GatewayChannel) -> RpcResult {
    let policy = load_dm_scope_policy(&channel.config().savfox_home).await;
    Ok(json!({ "policy": policy }))
}

pub(crate) async fn handle_dm_scope_policy_set(
    params: &Value,
    channel: &GatewayChannel,
) -> RpcResult {
    let raw = params
        .get("policy")
        .cloned()
        .unwrap_or_else(|| params.clone());
    let mut policy: DmScopePolicyConfig = serde_json::from_value(raw)
        .map_err(|e| (INVALID_PARAMS, format!("invalid policy: {e}")))?;
    policy.normalize();

    write_dm_scope_policy(&channel.config().savfox_home, &policy)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "status": "updated", "policy": policy }))
}

fn agent_from_routing_id(routing_id: &str) -> Option<String> {
    let rest = routing_id.strip_prefix("agent:")?;
    let agent = rest.split(':').next()?.trim();
    if agent.is_empty() {
        None
    } else {
        Some(agent.to_owned())
    }
}

fn merge_session_entries(mut left: SessionEntry, right: SessionEntry) -> SessionEntry {
    if right.updated_at > left.updated_at {
        left.updated_at = right.updated_at;
        left.channel = right.channel.clone().or(left.channel);
        left.last_channel = right.last_channel.clone().or(left.last_channel);
        left.to = right.to.clone().or(left.to);
        left.last_to = right.last_to.clone().or(left.last_to);
        left.thread_id = right.thread_id.clone().or(left.thread_id);
        left.reply_target = right.reply_target.clone().or(left.reply_target);
        left.parent_thread_id = right.parent_thread_id.clone().or(left.parent_thread_id);
        left.parent_message_id = right.parent_message_id.clone().or(left.parent_message_id);
        left.identity = right.identity.clone().or(left.identity);
    }

    left.input_tokens = left.input_tokens.saturating_add(right.input_tokens);
    left.output_tokens = left.output_tokens.saturating_add(right.output_tokens);
    left.total_tokens = left.total_tokens.saturating_add(right.total_tokens);
    left.compaction_count = left.compaction_count.saturating_add(right.compaction_count);
    left.memory_flush_count = left
        .memory_flush_count
        .saturating_add(right.memory_flush_count);
    left.memory_flush_bytes = left
        .memory_flush_bytes
        .saturating_add(right.memory_flush_bytes);
    left.memory_flush_tokens_saved = left
        .memory_flush_tokens_saved
        .saturating_add(right.memory_flush_tokens_saved);
    left.provenance.extend(right.provenance);
    left.provenance.sort_by_key(|item| item.timestamp);
    left
}

pub(crate) async fn handle_dm_scope_migrate(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let target_scope = parse_dm_scope(params.get("scope").and_then(|v| v.as_str()))
        .ok_or_else(|| (INVALID_PARAMS, "invalid or missing 'scope'".to_owned()))?;
    let dry_run = params
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let filter_agent = params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty());
    let filter_channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty());

    let mut moved = 0usize;
    let mut skipped = 0usize;
    let merged = 0usize;
    let mut rebuilt: HashMap<String, SessionEntry> = HashMap::new();

    for mut entry in session_store.list().await {
        if entry.group_id.is_some() {
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        }

        let Some(routing_id) = entry.routing_id.as_deref() else {
            skipped = skipped.saturating_add(1);
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        };
        let Some(agent) = agent_from_routing_id(routing_id) else {
            skipped = skipped.saturating_add(1);
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        };
        if let Some(filter) = filter_agent.as_deref()
            && agent.to_ascii_lowercase() != filter
        {
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        }
        if let Some(filter) = filter_channel.as_deref()
            && entry
                .channel
                .as_deref()
                .map(|ch| ch.to_ascii_lowercase())
                .as_deref()
                != Some(filter)
        {
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        }

        let peer = entry
            .to
            .clone()
            .or(entry.identity.clone())
            .or_else(|| entry.sender.as_ref().and_then(|s| s.name.clone()));
        let Some(peer) = peer else {
            skipped = skipped.saturating_add(1);
            rebuilt.insert(entry.session_id.clone(), entry);
            continue;
        };

        let new_routing_id = build_routing_id(
            &agent,
            entry.channel.as_deref(),
            entry.group_id.as_deref(),
            entry.thread_id.as_deref(),
            Some(peer.as_str()),
            entry.account_id.as_deref(),
            target_scope,
        );
        if entry.routing_id.as_deref() != Some(new_routing_id.as_str()) {
            moved = moved.saturating_add(1);
            entry.routing_id = Some(new_routing_id);
        }
        rebuilt.insert(entry.session_id.clone(), entry);
    }

    if !dry_run {
        session_store
            .replace_all(rebuilt.clone())
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("persist error: {e}")))?;
    }

    Ok(json!({
        "status": if dry_run { "dry_run" } else { "migrated" },
        "scope": target_scope.as_str(),
        "moved": moved,
        "merged": merged,
        "skipped": skipped,
        "sessions": rebuilt.len(),
        "filters": {
            "agent": filter_agent,
            "channel": filter_channel,
        }
    }))
}

// ── Sessions Usage ──────────────────────────────────────────────────────────

pub(crate) async fn handle_sessions_usage(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if session_id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'session_id' parameter".to_owned()));
    }

    match session_store.get(session_id).await {
        Some(entry) => {
            let total = entry.total_tokens;
            let input = entry.input_tokens;
            let output = entry.output_tokens;

            // Context weight analysis (estimated percentages)
            let system_pct = if total > 0 {
                (input as f64 * 0.3 / total as f64 * 100.0).round()
            } else {
                0.0
            };
            let history_pct = if total > 0 {
                (input as f64 * 0.6 / total as f64 * 100.0).round()
            } else {
                0.0
            };
            let tools_pct = if total > 0 {
                (input as f64 * 0.1 / total as f64 * 100.0).round()
            } else {
                0.0
            };

            Ok(json!({
                "session_id": session_id,
                "input_tokens": input,
                "output_tokens": output,
                "total_tokens": total,
                "model": entry.model,
                "provider": entry.provider,
                "created_at": entry.created_at,
                "updated_at": entry.updated_at,
                "context_weight": {
                    "system_prompt_pct": system_pct,
                    "history_pct": history_pct,
                    "tools_pct": tools_pct,
                },
            }))
        }
        None => Err((INVALID_PARAMS, format!("session not found: {session_id}"))),
    }
}

// ── Media Staging ───────────────────────────────────────────────────────────

pub(crate) async fn handle_media_staging_list(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let session_id = params.get("session_id").and_then(|v| v.as_str());
    let store = MediaStore::from_home(&channel.config().savfox_home);
    let entries = store.list_staging(session_id).await;
    Ok(json!({
        "entries": entries,
        "count": entries.len(),
        "session_id": session_id,
    }))
}

pub(crate) async fn handle_media_staging_import(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.trim().is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_owned()));
    }
    let workspace_dir = params
        .get("workspace_dir")
        .or_else(|| params.get("workspace"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                INVALID_PARAMS,
                "missing 'workspace_dir' parameter".to_owned(),
            )
        })?;
    if workspace_dir.trim().is_empty() {
        return Err((INVALID_PARAMS, "workspace_dir cannot be empty".to_owned()));
    }

    let store = MediaStore::from_home(&channel.config().savfox_home);
    let imported_path = store
        .import_from_staging(id, &PathBuf::from(workspace_dir))
        .await
        .map_err(|e| (INVALID_PARAMS, e))?;
    Ok(json!({
        "status": "ok",
        "id": id,
        "workspace_dir": workspace_dir,
        "imported_path": imported_path,
    }))
}

pub(crate) async fn handle_media_staging_cleanup(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if session_id.trim().is_empty() {
        return Err((INVALID_PARAMS, "missing 'session_id' parameter".to_owned()));
    }

    let store = MediaStore::from_home(&channel.config().savfox_home);
    let removed = store.cleanup_staging_for_session(session_id).await;
    Ok(json!({
        "status": "ok",
        "session_id": session_id,
        "removed": removed,
    }))
}

// ── Typing Indicators ───────────────────────────────────────────────────────

pub(crate) async fn handle_typing_start(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    session_mgr
        .broadcast_to_all(
            "typing.start",
            json!({
                "session_id": session_id,
                "agent_id": agent_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;

    Ok(json!({ "status": "typing_started", "session_id": session_id }))
}

pub(crate) async fn handle_typing_stop(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    session_mgr
        .broadcast_to_all(
            "typing.stop",
            json!({
                "session_id": session_id,
                "agent_id": agent_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;

    Ok(json!({ "status": "typing_stopped", "session_id": session_id }))
}

// ── Events (Server-Push) ────────────────────────────────────────────────────

/// Known event types that can be subscribed to.
const EVENT_TYPES: &[&str] = &[
    "agent.stream",
    "agent.stream.block",
    "agent.stream.progress",
    "agent.complete",
    "agent.error",
    "tool.call",
    "tool.result",
    "typing.start",
    "typing.stop",
    "session.updated",
    "session.created",
    "session.deleted",
    "config.changed",
    "channel.status",
    "channel.connected",
    "channel.disconnected",
    "approval.requested",
    "approval.resolved",
    "system.event",
    "system.presence",
    "cron.started",
    "cron.completed",
    "memory.updated",
];

pub(crate) async fn handle_events_subscribe(params: &Value) -> RpcResult {
    let events: Vec<String> = params
        .get("events")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                .collect()
        })
        .unwrap_or_default();

    if events.is_empty() {
        return Err((
            INVALID_PARAMS,
            "missing 'events' array parameter".to_owned(),
        ));
    }

    // Validate event patterns (support wildcards like "agent.*")
    let valid: Vec<&str> = events.iter().map(|s| s.as_str()).collect();
    let invalid: Vec<&&str> = valid
        .iter()
        .filter(|e| !e.contains('*') && !EVENT_TYPES.contains(e))
        .collect();

    if !invalid.is_empty() {
        // Allow unknown events too (for forward compat), just note them
    }

    Ok(json!({
        "status": "subscribed",
        "events": events,
        "count": events.len(),
    }))
}

pub(crate) async fn handle_events_unsubscribe(params: &Value) -> RpcResult {
    let events: Vec<String> = params
        .get("events")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                .collect()
        })
        .unwrap_or_default();

    Ok(json!({
        "status": "unsubscribed",
        "events": events,
        "count": events.len(),
    }))
}

pub(crate) async fn handle_events_list() -> RpcResult {
    let events: Vec<Value> = EVENT_TYPES.iter().map(|e| json!({ "event": e })).collect();

    Ok(json!({
        "events": events,
        "count": events.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::{TerminalAgentEvent, terminal_event_stream_payload};

    fn terminal_stream_order(events: &[TerminalAgentEvent]) -> Vec<String> {
        let mut order = vec!["started".to_owned()];
        for event in events {
            if let Some((_topic, payload)) = terminal_event_stream_payload(
                "req", "session", "thread", event, false, true, "logs",
            ) && let Some(kind) = payload
                .get("terminal_event")
                .and_then(|value| value.as_str())
            {
                order.push(kind.to_owned());
            }
        }
        order.push("message".to_owned());
        order.push("completed".to_owned());
        order
    }

    #[test]
    fn terminal_event_stream_order_keeps_terminal_lifecycle_stable() {
        let order = terminal_stream_order(&[
            TerminalAgentEvent::Log {
                stream: "stderr".to_owned(),
                text: "warning".to_owned(),
            },
            TerminalAgentEvent::Status {
                status: "running".to_owned(),
            },
            TerminalAgentEvent::Message {
                text: "reply chunk".to_owned(),
            },
            TerminalAgentEvent::Completed { exit_code: Some(0) },
        ]);

        assert_eq!(order, ["started", "log", "status", "message", "completed"]);
    }

    #[test]
    fn terminal_event_stream_payload_marks_stderr_truncation() {
        let (_topic, payload) = terminal_event_stream_payload(
            "req",
            "session",
            "thread",
            &TerminalAgentEvent::Log {
                stream: "stderr".to_owned(),
                text: "warning".to_owned(),
            },
            false,
            true,
            "logs",
        )
        .expect("stderr log maps to stream payload");

        assert_eq!(payload["terminal_event"], "log");
        assert_eq!(payload["truncated"], true);
    }
}
