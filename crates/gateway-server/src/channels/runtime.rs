mod coordinator;
mod delivery;
mod footer;
mod memory;
mod routing;
mod trigger;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use savfox_core::models_manager::manager::RefreshStrategy;
use tracing::warn;

pub(crate) type ChannelHealthMetrics = delivery::ChannelHealthMetrics;
pub(crate) use self::delivery::{
    channel_health_snapshot, record_channel_probe, send_metrics_snapshot, should_drop_duplicate,
};
use self::delivery::{record_channel_event, send_with_retry};
pub(crate) use self::footer::set_response_footer_config;
use self::footer::{append_footer, current_response_footer_config, format_model_footer};
use self::memory::maybe_auto_memory_flush;
pub(crate) use self::routing::StartThreadMeta;
use self::routing::{
    PolicyDecision, check_channel_policies, load_agent_trigger_config, resolve_linked_identity,
    resolve_routed_agent, resolve_text_target_match,
};
pub(crate) use self::trigger::SenderKind;
use self::trigger::{
    TriggerContext, TriggerDecision, TriggerReason, apply_agent_trigger_policy, decide_trigger,
    effective_conversation_kind,
};
use crate::auto_reply::directives::{
    DirectiveKind, fuzzy_match_model_name, parse_directives, parse_model_target,
};
use crate::auto_reply::{CommandAction, CommandContext, CommandRegistry};
use crate::channel::GatewayChannel;
use crate::channels::policy::{
    append_channel_tone_suffix, configured_channel_tone_suffix, configured_dm_scope,
};
use crate::log_store;
use crate::session::{
    AmbientMessage, InboundSessionMeta, SessionOverrides, SessionStore, format_ambient_context,
    prepend_ambient_context, push_ambient_message, session_file_to_store_value,
    take_ambient_messages, track_inbound_message, track_token_usage,
};

fn command_registry() -> &'static CommandRegistry {
    static REGISTRY: OnceLock<CommandRegistry> = OnceLock::new();
    REGISTRY.get_or_init(CommandRegistry::new)
}

fn command_result_message(
    result: &crate::auto_reply::CommandResult,
    fallback: &'static str,
) -> String {
    if let Some(error) = result.error.as_deref() {
        return format!("Command error: {error}");
    }
    result.reply.clone().unwrap_or_else(|| fallback.to_owned())
}

fn looks_like_textual_approval_reply(text: &str) -> bool {
    let normalized = text.trim();
    matches!(normalized, "+" | "-")
        || approval_command_id(normalized, "approve").is_some()
        || approval_command_id(normalized, "deny").is_some()
}

fn approval_command_id<'a>(text: &'a str, command: &str) -> Option<&'a str> {
    let (head, tail) = text.split_once(':')?;
    if head.eq_ignore_ascii_case(command) {
        let id = tail.trim();
        if !id.is_empty() {
            return Some(id);
        }
    }
    None
}

