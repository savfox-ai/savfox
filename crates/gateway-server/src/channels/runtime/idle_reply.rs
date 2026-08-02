use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::warn;

use super::delivery::send_with_retry;
use super::footer::{append_footer, current_response_footer_config, format_model_footer};
use super::memory::maybe_auto_memory_flush;
use super::routing::{ChannelReplyRouteContext, load_agent_trigger_config};
use super::trigger::{
    AgentTriggerConfig, ConversationKind, SenderKind, TriggerContext, TriggerDecision,
    TriggerReason,
};
use crate::channel::GatewayChannel;
use crate::channels::policy::{append_channel_tone_suffix, configured_channel_tone_suffix};
use crate::home_paths::idle_reply_state_path;
use crate::session::{
    SessionStore, clear_ambient_messages, format_ambient_context, peek_ambient_messages,
    prepend_ambient_context, session_file_to_store_value, track_token_usage,
};
use crate::{json_store, log_store};

const ONE_HOUR_MS: u64 = 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IdleReplyPendingStatus {
    pub session_id: String,
    pub agent_id: String,
    pub outbound_channel: String,
    #[serde(default)]
    pub saved_channel_config_id: String,
    pub delay_secs: u64,
    pub scheduled_at_ms: u64,
    pub deadline_at_ms: u64,
    pub message_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IdleReplySessionStatus {
    pub session_id: String,
    pub generation: u64,
    pub pending: Option<IdleReplyPendingStatus>,
    pub recent_sent_count: usize,
    pub recent_sent_at_ms: Vec<u64>,
    pub last_suppressed_at_ms: Option<u64>,
    pub suppressed_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    pub saved_channel_config_id: Option<String>,
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

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedIdleReplyState {
    sessions: HashMap<String, IdleReplyState>,
}

#[derive(Debug, Default)]
struct IdleReplyRuntimeStore {
    loaded_from: Option<PathBuf>,
    sessions: HashMap<String, IdleReplyState>,
}

fn state_store() -> &'static Mutex<IdleReplyRuntimeStore> {
    static STORE: OnceLock<Mutex<IdleReplyRuntimeStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(IdleReplyRuntimeStore::default()))
}

async fn ensure_store_loaded(savfox_home: &Path) {
    let path = idle_reply_state_path(savfox_home);
    let mut store = state_store().lock().await;
    if store.loaded_from.as_ref() == Some(&path) {
        return;
    }
    let persisted: PersistedIdleReplyState = json_store::load_json(&path, "idle reply state")
        .await
        .unwrap_or_default();
    store.loaded_from = Some(path);
    store.sessions = persisted.sessions;
}

