use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::Mutex;
use tracing::warn;

use super::delivery::send_with_retry;
use super::footer::{append_footer, current_response_footer_config, format_model_footer};
use super::memory::maybe_auto_memory_flush;
use super::trigger::{
    AgentTriggerConfig, ConversationKind, SenderKind, TriggerContext, TriggerDecision,
    TriggerReason,
};
use crate::channel::GatewayChannel;
use crate::channels::policy::{append_channel_tone_suffix, configured_channel_tone_suffix};
use crate::log_store;
use crate::session::{
    SessionStore, format_ambient_context, prepend_ambient_context, session_file_to_store_value,
    take_ambient_messages, track_token_usage,
};

const ONE_HOUR_MS: u64 = 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct IdleReplyPendingStatus {
    pub session_id: String,
    pub agent_id: String,
    pub outbound_channel: String,
    pub delay_secs: u64,
    pub scheduled_at_ms: u64,
    pub deadline_at_ms: u64,
    pub message_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct IdleReplySessionStatus {
    pub session_id: String,
    pub generation: u64,
    pub pending: Option<IdleReplyPendingStatus>,
    pub recent_sent_count: usize,
    pub recent_sent_at_ms: Vec<u64>,
    pub last_suppressed_at_ms: Option<u64>,
    pub suppressed_reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct IdleReplyState {
    generation: u64,
    pending: Option<IdleReplyPendingStatus>,
    recent_sent_at_ms: Vec<u64>,
    last_suppressed_at_ms: Option<u64>,
    suppressed_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct IdleReplySchedule {
    pub session_id: String,
    pub outbound_channel: String,
    pub platform: String,
    pub agent_id: String,
    pub thread_id: Option<String>,
    pub reply_target: Option<String>,
    pub prompt_override: Option<String>,
    pub delay_secs: u64,
    pub max_per_hour: u32,
    pub message_preview: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IdleReplyScheduleOutcome {
    Scheduled,
    RateLimited,
}

fn state_store() -> &'static Mutex<HashMap<String, IdleReplyState>> {
    static STORE: OnceLock<Mutex<HashMap<String, IdleReplyState>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn trim_preview(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let compact = compact.trim();
    let mut preview = String::new();
    for ch in compact.chars().take(96) {
        preview.push(ch);
    }
    if compact.chars().count() > 96 {
        preview.push_str("...");
    }
    preview
}

fn prune_recent_sent(state: &mut IdleReplyState, now_ms: u64) {
    state
        .recent_sent_at_ms
        .retain(|timestamp| now_ms.saturating_sub(*timestamp) <= ONE_HOUR_MS);
}

pub(crate) async fn get_idle_reply_status(session_id: &str) -> IdleReplySessionStatus {
    let mut store = state_store().lock().await;
    let now_ms = crate::json_store::now_ms();
    let state = store.entry(session_id.to_owned()).or_default();
    prune_recent_sent(state, now_ms);
    IdleReplySessionStatus {
        session_id: session_id.to_owned(),
        generation: state.generation,
        pending: state.pending.clone(),
        recent_sent_count: state.recent_sent_at_ms.len(),
        recent_sent_at_ms: state.recent_sent_at_ms.clone(),
        last_suppressed_at_ms: state.last_suppressed_at_ms,
        suppressed_reason: state.suppressed_reason.clone(),
    }
}

pub(super) async fn record_inbound_activity(session_id: &str) -> u64 {
    let mut store = state_store().lock().await;
    let state = store.entry(session_id.to_owned()).or_default();
    state.generation = state.generation.saturating_add(1);
    state.pending = None;
    state.generation
}

pub(super) fn should_schedule_idle_reply(
    decision: &TriggerDecision,
    context: TriggerContext<'_>,
    config: &AgentTriggerConfig,
) -> bool {
    if !config.idle_reply.enabled {
        return false;
    }

    if !matches!(decision, TriggerDecision::IngestOnly { .. }) {
        return false;
    }

    if !matches!(
        context.conversation_kind,
        ConversationKind::Group | ConversationKind::Broadcast | ConversationKind::Unknown
    ) {
        return false;
    }

    if context.sender_kind != SenderKind::Human
        || context.is_mentioned
        || context.is_command
        || context.reply_to_self
        || context.explicitly_targets_other_agent
        || context.text.trim().is_empty()
    {
        return false;
    }

    !matches!(
        decision,
        TriggerDecision::IngestOnly {
            reason: TriggerReason::MentionedOtherAgent
                | TriggerReason::ExternalBotIngestOnly
                | TriggerReason::ExternalBotIgnored
                | TriggerReason::SelfMessageIgnored,
        }
    )
}

pub(super) async fn schedule_idle_reply(
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    generation: u64,
    schedule: IdleReplySchedule,
) -> IdleReplyScheduleOutcome {
    let now_ms = crate::json_store::now_ms();
    {
        let mut store = state_store().lock().await;
        let state = store.entry(schedule.session_id.clone()).or_default();
        prune_recent_sent(state, now_ms);
        let max_per_hour = schedule.max_per_hour.max(1) as usize;
        if state.recent_sent_at_ms.len() >= max_per_hour {
            state.pending = None;
            state.last_suppressed_at_ms = Some(now_ms);
            state.suppressed_reason = Some("rate_limited".to_owned());
            return IdleReplyScheduleOutcome::RateLimited;
        }

        state.pending = Some(IdleReplyPendingStatus {
            session_id: schedule.session_id.clone(),
            agent_id: schedule.agent_id.clone(),
            outbound_channel: schedule.outbound_channel.clone(),
            delay_secs: schedule.delay_secs,
            scheduled_at_ms: now_ms,
            deadline_at_ms: now_ms.saturating_add(schedule.delay_secs.saturating_mul(1000)),
            message_preview: trim_preview(&schedule.message_preview),
        });
        state.last_suppressed_at_ms = None;
        state.suppressed_reason = None;
    }

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(schedule.delay_secs)).await;

        let current_generation = {
            let store = state_store().lock().await;
            store
                .get(&schedule.session_id)
                .map(|state| state.generation)
                .unwrap_or_default()
        };
        if current_generation != generation {
            return;
        }

        if let Err(err) = fire_idle_reply(gateway_channel, session_store, schedule).await {
            warn!("idle reply trigger failed: {err}");
        }
    });

    IdleReplyScheduleOutcome::Scheduled
}

fn idle_reply_prompt(delay_secs: u64, prompt_override: Option<&str>) -> String {
    if let Some(prompt) = prompt_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return prompt.to_owned();
    }

    format!(
        "[idle room fallback]\nThe room has been quiet for {delay_secs} seconds since the last buffered human message. Use the ambient context above and send a brief, helpful reply to address the pending last message."
    )
}