async fn apply_command_action(
    gateway_channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
    session_id: &str,
    action: &CommandAction,
) {
    match action {
        CommandAction::SetModel { model } => {
            let model_value = model.clone();
            let provider_value = model_value
                .split('/')
                .next()
                .map(std::string::ToString::to_string);
            let _ = session_store
                .update(session_id, move |entry| {
                    entry.model = Some(model_value.clone());
                    entry.provider = provider_value.clone();
                    entry.patch_overrides(SessionOverrides {
                        model: Some(model_value),
                        ..SessionOverrides::default()
                    });
                })
                .await;
        }
        CommandAction::SetThinking { level } => {
            let level_value = level.clone();
            let _ = session_store
                .update(session_id, move |entry| {
                    entry.patch_overrides(SessionOverrides {
                        thinking: Some(level_value),
                        ..SessionOverrides::default()
                    });
                })
                .await;
        }
        CommandAction::SetVerbose { enabled } => {
            let verbose_value = if *enabled {
                "on".to_owned()
            } else {
                "off".to_owned()
            };
            let _ = session_store
                .update(session_id, move |entry| {
                    entry.patch_overrides(SessionOverrides {
                        verbose: Some(verbose_value),
                        ..SessionOverrides::default()
                    });
                })
                .await;
        }
        CommandAction::Compact { .. } => {
            let _ = session_store
                .update(session_id, |entry| {
                    entry.compaction_count = entry.compaction_count.saturating_add(1);
                    entry.touch();
                })
                .await;
        }
        CommandAction::Reset => {
            let _ = session_store
                .update(session_id, |entry| {
                    entry.model = None;
                    entry.provider = None;
                    entry.overrides = None;
                    entry.thread_id = None;
                    entry.session_file = None;
                    entry.touch();
                })
                .await;
            gateway_channel
                .unbind_logical_session_thread(session_id)
                .await;
        }
        CommandAction::Clear => {
            // Clear conversation history on the existing agent thread.
            // Keeps the same thread and all current settings (model, thinking, etc.).
            gateway_channel
                .rollback_logical_session(session_id, u32::MAX)
                .await;
        }
        CommandAction::NewSession => {
            // Create a brand new agent thread, reset all settings to defaults.
            let _ = session_store
                .update(session_id, |entry| {
                    entry.model = None;
                    entry.provider = None;
                    entry.overrides = None;
                    entry.thread_id = None;
                    entry.session_file = None;
                    entry.touch();
                })
                .await;
            gateway_channel
                .unbind_logical_session_thread(session_id)
                .await;
        }
        CommandAction::Stop => {
            // Interrupt the active agent session if one is running.
            gateway_channel.interrupt_logical_session(session_id).await;
        }
        CommandAction::ShowHelp | CommandAction::ShowStatus => {}
    }
}

pub(crate) async fn spawn_start_thread_pipeline(
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    platform: &'static str,
    channel_id: String,
    prompt: String,
    name: Option<String>,
) {
    dispatch_to_coordinator(
        gateway_channel,
        session_store,
        platform,
        channel_id,
        prompt,
        name,
        None,
    )
    .await;
}

pub(crate) async fn spawn_start_thread_pipeline_with_meta_coordinated(
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    platform: &'static str,
    channel_id: String,
    prompt: String,
    name: Option<String>,
    meta: Option<StartThreadMeta>,
) {
    dispatch_to_coordinator(
        gateway_channel,
        session_store,
        platform,
        channel_id,
        prompt,
        name,
        meta,
    )
    .await;
}

/// Route an inbound message through the per-session coordinator.
/// This enables the `select!` loop that detects new messages during processing.
async fn dispatch_to_coordinator(
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    platform: &'static str,
    channel_id: String,
    prompt: String,
    name: Option<String>,
    meta: Option<StartThreadMeta>,
) {
    let session_key = format!("{platform}:{channel_id}");
    let task = coordinator::InboundTask {
        platform,
        channel_id,
        prompt,
        name,
        meta,
        gateway_channel,
        session_store,
    };
    coordinator::dispatch(session_key, task).await;
}

