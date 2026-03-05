use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{Channel, RichMessage, runtime};
use crate::bridge::GatewayBridge;
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

/// Google Chat bridge using webhook and Spaces API.
pub(crate) struct GoogleChatChannel {
    webhook_url: Option<String>,
    http_client: reqwest::Client,
}

impl GoogleChatChannel {
    #[must_use]
    pub(crate) fn new(webhook_url: Option<String>, http_client: reqwest::Client) -> Self {
        Self {
            webhook_url,
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
impl Channel for GoogleChatChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Google Chat bridge starting");
        Ok(())
    }

    async fn send_message(&self, _channel: &str, message: &str) -> anyhow::Result<()> {
        let url = match &self.webhook_url {
            Some(url) => url.clone(),
            None => return Err(anyhow::anyhow!("Google Chat webhook URL not configured")),
        };
        let body = json!({ "text": message });
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
                "Google Chat send error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let mut text = msg.text.clone();
        for block in &msg.code_blocks {
            text.push_str(&format!("\n```\n{}\n```", block.content));
        }
        self.send_message(channel, &text).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction> {
        let message = payload.get("message").unwrap_or(&Value::Null);
        let text = message.get("text").and_then(|t| t.as_str()).unwrap_or("");
        let space = payload
            .get("space")
            .and_then(|s| s.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_owned();
        let _sender = message
            .get("sender")
            .and_then(|s| s.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_owned();

        if let Some(prompt) = text
            .strip_prefix("/savfox ")
            .or_else(|| text.strip_prefix("@savfox "))
        {
            let prompt = prompt.trim().to_owned();
            if !prompt.is_empty() {
                return Ok(BridgeAction::StartThread {
                    channel: space,
                    prompt,
                });
            }
        }
        Ok(BridgeAction::Ignore)
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

    let text = body
        .get("message")
        .and_then(|m| m.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let channel = body
        .get("space")
        .and_then(|s| s.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let prompt = text
        .strip_prefix("/savfox ")
        .or_else(|| text.strip_prefix("@savfox "))
        .map(str::trim)
        .unwrap_or("")
        .to_owned();

    let dedupe_key = body
        .get("message")
        .and_then(|m| m.get("name"))
        .and_then(|v| v.as_str())
        .map(|id| format!("googlechat:{id}"));
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
                "googlechat",
                channel,
                prompt,
                None,
            )
            .await;
        });
    }

    info!("Google Chat webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
