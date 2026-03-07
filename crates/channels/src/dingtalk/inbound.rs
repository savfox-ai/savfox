use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dingtalk_sdk::stream::{
    DataFrameResponse, DingTalkStreamClient, EVENT_HEADER_ID, TOPIC_BOT_MESSAGE_CALLBACK,
};
use savfox_core::channel::ChannelAction;
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
        DingTalkStreamClient::new(client_id.to_string(), client_secret.to_string())
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
            let event_id = frame.header(EVENT_HEADER_ID).map(str::to_string);
            let message_id = frame.message_id().map(str::to_string);
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

    let channel_id = channel_id.to_string();
    tokio::spawn(async move {
        info!(channel_id = %channel_id, "DingTalk stream channel starting");
        if let Err(err) = stream_client.start().await {
            warn!(channel_id = %channel_id, error = %err, "DingTalk stream channel stopped");
        }
    });
    Ok(())
}
