use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use dingtalk_sdk::stream::{
    DataFrameResponse, DingTalkStreamClient, EVENT_HEADER_ID, TOPIC_BOT_MESSAGE_CALLBACK,
};
use savfox_core::channel::ChannelAction;
use serde::Serialize;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::config::DingtalkChannelConfig;
use super::parse::parse_inbound_payload;

#[async_trait]
pub trait DingtalkActionSink: Send + Sync {
    async fn handle_action(
        &self,
        action: ChannelAction,
        event_id: Option<&str>,
        message_id: Option<&str>,
        meta: super::parse::DingtalkMessageMeta,
    );
}

#[derive(Debug)]
struct StreamTaskEntry {
    generation: u64,
    handle: JoinHandle<()>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DingtalkStreamState {
    pub running: bool,
    pub phase: String,
    pub attempt_count: u32,
    pub last_error: Option<String>,
    pub updated_at_ms: i64,
}

fn stream_handles() -> &'static Mutex<HashMap<String, StreamTaskEntry>> {
    static HANDLES: OnceLock<Mutex<HashMap<String, StreamTaskEntry>>> = OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn stream_states() -> &'static std::sync::RwLock<HashMap<String, DingtalkStreamState>> {
    static STATES: OnceLock<std::sync::RwLock<HashMap<String, DingtalkStreamState>>> =
        OnceLock::new();
    STATES.get_or_init(|| std::sync::RwLock::new(HashMap::new()))
}

fn next_stream_generation() -> u64 {
    static GENERATION: AtomicU64 = AtomicU64::new(1);
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

fn update_stream_state(channel_id: &str, running: bool, phase: &str, last_error: Option<String>) {
    let mut states = stream_states()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous_attempts = states
        .get(channel_id)
        .map(|state| state.attempt_count)
        .unwrap_or(0);
    states.insert(
        channel_id.to_owned(),
        DingtalkStreamState {
            running,
            phase: phase.to_owned(),
            attempt_count: if phase == "starting" {
                previous_attempts.saturating_add(1)
            } else {
                previous_attempts
            },
            last_error,
            updated_at_ms: chrono::Utc::now().timestamp_millis(),
        },
    );
}

pub async fn start_dingtalk_stream(
    channel_id: &str,
    config: &DingtalkChannelConfig,
    sink: Arc<dyn DingtalkActionSink>,
) -> anyhow::Result<()> {
    if !config.stream_enabled() {
        return Ok(());
    }

    let client_id = config
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("DingTalk stream mode requires client_id"))?;
    let client_secret = config
        .client_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("DingTalk stream mode requires client_secret"))?;

    let mut stream_client =
        DingTalkStreamClient::new(client_id.to_owned(), client_secret.to_owned())
            .map_err(|err| anyhow::anyhow!("failed to build DingTalk stream client: {err}"))?;
    {
        let stream_config = stream_client.config_mut();
        stream_config.openapi_host = config.openapi_host.clone();
        stream_config.auto_reconnect = config.stream_auto_reconnect;
        stream_config.reconnect_interval =
            Duration::from_secs(config.stream_reconnect_interval_secs.max(1));
        stream_config.keep_alive_idle =
            Duration::from_secs(config.stream_keep_alive_idle_secs.max(3));
    }

    stream_client.register_callback_handler(TOPIC_BOT_MESSAGE_CALLBACK, move |frame| {
        let sink = Arc::clone(&sink);
        async move {
            let event_id = frame.header(EVENT_HEADER_ID).map(str::to_owned);
            let message_id = frame.message_id().map(str::to_owned);
            let payload = serde_json::from_str::<serde_json::Value>(&frame.data)?;
            let parsed = parse_inbound_payload(&payload);
            if parsed.action != ChannelAction::Ignore {
                sink.handle_action(
                    parsed.action,
                    event_id.as_deref(),
                    message_id.as_deref(),
                    parsed.meta,
                )
                .await;
            }
            Ok(Some(DataFrameResponse::success()))
        }
    });

    let stream_key = channel_id.to_owned();
    let generation = next_stream_generation();
    let task_channel_id = stream_key.clone();
    let tracked_channel_id = stream_key.clone();
    update_stream_state(channel_id, true, "starting", None);
    let handle = tokio::spawn(async move {
        info!(channel_id = %task_channel_id, "DingTalk stream channel starting");
        update_stream_state(&task_channel_id, true, "listening", None);
        if let Err(err) = stream_client.start().await {
            let error = err.to_string();
            warn!(channel_id = %task_channel_id, error = %error, "DingTalk stream channel stopped");
            update_stream_state(&task_channel_id, false, "failed", Some(error));
        } else {
            update_stream_state(&task_channel_id, false, "stopped", None);
        }
        let mut handles = stream_handles().lock().await;
        if handles
            .get(&tracked_channel_id)
            .is_some_and(|entry| entry.generation == generation)
        {
            handles.remove(&tracked_channel_id);
        }
    });

    let mut handles = stream_handles().lock().await;
    if let Some(previous) = handles.insert(stream_key, StreamTaskEntry { generation, handle }) {
        previous.handle.abort();
    }
    Ok(())
}

pub async fn stop_dingtalk_stream(channel_id: &str) -> bool {
    let mut handles = stream_handles().lock().await;
    if let Some(entry) = handles.remove(channel_id) {
        entry.handle.abort();
        update_stream_state(channel_id, false, "stopped", None);
        true
    } else {
        false
    }
}

pub async fn is_dingtalk_stream_running(channel_id: &str) -> bool {
    let mut handles = stream_handles().lock().await;
    if handles
        .get(channel_id)
        .is_some_and(|entry| entry.handle.is_finished())
    {
        handles.remove(channel_id);
        return false;
    }
    handles.contains_key(channel_id)
}

#[must_use]
pub fn dingtalk_stream_state_snapshot() -> HashMap<String, DingtalkStreamState> {
    stream_states()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}
