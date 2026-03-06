use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{error, info, warn};

use super::{Channel, RichMessage, runtime};
use crate::bridge::GatewayChannel;
use crate::config::SignalChannelConfig;
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

/// Signal bridge using signal-cli JSON-RPC interface.
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
        info!("Signal bridge initialized (JSON-RPC mode)");
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

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(to = %channel, "Signal JSON-RPC error: HTTP {status}: {resp_body}");
            anyhow::bail!("Signal JSON-RPC error: HTTP {status}");
        }

        info!(to = %channel, "Signal message sent");
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let mut text = msg.text.clone();
        for block in &msg.code_blocks {
            text.push_str(&format!("\n```\n{}\n```", block.content));
        }
        self.send_message(channel, &text).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        Self::parse_message(&payload)
    }
}

fn render_error(res: &mut Response, status: StatusCode, code: &str, message: impl Into<String>) {
    res.status_code(status);
    res.render(Text::Json(
        json!({
            "error": {
                "code": code,
                "message": message.into(),
            }
        })
        .to_string(),
    ));
}

/// `POST /webhooks/signal`: Handle Signal webhook events (from signal-cli --webhook mode).
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let body = match req.parse_json::<Value>().await {
        Ok(v) => v,
        Err(err) => {
            warn!("Signal webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                "failed to parse JSON",
            );
            return;
        }
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
        ChannelAction::StartThread { channel, prompt } => {
            info!(from = %channel, "Signal: starting thread with prompt");

            let timestamp = body
                .get("envelope")
                .and_then(|e| e.get("timestamp"))
                .and_then(|t| t.as_str())
                .map(|t| format!("signal:{t}"));

            if runtime::should_drop_duplicate(timestamp).await {
                res.status_code(StatusCode::OK);
                return;
            }

            let bridge = match depot.obtain::<Arc<GatewayChannel>>() {
                Ok(bridge) => bridge.clone(),
                Err(err) => {
                    warn!("Signal webhook: missing gateway bridge state: {err:?}");
                    render_error(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "state_unavailable",
                        "gateway bridge unavailable",
                    );
                    return;
                }
            };
            let session_store = match depot.obtain::<Arc<SessionStore>>() {
                Ok(store) => store.clone(),
                Err(err) => {
                    warn!("Signal webhook: missing session store: {err:?}");
                    render_error(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "state_unavailable",
                        "session store unavailable",
                    );
                    return;
                }
            };

            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline(
                    bridge,
                    session_store,
                    "signal",
                    channel,
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