pub(crate) async fn spawn_start_thread_pipeline_with_meta(
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    platform: &'static str,
    channel_id: String,
    prompt: String,
    name: Option<String>,
    meta: Option<StartThreadMeta>,
) {
    let parsed = parse_directives(&prompt);
    let cleaned_prompt = if parsed.directives.is_empty() {
        prompt.trim().to_owned()
    } else {
        parsed.cleaned_text.trim().to_owned()
    };
    if cleaned_prompt.is_empty() {
        let outbound_channel = format!("{platform}:{channel_id}");
        let msg = "Savfox: message is empty after parsing directives.";
        let _ = send_with_retry(
            &gateway_channel,
            &outbound_channel,
            msg,
            Some(1),
            None,
            None,
        )
        .await;
        return;
    }

    let outbound_channel = format!("{platform}:{channel_id}");
    record_channel_event(&outbound_channel).await;
    let start_meta = meta.unwrap_or_default();

    // Check DM/group access policies before processing the message.
    if let PolicyDecision::Block(reason) = check_channel_policies(
        &gateway_channel.config().savfox_home,
        platform,
        &channel_id,
        start_meta.peer_id.as_deref(),
        start_meta.chat_type.as_deref(),
        start_meta.group_id.as_deref(),
        start_meta.saved_channel_config_id.as_deref(),
    )
    .await
    {
        log_store::append_log(
            "info",
            "channel/runtime",
            format!("message blocked by policy: channel={outbound_channel}, reason={reason}"),
        )
        .await;
        return;
    }

    let linked_identity = resolve_linked_identity(
        &gateway_channel.config().savfox_home,
        &session_store,
        platform,
        start_meta.peer_id.as_deref(),
        name.as_deref(),
    )
    .await;
    let default_routed_agent = resolve_routed_agent(
        &gateway_channel,
        &session_store,
        platform,
        &channel_id,
        name.as_deref(),
        &start_meta,
    )
    .await;
    let text_target_match =
        resolve_text_target_match(&gateway_channel.config().savfox_home, &cleaned_prompt).await;
    let routed_agent = text_target_match
        .agent_id
        .clone()
        .unwrap_or(default_routed_agent);
    let cleaned_prompt = text_target_match
        .stripped_prompt
        .filter(|value| !value.is_empty())
        .unwrap_or(cleaned_prompt);
    let dm_scope = if let Some(scope) = start_meta.dm_scope {
        scope
    } else {
        configured_dm_scope(
            &gateway_channel.config().savfox_home,
            &routed_agent,
            &outbound_channel,
        )
        .await
    };

    let tracked = track_inbound_message(
        &session_store,
        InboundSessionMeta {
            agent_id: &routed_agent,
            platform,
            channel_id: &channel_id,
            routing_group_id: start_meta.routing_group_id.as_deref(),
            routing_thread_id: start_meta.routing_thread_id.as_deref(),
            peer_id: start_meta.peer_id.as_deref(),
            identity: linked_identity.as_deref(),
            group_id: start_meta.group_id.as_deref(),
            thread_id: start_meta.thread_id.as_deref(),
            parent_thread_id: start_meta.parent_thread_id.as_deref(),
            reply_target: start_meta.reply_target.as_deref(),
            account_id: start_meta.account_id.as_deref(),
            name: name.as_deref(),
            topic: start_meta.topic.as_deref(),
            first_message: Some(cleaned_prompt.as_str()),
            chat_type: start_meta.chat_type.as_deref(),
            dm_scope,
        },
    )
    .await;
    log_store::append_log(
        "info",
        "channel/runtime",
        format!(
            "start_thread platform={platform} channel={channel_id} session_id={}",
            tracked.session_id
        ),
    )
    .await;

    if looks_like_textual_approval_reply(&cleaned_prompt) {
        match gateway_channel
            .resolve_agent_session(
                gateway_channel.config().as_ref().clone(),
                Some(&tracked.session_id),
            )
            .await
        {
            Ok(resolved_session) => match gateway_channel
                .session_manager()
                .get_session(resolved_session.session_id)
                .await
            {
                Ok(session) => match session.maybe_submit_textual_approval(&cleaned_prompt).await {
                    Ok(true) => {
                        log_store::append_log(
                            "info",
                            "channel/runtime",
                            format!(
                                "resolved pending approval from channel reply: channel={outbound_channel}, session_id={}",
                                tracked.session_id
                            ),
                        )
                        .await;
                        return;
                    }
                    Ok(false) => {
                        log_store::append_log(
                            "info",
                            "channel/runtime",
                            format!(
                                "approval-like reply did not match a pending approval: channel={outbound_channel}, session_id={}",
                                tracked.session_id
                            ),
                        )
                        .await;
                    }
                    Err(err) => {
                        warn!(
                            channel = %outbound_channel,
                            "channel runtime: failed to submit approval reply: {err}"
                        );
                    }
                },
                Err(err) => {
                    warn!(
                        channel = %outbound_channel,
                        "channel runtime: failed to load active session for approval reply: {err}"
                    );
                }
            },
            Err(err) => {
                warn!(
                    channel = %outbound_channel,
                    "channel runtime: failed to resolve active session for approval reply: {err}"
                );
            }
        }
    }

    let runtime_command = command_registry().has_command(&cleaned_prompt);
    let agent_trigger_config =
        load_agent_trigger_config(&gateway_channel.config().savfox_home, &routed_agent).await;
    let effective_sender_kind = if matches!(start_meta.sender_kind, SenderKind::ExternalBot)
        && matches!(
            agent_trigger_config.external_bot_policy,
            self::trigger::ExternalBotPolicy::ReplyAllowed
        ) {
        SenderKind::Human
    } else {
        start_meta.sender_kind
    };
    let text_targets_current_agent = text_target_match
        .agent_id
        .as_deref()
        .is_some_and(|agent_id| agent_id.eq_ignore_ascii_case(&routed_agent));
    let text_targets_other_agent = text_target_match
        .agent_id
        .as_deref()
        .is_some_and(|agent_id| !agent_id.eq_ignore_ascii_case(&routed_agent));
    let effective_is_mentioned = start_meta.is_mentioned || text_targets_current_agent;
    let effective_targets_other_agent =
        start_meta.explicitly_targets_other_agent || text_targets_other_agent;
    let conversation_kind = effective_conversation_kind(
        start_meta.chat_type.as_deref(),
        start_meta.participant_count,
    );
    let base_trigger_decision = if matches!(start_meta.sender_kind, SenderKind::ExternalBot)
        && matches!(
            agent_trigger_config.external_bot_policy,
            self::trigger::ExternalBotPolicy::IngestOnly
        ) {
        TriggerDecision::IngestOnly {
            reason: TriggerReason::ExternalBotIngestOnly,
        }
    } else {
        decide_trigger(
            effective_sender_kind,
            start_meta.chat_type.as_deref(),
            start_meta.participant_count,
            effective_is_mentioned,
            start_meta.reply_to_self,
            start_meta.is_command || runtime_command,
            start_meta.used_plain_text_fallback,
            effective_targets_other_agent,
        )
    };
    let trigger_decision = apply_agent_trigger_policy(
        base_trigger_decision,
        TriggerContext {
            sender_kind: start_meta.sender_kind,
            conversation_kind,
            is_mentioned: effective_is_mentioned,
            reply_to_self: start_meta.reply_to_self,
            is_command: start_meta.is_command || runtime_command,
            explicitly_targets_other_agent: effective_targets_other_agent,
            text: &cleaned_prompt,
        },
        &agent_trigger_config,
    );
    match &trigger_decision {
        TriggerDecision::Ignore { reason } => {
            log_store::append_log(
                "info",
                "channel/runtime",
                format!(
                    "message ignored by trigger: channel={outbound_channel}, session_id={}, reason={}",
                    tracked.session_id,
                    reason.as_str()
                ),
            )
            .await;
            return;
        }
        TriggerDecision::IngestOnly { reason } => {
            push_ambient_message(
                &tracked.session_id,
                AmbientMessage {
                    timestamp_ms: crate::json_store::now_ms(),
                    sender_id: start_meta.peer_id.clone(),
                    sender_name: name.clone(),
                    sender_kind: start_meta.sender_kind.as_str().to_owned(),
                    text: cleaned_prompt.clone(),
                    reason: reason.as_str().to_owned(),
                },
            )
            .await;
            log_store::append_log(
                "info",
                "channel/runtime",
                format!(
                    "message buffered without reply: channel={outbound_channel}, session_id={}, reason={}",
                    tracked.session_id,
                    reason.as_str()
                ),
            )
            .await;
            return;
        }
        TriggerDecision::Reply { .. } => {}
    }

    if runtime_command {
        let mut metadata = HashMap::new();
        if let Some(model) = tracked
            .overrides
            .as_ref()
            .and_then(|o| o.model.as_ref())
            .or(tracked.model.as_ref())
        {
            metadata.insert("model".to_owned(), model.clone());
        }
        metadata.insert("tokens_used".to_owned(), tracked.total_tokens.to_string());

        let command_ctx = CommandContext {
            sender_id: start_meta
                .peer_id
                .clone()
                .or_else(|| name.clone())
                .unwrap_or_else(|| format!("{platform}:{channel_id}")),
            channel_id: outbound_channel.clone(),
            session_id: Some(tracked.session_id.clone()),
            is_authorized: true,
            is_mentioned: effective_is_mentioned,
            is_group: matches!(tracked.chat_type.as_deref(), Some("group" | "channel")),
            metadata,
        };

        if let Some(result) = command_registry().handle_command(&cleaned_prompt, &command_ctx) {
            if let Some(action) = result.action.as_ref() {
                apply_command_action(
                    &gateway_channel,
                    &session_store,
                    &tracked.session_id,
                    action,
                )
                .await;
            }

            let response = command_result_message(&result, "Command executed.");
            if let Err(err) = send_with_retry(
                &gateway_channel,
                &outbound_channel,
                &response,
                None,
                tracked.thread_id.as_deref(),
                tracked.reply_target.as_deref(),
            )
            .await
            {
                warn!(
                    channel = %outbound_channel,
                    "channel runtime: failed to send command reply: {err}"
                );
                log_store::append_log(
                    "warn",
                    "channel/runtime",
                    format!("send command reply failed: channel={outbound_channel}, err={err}"),
                )
                .await;
            }
            return;
        }
    }

    let model_directive = parsed
        .directives
        .iter()
        .rev()
        .find(|d| d.kind == DirectiveKind::Model)
        .map(|d| d.value.clone());
    let (effective_model, model_profile) = if let Some(raw_model) = model_directive {
        let parsed_target = parse_model_target(&raw_model);
        let candidates: Vec<String> = gateway_channel
            .session_manager()
            .list_models(gateway_channel.config(), RefreshStrategy::Offline)
            .await
            .into_iter()
            .map(|m| m.id)
            .collect();
        let resolved = fuzzy_match_model_name(&parsed_target.model, &candidates)
            .unwrap_or(parsed_target.model);
        (resolved, parsed_target.profile)
    } else {
        (routed_agent.clone(), None)
    };
    let provider = effective_model
        .split('/')
        .next()
        .unwrap_or("unknown")
        .to_owned();
    let tone_suffix = configured_channel_tone_suffix(
        &gateway_channel.config().savfox_home,
        &routed_agent,
        &outbound_channel,
    )
    .await;
    let effective_prompt = append_channel_tone_suffix(&cleaned_prompt, tone_suffix.as_deref());
    if tone_suffix.is_some() {
        log_store::append_log(
            "info",
            "channel/runtime",
            format!(
                "channel tone override applied: channel={outbound_channel}, agent={routed_agent}"
            ),
        )
        .await;
    }
    let ambient_context = format_ambient_context(&take_ambient_messages(&tracked.session_id).await);
    let effective_prompt = prepend_ambient_context(&effective_prompt, ambient_context.as_deref());
    // Set up streaming for channels that support progressive message editing.
    // Supported: Telegram, Discord, Slack, Mattermost, Feishu/Lark, DingTalk.
    use super::channel_stream::{
        ChannelStreamWriter, StreamEvent, StreamSinkContext, create_stream_sink,
        send_status_message,
    };
    let sink_ctx = StreamSinkContext {
        peer_id: start_meta.peer_id.as_deref(),
        thread_id: start_meta.thread_id.as_deref(),
        chat_type: start_meta.chat_type.as_deref(),
    };
    let (stream_tx, stream_handle) = if let Some(sink) = create_stream_sink(
        platform,
        &channel_id,
        tracked.reply_target.as_deref(),
        &gateway_channel,
        Some(&sink_ctx),
    )
    .await
    {
        // Send initial "⏳" status message so the user gets immediate feedback.
        let status_msg_id = send_status_message(sink.as_ref()).await;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
        let writer = ChannelStreamWriter::new(sink, status_msg_id);
        (Some(tx), Some(tokio::spawn(writer.run(rx))))
    } else {
        (None, None)
    };

    // Set up approval notification: when the agent needs tool approval,
    // send a message to the channel so the user can reply with + or -.
    let (approval_tx, mut approval_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let approval_gw = Arc::clone(&gateway_channel);
    let approval_channel = outbound_channel.clone();
    let approval_thread_id = tracked.thread_id.clone();
    let approval_reply_target = tracked.reply_target.clone();
    let approval_task = tokio::spawn(async move {
        while let Some(msg) = approval_rx.recv().await {
            let _ = send_with_retry(
                &approval_gw,
                &approval_channel,
                &msg,
                Some(1),
                approval_thread_id.as_deref(),
                approval_reply_target.as_deref(),
            )
            .await;
        }
    });

    let on_approval: Box<dyn FnMut(&str) + Send> = Box::new(move |msg: &str| {
        let _ = approval_tx.send(msg.to_owned());
    });

    let delta_tx = stream_tx.clone();
    match gateway_channel
        .invoke_agent_text_in_session_with_approval(
            &effective_prompt,
            &effective_model,
            Some(&tracked.session_id),
            move |delta: &str| {
                if let Some(tx) = &delta_tx {
                    let _ = tx.send(StreamEvent::Delta(delta.to_owned()));
                }
            },
            on_approval,
        )
        .await
    {
        Ok(result) => {
            let mut input_tokens = 0_u64;
            let mut output_tokens = 0_u64;
            if let Some(tokens) = result.last_token_usage.as_ref() {
                input_tokens = tokens.input_tokens.max(0) as u64;
                output_tokens = tokens.output_tokens.max(0) as u64;
                track_token_usage(
                    &session_store,
                    &tracked.session_id,
                    input_tokens,
                    output_tokens,
                )
                .await;
            }

            maybe_auto_memory_flush(
                &session_store,
                &gateway_channel.config().savfox_home,
                &tracked.session_id,
                tracked.total_tokens,
                input_tokens,
                output_tokens,
                &cleaned_prompt,
                &result.reply,
            )
            .await;

            let footer_config = current_response_footer_config();
            let footer_text = format_model_footer(
                &footer_config,
                platform,
                &effective_model,
                &provider,
                model_profile.as_deref(),
                result.last_token_usage.as_ref(),
            );
            let reply = if let Some(ref footer) = footer_text {
                append_footer(&result.reply, footer)
            } else {
                result.reply.clone()
            };
            if let Some(path) = result.rollout_path {
                let session_file =
                    session_file_to_store_value(&gateway_channel.config().savfox_home, &path);
                let thread_id = result.thread_id;
                let session_id = tracked.session_id.clone();
                let _ = session_store
                    .update(&session_id, move |entry| {
                        entry.session_file = Some(session_file.clone());
                        if entry.thread_id.is_none() {
                            entry.thread_id = Some(thread_id);
                        }
                    })
                    .await;
            }

            // Signal streaming completion with footer text.
            let streamed = if let Some(tx) = stream_tx {
                let footer_suffix = footer_text.map(|f| format!("\n\n{f}"));
                let _ = tx.send(StreamEvent::Complete {
                    footer: footer_suffix,
                });
                drop(tx);
                if let Some(handle) = stream_handle {
                    handle.await.unwrap_or(false)
                } else {
                    false
                }
            } else {
                false
            };

            if !streamed {
                // Normal send path (non-streaming or streaming fell back).
                if let Err(err) = send_with_retry(
                    &gateway_channel,
                    &outbound_channel,
                    &reply,
                    None,
                    tracked.thread_id.as_deref(),
                    tracked.reply_target.as_deref(),
                )
                .await
                {
                    warn!(
                        channel = %outbound_channel,
                        "channel runtime: failed to send agent reply after retries: {err}"
                    );
                    log_store::append_log(
                        "warn",
                        "channel/runtime",
                        format!(
                            "send reply failed after retries: channel={outbound_channel}, err={err}"
                        ),
                    )
                    .await;
                } else {
                    log_store::append_log(
                        "info",
                        "channel/runtime",
                        format!(
                            "reply sent: channel={outbound_channel}, bytes={}",
                            reply.len()
                        ),
                    )
                    .await;
                }
            } else {
                log_store::append_log(
                    "info",
                    "channel/runtime",
                    format!(
                        "reply streamed: channel={outbound_channel}, bytes={}",
                        reply.len()
                    ),
                )
                .await;
            }
        }
        Err(err) => {
            // Close streaming channel on error so the writer task exits.
            drop(stream_tx);
            if let Some(handle) = stream_handle {
                let _ = handle.await;
            }

            let fallback = format!("Savfox agent error: {err}");
            if let Err(send_err) = send_with_retry(
                &gateway_channel,
                &outbound_channel,
                &fallback,
                None,
                tracked.thread_id.as_deref(),
                tracked.reply_target.as_deref(),
            )
            .await
            {
                warn!(
                    channel = %outbound_channel,
                    "channel runtime: failed to send error reply: {send_err}"
                );
                log_store::append_log(
                    "warn",
                    "channel/runtime",
                    format!("send fallback failed: channel={outbound_channel}, err={send_err}"),
                )
                .await;
            }
        }
    }
    approval_task.abort();
}

#[cfg(test)]
mod tests {
    use savfox_protocol::protocol::TokenUsage;

    use super::format_model_footer;
    use crate::channels::policy::append_channel_tone_suffix;
    use crate::config::ResponseFooterConfig;

    #[test]
    fn footer_respects_global_disable() {
        let cfg = ResponseFooterConfig {
            enabled: false,
            ..ResponseFooterConfig::default()
        };
        let footer = format_model_footer(&cfg, "discord", "openai/gpt-4o", "openai", None, None);
        assert!(footer.is_none());
    }

    #[test]
    fn footer_uses_channel_template_and_max_length() {
        let mut cfg = ResponseFooterConfig::default();
        cfg.channel_templates
            .insert("telegram".to_owned(), "m:{model} t:{tokens}".to_owned());
        cfg.channel_max_length.insert("telegram".to_owned(), 14);

        let usage = TokenUsage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 12345,
        };

        let footer = format_model_footer(
            &cfg,
            "telegram",
            "openai/gpt-4o-mini",
            "openai",
            None,
            Some(&usage),
        )
        .expect("footer should render");

        assert!(footer.chars().count() <= 14);
    }

    #[test]
    fn append_channel_tone_suffix_is_noop_when_missing() {
        let prompt = "summarize this";
        assert_eq!(append_channel_tone_suffix(prompt, None), prompt);
    }

    #[test]
    fn append_channel_tone_suffix_injects_instruction() {
        let prompt = "summarize this";
        let out = append_channel_tone_suffix(prompt, Some("Be more formal on Slack."));
        assert!(out.contains(prompt));
        assert!(out.contains("[system tone]"));
        assert!(out.contains("Be more formal on Slack."));
    }
}
