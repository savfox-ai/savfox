use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::bridge::GatewayBridge;
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

use super::{ChatBridge, RichMessage, runtime};

/// Feishu/Lark bot bridge using the Feishu Open Platform API.
pub(crate) struct FeishuBridge {
    app_access_token: String,
    http_client: reqwest::Client,
}

impl FeishuBridge {
    #[must_use]
    pub(crate) fn new(app_access_token: String, http_client: reqwest::Client) -> Self {
        Self {
            app_access_token,
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
impl ChatBridge for FeishuBridge {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Feishu/Lark bridge starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = "https://open.feishu.cn/open-apis/im/v1/messages?receive_id_type=chat_id";
        let body = json!({
            "receive_id": channel,
            "msg_type": "text",
            "content": json!({"text": message}).to_string(),
        });
        let response = self
            .http_client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.app_access_token))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Feishu send error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let mut text = msg.text.clone();
        for block in &msg.code_blocks {
            text.push_str(&format!("\n```{}\n{}\n```", block.language, block.content));
        }
        self.send_message(channel, &text).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction> {
        // Feishu event callback format.
        // Challenge verification: { "challenge": "...", "token": "...", "type": "url_verification" }
        if payload.get("type").and_then(|t| t.as_str()) == Some("url_verification") {
            return Ok(BridgeAction::Ignore);
        }

        let event = payload.get("event").unwrap_or(&Value::Null);
        let message = event.get("message").unwrap_or(&Value::Null);
        let msg_type = message
            .get("message_type")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if msg_type != "text" {
            return Ok(BridgeAction::Ignore);
        }

        let content_str = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("{}");
        let content: Value = serde_json::from_str(content_str).unwrap_or(Value::Null);
        let text = content.get("text").and_then(|t| t.as_str()).unwrap_or("");

        let chat_id = message
            .get("chat_id")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_owned();
        let _sender = event
            .get("sender")
            .and_then(|s| s.get("sender_id"))
            .and_then(|s| s.get("open_id"))
            .and_then(|o| o.as_str())
            .unwrap_or("")
            .to_owned();

        if let Some(prompt) = text.strip_prefix("/savfox ") {
            let prompt = prompt.trim().to_owned();
            if !prompt.is_empty() {
                return Ok(BridgeAction::StartThread {
                    channel: chat_id,
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

    // Handle Feishu URL verification challenge.
    if body.get("type").and_then(|t| t.as_str()) == Some("url_verification") {
        let challenge = body.get("challenge").and_then(|c| c.as_str()).unwrap_or("");
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "challenge": challenge })));
        return;
    }

    let event = body.get("event").unwrap_or(&Value::Null);
    let message = event.get("message").unwrap_or(&Value::Null);
    let mut chat_id = String::new();
    let mut prompt = String::new();
    if message.get("message_type").and_then(|t| t.as_str()) == Some("text") {
        let content_str = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("{}");
        let content: Value = serde_json::from_str(content_str).unwrap_or(Value::Null);
        let text = content.get("text").and_then(|t| t.as_str()).unwrap_or("");
        if let Some(stripped) = text.strip_prefix("/savfox ").map(str::trim)
            && !stripped.is_empty()
        {
            chat_id = message
                .get("chat_id")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_owned();
            prompt = stripped.to_owned();
        }
    }

    let dedupe_key = body
        .get("header")
        .and_then(|h| h.get("event_id"))
        .and_then(|v| v.as_str())
        .or_else(|| message.get("message_id").and_then(|v| v.as_str()))
        .map(|id| format!("feishu:{id}"));

    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if !chat_id.is_empty() && !prompt.is_empty() {
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
                "feishu",
                chat_id,
                prompt,
                None,
            )
            .await;
        });
    }

    info!("Feishu webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