async fn persist_snapshot(path: Option<PathBuf>, sessions: HashMap<String, IdleReplyState>) {
    let Some(path) = path else {
        return;
    };
    let data = PersistedIdleReplyState { sessions };
    if let Err(err) = json_store::save_json(&path, &data, "idle reply state").await {
        warn!("failed to persist idle reply state: {err}");
    }
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

fn remaining_delay_secs(now_ms: u64, deadline_at_ms: u64) -> u64 {
    if deadline_at_ms <= now_ms {
        0
    } else {
        (deadline_at_ms - now_ms).div_ceil(1000)
    }
}

pub(crate) async fn get_idle_reply_status(
    savfox_home: &Path,
    session_id: &str,
) -> IdleReplySessionStatus {
    ensure_store_loaded(savfox_home).await;
    let mut store = state_store().lock().await;
    let now_ms = crate::json_store::now_ms();
    let (
        generation,
        pending,
        recent_sent_count,
        recent_sent_at_ms,
        last_suppressed_at_ms,
        suppressed_reason,
    ) = {
        let state = store.sessions.entry(session_id.to_owned()).or_default();
        prune_recent_sent(state, now_ms);
        (
            state.generation,
            state.pending.clone(),
            state.recent_sent_at_ms.len(),
            state.recent_sent_at_ms.clone(),
            state.last_suppressed_at_ms,
            state.suppressed_reason.clone(),
        )
    };
    let path = store.loaded_from.clone();
    let snapshot = store.sessions.clone();
    drop(store);
    persist_snapshot(path, snapshot).await;
    IdleReplySessionStatus {
        session_id: session_id.to_owned(),
        generation,
        pending,
        recent_sent_count,
        recent_sent_at_ms,
        last_suppressed_at_ms,
        suppressed_reason,
    }
}

pub(super) async fn record_inbound_activity(savfox_home: &Path, session_id: &str) -> u64 {
    ensure_store_loaded(savfox_home).await;
    let mut store = state_store().lock().await;
    let generation = {
        let state = store.sessions.entry(session_id.to_owned()).or_default();
        state.generation = state.generation.saturating_add(1);
        state.pending = None;
        state.generation
    };
    let path = store.loaded_from.clone();
    let snapshot = store.sessions.clone();
    drop(store);
    persist_snapshot(path, snapshot).await;
    generation
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
    savfox_home: &Path,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    generation: u64,
    schedule: IdleReplySchedule,
) -> IdleReplyScheduleOutcome {
    ensure_store_loaded(savfox_home).await;
    let now_ms = crate::json_store::now_ms();
    {
        let mut store = state_store().lock().await;
        let path = store.loaded_from.clone();
        let state = store
            .sessions
            .entry(schedule.session_id.clone())
            .or_default();
        prune_recent_sent(state, now_ms);
        let max_per_hour = schedule.max_per_hour.max(1) as usize;
        if state.recent_sent_at_ms.len() >= max_per_hour {
            state.pending = None;
            state.last_suppressed_at_ms = Some(now_ms);
            state.suppressed_reason = Some("rate_limited".to_owned());
            let snapshot = store.sessions.clone();
            drop(store);
            persist_snapshot(path, snapshot).await;
            return IdleReplyScheduleOutcome::RateLimited;
        }

        state.pending = Some(IdleReplyPendingStatus {
            session_id: schedule.session_id.clone(),
            agent_id: schedule.agent_id.clone(),
            outbound_channel: schedule.outbound_channel.clone(),
            saved_channel_config_id: schedule.saved_channel_config_id.clone().unwrap_or_default(),
            delay_secs: schedule.delay_secs,
            scheduled_at_ms: now_ms,
            deadline_at_ms: now_ms.saturating_add(schedule.delay_secs.saturating_mul(1000)),
            message_preview: trim_preview(&schedule.message_preview),
        });
        state.last_suppressed_at_ms = None;
        state.suppressed_reason = None;
        let snapshot = store.sessions.clone();
        drop(store);
        persist_snapshot(path, snapshot).await;
    }

    spawn_idle_reply_task(
        savfox_home.to_path_buf(),
        gateway_channel,
        session_store,
        generation,
        schedule,
    );

    IdleReplyScheduleOutcome::Scheduled
}

fn spawn_idle_reply_task(
    state_home: PathBuf,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    generation: u64,
    schedule: IdleReplySchedule,
) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(schedule.delay_secs)).await;

        let current_generation = {
            ensure_store_loaded(&state_home).await;
            let store = state_store().lock().await;
            store
                .sessions
                .get(&schedule.session_id)
                .map(|state| state.generation)
                .unwrap_or_default()
        };
        if current_generation != generation {
            return;
        }

        if let Err(err) = fire_idle_reply(gateway_channel, session_store, schedule.clone()).await {
            warn!("idle reply trigger failed: {err}");
            mark_idle_reply_failed(&schedule.session_id, "trigger_failed").await;
        }
    });
}

