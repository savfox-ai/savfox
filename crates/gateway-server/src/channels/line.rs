use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{Channel, RichMessage, runtime};
use crate::bridge::GatewayChannel;
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

/// LINE Messaging API bridge.
pub(crate) struct LineChannel {
    channel_token: String,
    http_client: reqwest::Client,
}

impl LineChannel {
    #[must_use]
    pub(crate) fn new(channel_token: String, http_client: reqwest::Client) -> Self {
        Self {
            channel_token,
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
impl Channel for LineChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("LINE bridge starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = "https://api.line.me/v2/bot/message/push";
        let body = json!({
            "to": channel,
            "messages": [{ "type": "text", "text": message }]
        });
        let response = self
            .http_client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.channel_token))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "LINE send error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        self.send_message(channel, &msg.text).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        let events = payload
            .get("events")
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();
        for event in &events {
            let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if event_type != "message" {
                continue;
            }
            let message = event.get("message").unwrap_or(&Value::Null);
            let msg_type = message.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if msg_type != "text" {
                continue;
            }
            let text = message.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let source = event.get("source").unwrap_or(&Value::Null);
            let _user_id = source
                .get("userId")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_owned();
            let reply_token = event
                .get("replyToken")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_owned();

            if let Some(prompt) = text.strip_prefix("/savfox ") {
                let prompt = prompt.trim().to_owned();
                if !prompt.is_empty() {
                    return Ok(ChannelAction::StartThread {
                        channel: reply_token,
                        prompt,
                    });
                }
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

    let mut channel = String::new();
    let mut prompt = String::new();
    let mut dedupe_key: Option<String> = None;
    let mut name: Option<String> = None;
    if let Some(events) = body.get("events").and_then(|e| e.as_array()) {
        for event in events {
            if event.get("type").and_then(|t| t.as_str()) != Some("message") {
                continue;
            }
            let message = event.get("message").unwrap_or(&Value::Null);
            if message.get("type").and_then(|t| t.as_str()) != Some("text") {
                continue;
            }
            let text = message.get("text").and_then(|t| t.as_str()).unwrap_or("");
            if let Some(stripped) = text.strip_prefix("/savfox ").map(str::trim)
                && !stripped.is_empty()
            {
                channel = event
                    .get("source")
                    .and_then(|s| {
                        s.get("userId")
                            .or_else(|| s.get("groupId"))
                            .or_else(|| s.get("roomId"))
                    })
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                prompt = stripped.to_owned();
                dedupe_key = message
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|id| format!("line:{id}"));
                name = event
                    .get("source")
                    .and_then(|s| s.get("userId"))
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned);
                break;
            }
        }
    }

    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if !channel.is_empty() && !prompt.is_empty() {
        let bridge = match depot.obtain::<Arc<GatewayChannel>>() {
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
                "line",
                channel,
                prompt,
                name,
            )
            .await;
        });
    }

    info!("LINE webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
