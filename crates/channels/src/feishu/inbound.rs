use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use feishu_sdk::core::{Error as FeishuSdkError, LogLevel};
use feishu_sdk::event::models::{MessageEvent as FeishuMessageEvent, MessageId};
use feishu_sdk::event::{
    Event as FeishuEvent, EventDispatcher, EventDispatcherConfig,
    EventHandler as FeishuEventHandler, EventResp as FeishuEventResp,
};
use feishu_sdk::ws::{StreamClient, StreamConfig};
use savfox_core::channel::ChannelAction;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::config::{FeishuChannelConfig, build_feishu_sdk_config, default_stream_locale};
use super::parse::extract_channel_action;

#[derive(Debug, Clone, Default)]
pub struct FeishuMessageMeta {
    pub sender_id: Option<String>,
    pub sender_name: Option<String>,
    pub chat_id: Option<String>,
    pub chat_type: Option<String>,
    pub thread_id: Option<String>,
    pub parent_message_id: Option<String>,
}

#[async_trait]
pub trait FeishuActionSink: Send + Sync {
    async fn handle_action(
        &self,
        action: ChannelAction,
        event_id: Option<&str>,
        message_id: Option<&str>,
        meta: FeishuMessageMeta,
    );
}

#[derive(Debug)]
struct StreamTaskEntry {
    generation: u64,
    handle: JoinHandle<()>,
}

fn stream_handles() -> &'static Mutex<std::collections::HashMap<String, StreamTaskEntry>> {
    static HANDLES: OnceLock<Mutex<std::collections::HashMap<String, StreamTaskEntry>>> =
        OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn next_stream_generation() -> u64 {
    static GENERATION: AtomicU64 = AtomicU64::new(1);
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

fn extract_sender_id(sender_id: Option<&MessageId>) -> Option<String> {
    let sender_id = sender_id?;
    sender_id
        .user_id
        .as_deref()
        .or(sender_id.open_id.as_deref())
        .or(sender_id.union_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

struct SavfoxFeishuEventHandler {
    sink: Arc<dyn FeishuActionSink>,
}

impl FeishuEventHandler for SavfoxFeishuEventHandler {
    fn event_type(&self) -> &str {
        "im.message.receive_v1"
    }

    fn handle(
        &self,
        event: FeishuEvent,
    ) -> Pin<Box<dyn Future<Output = Result<Option<FeishuEventResp>, FeishuSdkError>> + Send + '_>>
    {
        Box::pin(async move {
            let event_id = event.event_id().map(str::to_owned);
            debug!(event_id = ?event_id, event = ?event, "Received Feishu event via WebSocket");
            let payload = event.event.ok_or_else(|| {
                FeishuSdkError::InvalidEventFormat("missing event payload".to_owned())
            })?;
            let message_event: FeishuMessageEvent = serde_json::from_value(payload)
                .map_err(|e| FeishuSdkError::InvalidEventFormat(e.to_string()))?;
            debug!(message_event = ?message_event, "Parsed Feishu message event from WebSocket");

            let meta = FeishuMessageMeta {
                sender_id: extract_sender_id(message_event.sender.sender_id.as_ref()),
                sender_name: None,
                chat_id: message_event.message.chat_id.clone(),
                chat_type: None,
                thread_id: message_event.message.root_id.clone(),
                parent_message_id: message_event.message.parent_id.clone(),
            };

            if let Some(action) = extract_channel_action(&message_event) {
                debug!(action = ?action, meta = ?meta, "Extracted channel action from Feishu message");
                self.sink
                    .handle_action(
                        action,
                        event_id.as_deref(),
                        message_event.message.message_id.as_deref(),
                        meta,
                    )
                    .await;
            }

            Ok(None)
        })
    }
}

pub async fn build_feishu_event_dispatcher(
    config: &FeishuChannelConfig,
    sink: Arc<dyn FeishuActionSink>,
) -> Arc<EventDispatcher> {
    let mut event_config = EventDispatcherConfig::new();
    if let Some(token) = config.verification_token.as_deref() {
        event_config = event_config.verification_token(token.to_owned());
    }
    if let Some(key) = config.encrypt_key.as_deref() {
        event_config = event_config.encrypt_key(key.to_owned());
    }

    let dispatcher = Arc::new(EventDispatcher::new(
        event_config,
        feishu_sdk::core::new_logger(LogLevel::Info),
    ));
    dispatcher
        .register_handler(Box::new(SavfoxFeishuEventHandler { sink }))
        .await;
    dispatcher
}

pub async fn start_feishu_stream(
    channel_id: &str,
    config: &FeishuChannelConfig,
    sink: Arc<dyn FeishuActionSink>,
) -> anyhow::Result<()> {
    if !config.stream_enabled() {
        return Ok(());
    }

    let dispatcher = build_feishu_event_dispatcher(config, sink).await;
    let stream_config = StreamConfig::new()
        .locale(
            config
                .stream_locale
                .clone()
                .unwrap_or_else(|| default_stream_locale(&config.kind).to_owned()),
        )
        .auto_reconnect(config.stream_auto_reconnect)
        .reconnect_count(config.stream_reconnect_count)
        .reconnect_interval(Duration::from_secs(config.stream_reconnect_interval_secs))
        .ping_interval(Duration::from_secs(config.stream_ping_interval_secs));

    let stream_client = StreamClient::builder(build_feishu_sdk_config(config)?)
        .event_dispatcher_ref(dispatcher)
        .stream_config(stream_config)
        .build()
        .map_err(|err| anyhow::anyhow!("failed to build Feishu stream client: {err}"))?;

    let stream_key = channel_id.to_owned();
    let generation = next_stream_generation();
    let task_channel_id = stream_key.clone();
    let tracked_channel_id = stream_key.clone();
    let handle = tokio::spawn(async move {
        info!(channel_id = %task_channel_id, "Feishu/Lark stream channel starting");
        if let Err(err) = stream_client.start().await {
            warn!(channel_id = %task_channel_id, error = %err, "Feishu/Lark stream channel stopped");
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

pub async fn stop_feishu_stream(channel_id: &str) -> bool {
    let mut handles = stream_handles().lock().await;
    if let Some(entry) = handles.remove(channel_id) {
        entry.handle.abort();
        true
    } else {
        false
    }
}

pub async fn is_feishu_stream_running(channel_id: &str) -> bool {
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
