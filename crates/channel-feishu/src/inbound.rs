use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use feishu_sdk::core::{Error as FeishuSdkError, LogLevel};
use feishu_sdk::event::models::MessageEvent as FeishuMessageEvent;
use feishu_sdk::event::{
    Event as FeishuEvent, EventDispatcher, EventDispatcherConfig,
    EventHandler as FeishuEventHandler, EventResp as FeishuEventResp,
};
use feishu_sdk::ws::{StreamClient, StreamConfig};
use savfox_core::channel::ChannelAction;
use tracing::{info, warn};

use crate::config::{FeishuChannelConfig, build_feishu_sdk_config, default_stream_locale};
use crate::parse::extract_channel_action;

#[async_trait]
pub trait FeishuActionSink: Send + Sync {
    async fn handle_action(
        &self,
        action: ChannelAction,
        event_id: Option<&str>,
        message_id: Option<&str>,
    );
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
            let event_id = event.event_id().map(str::to_string);
            let payload = event.event.ok_or_else(|| {
                FeishuSdkError::InvalidEventFormat("missing event payload".to_string())
            })?;
            let message_event: FeishuMessageEvent = serde_json::from_value(payload)
                .map_err(|e| FeishuSdkError::InvalidEventFormat(e.to_string()))?;

            if let Some(action) = extract_channel_action(&message_event) {
                self.sink
                    .handle_action(
                        action,
                        event_id.as_deref(),
                        message_event.message.message_id.as_deref(),
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
        event_config = event_config.verification_token(token.to_string());
    }
    if let Some(key) = config.encrypt_key.as_deref() {
        event_config = event_config.encrypt_key(key.to_string());
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
                .unwrap_or_else(|| default_stream_locale(&config.kind).to_string()),
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

    let channel_id = channel_id.to_string();
    tokio::spawn(async move {
        info!(channel_id = %channel_id, "Feishu/Lark stream channel starting");
        if let Err(err) = stream_client.start().await {
            warn!(channel_id = %channel_id, error = %err, "Feishu/Lark stream channel stopped");
        }
    });
    Ok(())
}