pub(crate) async fn resume_pending_idle_replies(
    savfox_home: &Path,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) {
    ensure_store_loaded(savfox_home).await;
    let now_ms = crate::json_store::now_ms();
    let pending = {
        let store = state_store().lock().await;
        store
            .sessions
            .iter()
            .filter_map(|(session_id, state)| {
                state
                    .pending
                    .clone()
                    .map(|pending| (session_id.clone(), state.generation, pending))
            })
            .collect::<Vec<_>>()
    };

    for (session_id, generation, pending) in pending {
        let Some(entry) = session_store.get(&session_id).await else {
            remove_idle_reply_session(savfox_home, &session_id).await;
            continue;
        };

        let (platform, channel_id) = pending
            .outbound_channel
            .split_once(':')
            .map(|(platform, channel_id)| (Some(platform), Some(channel_id)))
            .unwrap_or((None, Some(pending.outbound_channel.as_str())));
        let config = load_agent_trigger_config(
            savfox_home,
            &pending.agent_id,
            ChannelReplyRouteContext {
                platform,
                channel_id,
                saved_channel_config_id: (!pending.saved_channel_config_id.is_empty())
                    .then_some(pending.saved_channel_config_id.as_str()),
            },
        )
        .await;
        if !config.idle_reply.enabled {
            clear_pending(&session_id).await;
            continue;
        }

        let schedule = IdleReplySchedule {
            session_id: pending.session_id.clone(),
            outbound_channel: pending.outbound_channel.clone(),
            platform: pending
                .outbound_channel
                .split(':')
                .next()
                .unwrap_or("unknown")
                .to_owned(),
            saved_channel_config_id: (!pending.saved_channel_config_id.is_empty())
                .then_some(pending.saved_channel_config_id.clone()),
            agent_id: pending.agent_id.clone(),
            thread_id: entry.remote_thread_id.clone(),
            reply_target: entry.reply_target.clone(),
            prompt_override: config.idle_reply.prompt.clone(),
            delay_secs: remaining_delay_secs(now_ms, pending.deadline_at_ms),
            max_per_hour: config.idle_reply.max_per_hour,
            message_preview: pending.message_preview.clone(),
        };

        spawn_idle_reply_task(
            savfox_home.to_path_buf(),
            Arc::clone(&gateway_channel),
            Arc::clone(&session_store),
            generation,
            schedule,
        );
    }
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
        remove_idle_reply_session(&gateway_channel.config().savfox_home, &schedule.session_id)
            .await;
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
    let ambient_context = format_ambient_context(
        &peek_ambient_messages(&gateway_channel.config().savfox_home, &schedule.session_id).await,
    );
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
                    session.remote_thread_id = Some(thread_id);
                }
            })
            .await;
    }

    if reply.trim().is_empty() {
        clear_pending(&schedule.session_id).await;
        return Ok(());
    }

    let send_thread_id = entry
        .remote_thread_id
        .as_deref()
        .or(schedule.thread_id.as_deref());
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
        schedule.saved_channel_config_id.as_deref(),
    )
    .await?;

    clear_ambient_messages(&gateway_channel.config().savfox_home, &schedule.session_id).await;
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
    if let Some(state) = store.sessions.get_mut(session_id) {
        state.pending = None;
    }
    let path = store.loaded_from.clone();
    let snapshot = store.sessions.clone();
    drop(store);
    persist_snapshot(path, snapshot).await;
}

async fn mark_idle_reply_failed(session_id: &str, reason: &str) {
    let mut store = state_store().lock().await;
    let now_ms = crate::json_store::now_ms();
    let state = store.sessions.entry(session_id.to_owned()).or_default();
    state.pending = None;
    state.last_suppressed_at_ms = Some(now_ms);
    state.suppressed_reason = Some(reason.to_owned());
    let path = store.loaded_from.clone();
    let snapshot = store.sessions.clone();
    drop(store);
    persist_snapshot(path, snapshot).await;
}

async fn mark_idle_reply_sent(session_id: &str) {
    let mut store = state_store().lock().await;
    let now_ms = crate::json_store::now_ms();
    let state = store.sessions.entry(session_id.to_owned()).or_default();
    state.pending = None;
    state.last_suppressed_at_ms = None;
    state.suppressed_reason = None;
    state.recent_sent_at_ms.push(now_ms);
    prune_recent_sent(state, now_ms);
    let path = store.loaded_from.clone();
    let snapshot = store.sessions.clone();
    drop(store);
    persist_snapshot(path, snapshot).await;
}

pub(crate) async fn remove_idle_reply_session(savfox_home: &Path, session_id: &str) {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return;
    }
    ensure_store_loaded(savfox_home).await;
    let mut store = state_store().lock().await;
    store.sessions.remove(session_id);
    let path = store.loaded_from.clone();
    let snapshot = store.sessions.clone();
    drop(store);
    persist_snapshot(path, snapshot).await;
}

#[cfg(test)]
mod tests {
    use super::{
        IdleReplyPendingStatus, IdleReplyScheduleOutcome, IdleReplyState, ONE_HOUR_MS,
        idle_reply_prompt, prune_recent_sent, remaining_delay_secs, should_schedule_idle_reply,
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
    fn remaining_delay_secs_rounds_up() {
        assert_eq!(remaining_delay_secs(1_000, 1_000), 0);
        assert_eq!(remaining_delay_secs(1_000, 1_001), 1);
        assert_eq!(remaining_delay_secs(1_000, 1_999), 1);
        assert_eq!(remaining_delay_secs(1_000, 2_001), 2);
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

    #[test]
    fn persisted_pending_reply_without_instance_id_remains_readable() {
        let pending = serde_json::from_value::<IdleReplyPendingStatus>(serde_json::json!({
            "session_id": "session-1",
            "agent_id": "default",
            "outbound_channel": "arkret:ak:realm:01904100-0000-7000-8000-000000000001",
            "delay_secs": 60,
            "scheduled_at_ms": 1,
            "deadline_at_ms": 61_000,
            "message_preview": "hello"
        }))
        .expect("deserialize legacy pending reply");

        assert!(pending.saved_channel_config_id.is_empty());
    }
}
