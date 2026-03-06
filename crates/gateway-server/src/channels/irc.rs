use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{Channel, RichMessage, runtime};
use crate::bridge::GatewayBridge;
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

/// IRC bridge via an HTTP relay service.
/// IRC doesn't have a native HTTP API, so this communicates with a local bridge
/// service (e.g., IRC-to-HTTP relay) that exposes REST endpoints.
pub(crate) struct IrcChannel {
    bridge_url: String,
    http_client: reqwest::Client,
}

impl IrcChannel {
    #[must_use]
    pub(crate) fn new(bridge_url: String, http_client: reqwest::Client) -> Self {
        Self {
            bridge_url,
            http_client,
        }
    }
}

fn render_error(res: &mut Response, status: StatusCode, code: &str, message: impl Into<String>) {
    res.status_code(status);
    res.render(Json(json!({
        "error": {
            "code": code,
            "message": message.into(),
        }
    })));
}

#[async_trait]
impl Channel for IrcChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!(bridge_url = %self.bridge_url, "IRC bridge starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = format!("{}/send", self.bridge_url);
        let body = json!({ "channel": channel, "message": message });
        let response = self
            .http_client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "IRC bridge send error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        // IRC doesn't support rich formatting well; send as plain text.
        self.send_message(channel, &msg.text).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        let message = payload
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("");
        let channel_name = payload
            .get("channel")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_owned();
        let _nick = payload
            .get("nick")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_owned();

        if let Some(prompt) = message.strip_prefix("!savfox ") {
            let prompt = prompt.trim().to_owned();
            if !prompt.is_empty() {
                return Ok(ChannelAction::StartThread {
                    channel: channel_name,
                    prompt,
                });
            }
        }
        Ok(ChannelAction::Ignore)
    }
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let body = match req.parse_json::<Value>().await {
        Ok(body) => body,
        Err(err) => {
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("invalid JSON: {err}"),
            );
            return;
        }
    };

    let message = body.get("message").and_then(|m| m.as_str()).unwrap_or("");
    let channel = body
        .get("channel")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_owned();
    let prompt = message
        .strip_prefix("!savfox ")
        .map(str::trim)
        .unwrap_or("")
        .to_owned();
    let dedupe_key = body
        .get("message_id")
        .or_else(|| body.get("id"))
        .and_then(|v| v.as_str())
        .map(|id| format!("irc:{id}"))
        .or_else(|| {
            if channel.is_empty() || message.is_empty() {
                None
            } else {
                Some(format!(
                    "irc:{}:{}:{}",
                    channel,
                    body.get("nick").and_then(|v| v.as_str()).unwrap_or(""),
                    message
                ))
            }
        });

    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if !channel.is_empty() && !prompt.is_empty() {
        let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
            Ok(bridge) => bridge.clone(),
            Err(_) => {
                render_error(
                    res,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "state_unavailable",
                    "gateway bridge state unavailable",
                );
                return;
            }
        };
        let session_store = match depot.obtain::<Arc<SessionStore>>() {
            Ok(store) => store.clone(),
            Err(_) => {
                render_error(
                    res,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "state_unavailable",
                    "session store state unavailable",
                );
                return;
            }
        };
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline(
                bridge,
                session_store,
                "irc",
                channel,
                prompt,
                None,
            )
            .await;
        });
    }

    info!("IRC webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({ "status": "ok" })));
}