async fn fire_idle_reply(
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    schedule: IdleReplySchedule,
) -> anyhow::Result<()> {
    log_store::append_log(
        "info",
        "channel/runtime",
        format!(
            "idle reply fired: channel={}, session_id={}, agent={}",
            schedule.outbound_channel, schedule.session_id, schedule.agent_id
        ),
    )
    .await;

    let Some(entry) = session_store.get(&schedule.session_id).await else {
        return Ok(());
    };

    let tone_suffix = configured_channel_tone_suffix(
        &gateway_channel.config().savfox_home,
        &schedule.agent_id,
        &schedule.outbound_channel,
    )
    .await;
    let prompt = idle_reply_prompt(schedule.delay_secs, schedule.prompt_override.as_deref());
    let prompt = append_channel_tone_suffix(&prompt, tone_suffix.as_deref());
    let ambient_context =
        format_ambient_context(&take_ambient_messages(&schedule.session_id).await);
    let prompt = prepend_ambient_context(&prompt, ambient_context.as_deref());

    let result = gateway_channel
        .invoke_agent_text_in_session_with_metadata(
            &prompt,
            &schedule.agent_id,
            Some(&schedule.session_id),
        )
        .await?;

    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    if let Some(tokens) = result.last_token_usage.as_ref() {
        input_tokens = tokens.input_tokens.max(0) as u64;
        output_tokens = tokens.output_tokens.max(0) as u64;
        track_token_usage(
            &session_store,
            &schedule.session_id,
            input_tokens,
            output_tokens,
        )
        .await;
    }

    maybe_auto_memory_flush(
        &session_store,
        &gateway_channel.config().savfox_home,
        &schedule.session_id,
        entry.total_tokens,
        input_tokens,
        output_tokens,
        &prompt,
        &result.reply,
    )
    .await;

    let footer_config = current_response_footer_config();
    let footer_text = format_model_footer(
        &footer_config,
        &schedule.platform,
        &schedule.agent_id,
        schedule.agent_id.split('/').next().unwrap_or("unknown"),
        None,
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
        let session_id = schedule.session_id.clone();
        let _ = session_store
            .update(&session_id, move |session| {
                session.session_file = Some(session_file.clone());
                if session.thread_id.is_none() {
                    session.thread_id = Some(thread_id);
                }
            })
            .await;
    }

    if reply.trim().is_empty() {
        clear_pending(&schedule.session_id).await;
        return Ok(());
    }

    let send_thread_id = entry.thread_id.as_deref().or(schedule.thread_id.as_deref());
    let send_reply_target = entry
        .reply_target
        .as_deref()
        .or(schedule.reply_target.as_deref());
    send_with_retry(
        &gateway_channel,
        &schedule.outbound_channel,
        &reply,
        None,
        send_thread_id,
        send_reply_target,
    )
    .await?;

    mark_idle_reply_sent(&schedule.session_id).await;

    log_store::append_log(
        "info",
        "channel/runtime",
        format!(
            "idle reply sent: channel={}, session_id={}, bytes={}",
            schedule.outbound_channel,
            schedule.session_id,
            reply.len()
        ),
    )
    .await;

    Ok(())
}

