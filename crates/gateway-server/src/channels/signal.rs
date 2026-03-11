use async_trait::async_trait;
use salvo::prelude::*;
use savfox_channels::http::check_response;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{Channel, render_error, runtime};
use crate::config::SignalChannelConfig;
use crate::protocol::ChannelAction;

/// Signal channel using signal-cli JSON-RPC interface.
pub(crate) struct SignalChannel {
    config: SignalChannelConfig,
    http_client: reqwest::Client,
}

impl SignalChannel {
    #[must_use]
    pub(crate) fn new(config: SignalChannelConfig, http_client: reqwest::Client) -> Self {
        Self {
            config,
            http_client,
        }
    }

    /// Parse a Signal message into a `ChannelAction`.
    fn parse_message(payload: &Value) -> anyhow::Result<ChannelAction> {
        let envelope = payload.get("envelope").unwrap_or(&Value::Null);

        let source = envelope
            .get("source")
            .and_then(|s| s.as_str())
            .unwrap_or_default();

        let data_message = envelope.get("dataMessage").unwrap_or(&Value::Null);
        let text = data_message
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("");

        if !text.is_empty() {
            return Ok(ChannelAction::StartThread {
                channel: source.to_string(),
                prompt: text.to_owned(),
            });
        }

        Ok(ChannelAction::Ignore)
    }
}

#[async_trait]
impl Channel for SignalChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Signal channel initialized (JSON-RPC mode)");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let rpc_url = self
            .config
            .rpc_url
            .as_deref()
            .unwrap_or("http://127.0.0.1:8080/api/v1/rpc");

        let body = json!({
            "jsonrpc": "2.0",
            "method": "send",
            "params": {
                "recipient": [channel],
                "message": message,
            },
            "id": uuid::Uuid::now_v7().to_string(),
        });

        let response = self
            .http_client
            .post(rpc_url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        check_response(response, "Signal JSON-RPC error").await?;

        info!(to = %channel, "Signal message sent");
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        Self::parse_message(&payload)
    }
}

/// `POST /webhooks/signal`: Handle Signal webhook events (from signal-cli --webhook mode).
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(body) = super::parse_json_body(req, res, "signal").await else {
        return;
    };

    let action = match SignalChannel::parse_message(&body) {
        Ok(action) => action,
        Err(err) => {
            warn!("Signal webhook: failed to parse message: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse: {err}"),
            );
            return;
        }
    };

    match action {
        ChannelAction::StartThread {
            channel: channel_id,
            prompt,
        } => {
            info!(from = %channel_id, "Signal: starting thread with prompt");

            let timestamp = body
                .get("envelope")
                .and_then(|e| e.get("timestamp"))
                .and_then(|t| t.as_str())
                .map(|t| format!("signal:{t}"));

            if runtime::should_drop_duplicate(timestamp).await {
                res.status_code(StatusCode::OK);
                return;
            }

            let Some((gateway_channel, session_store)) =
                super::obtain_channel_and_store(depot, res)
            else {
                return;
            };

            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline(
                    gateway_channel,
                    session_store,
                    "signal",
                    channel_id,
                    prompt,
                    None,
                )
                .await;
            });
        }
        ChannelAction::Approve {
            thread_id,
            decision,
        } => {
            info!(thread_id = %thread_id, decision = %decision, "Signal: approval response");
        }
        ChannelAction::Ignore | ChannelAction::SendToThread { .. } => {}
    }

    res.status_code(StatusCode::OK);
}