async fn clear_pending(session_id: &str) {
    let mut store = state_store().lock().await;
    if let Some(state) = store.get_mut(session_id) {
        state.pending = None;
    }
}

async fn mark_idle_reply_sent(session_id: &str) {
    let mut store = state_store().lock().await;
    let now_ms = crate::json_store::now_ms();
    let state = store.entry(session_id.to_owned()).or_default();
    state.pending = None;
    state.last_suppressed_at_ms = None;
    state.suppressed_reason = None;
    state.recent_sent_at_ms.push(now_ms);
    prune_recent_sent(state, now_ms);
}

#[cfg(test)]
mod tests {
    use super::{
        IdleReplyScheduleOutcome, IdleReplyState, ONE_HOUR_MS, idle_reply_prompt, prune_recent_sent,
        should_schedule_idle_reply,
    };
    use crate::auto_reply::GroupActivation;
    use crate::channels::runtime::trigger::{
        AgentTriggerConfig, ConversationKind, IdleReplyConfig, SenderKind, TriggerContext,
        TriggerDecision, TriggerReason,
    };

    #[test]
    fn idle_reply_requires_group_human_ingest_only() {
        let config = AgentTriggerConfig {
            group_activation: GroupActivation::Mention,
            idle_reply: IdleReplyConfig {
                enabled: true,
                delay_secs: 180,
                max_per_hour: 1,
                prompt: None,
            },
            ..AgentTriggerConfig::default()
        };
        let base_context = TriggerContext {
            sender_kind: SenderKind::Human,
            conversation_kind: ConversationKind::Group,
            is_mentioned: false,
            reply_to_self: false,
            is_command: false,
            explicitly_targets_other_agent: false,
            text: "can someone answer this?",
        };
        assert!(should_schedule_idle_reply(
            &TriggerDecision::IngestOnly {
                reason: TriggerReason::RoomDefaultAgent,
            },
            base_context,
            &config,
        ));
        assert!(!should_schedule_idle_reply(
            &TriggerDecision::IngestOnly {
                reason: TriggerReason::MentionedOtherAgent,
            },
            TriggerContext {
                text: "reviewer: please look",
                explicitly_targets_other_agent: true,
                ..base_context
            },
            &config,
        ));
    }

    #[test]
    fn idle_prompt_mentions_delay() {
        let prompt = idle_reply_prompt(120, None);
        assert!(prompt.contains("120 seconds"));
        assert!(prompt.contains("idle room fallback"));
    }

    #[test]
    fn prune_recent_sent_keeps_one_hour_window() {
        let mut state = IdleReplyState {
            recent_sent_at_ms: vec![1_000, ONE_HOUR_MS + 2_000],
            ..IdleReplyState::default()
        };
        prune_recent_sent(&mut state, ONE_HOUR_MS + 2_001);
        assert_eq!(state.recent_sent_at_ms, vec![ONE_HOUR_MS + 2_000]);
    }

    #[test]
    fn schedule_outcome_exposes_rate_limit_variant() {
        assert_eq!(
            IdleReplyScheduleOutcome::RateLimited,
            IdleReplyScheduleOutcome::RateLimited
        );
    }
}
