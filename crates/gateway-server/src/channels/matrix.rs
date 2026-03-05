use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{ChatBridge, RichMessage, runtime};
use crate::bridge::GatewayBridge;
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

/// Matrix chat bridge using the Client-Server API with appservice or webhook mode.
pub(crate) struct MatrixBridge {
    homeserver_url: String,
    access_token: String,
    http_client: reqwest::Client,
}

impl MatrixBridge {
    #[must_use]
    pub(crate) fn new(
        homeserver_url: String,
        access_token: String,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            homeserver_url,
            access_token,
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
impl ChatBridge for MatrixBridge {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!(homeserver = %self.homeserver_url, "Matrix bridge starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let txn_id = uuid::Uuid::now_v7().to_string();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver_url, channel, txn_id
        );
        let body = json!({
            "msgtype": "m.text",
            "body": message,
        });

        let response = self
            .http_client
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Matrix send error: HTTP {status}: {}",
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

        let txn_id = uuid::Uuid::now_v7().to_string();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver_url, channel, txn_id
        );

        let html = text.replace('\n', "<br/>");
        let body = json!({
            "msgtype": "m.text",
            "body": text,
            "format": "org.matrix.custom.html",
            "formatted_body": html,
        });

        let response = self
            .http_client
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Matrix rich send error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction> {
        // Matrix appservice transaction format:
        // { "events": [{ "type": "m.room.message", "content": {...}, "room_id": "...", "sender":
        // "..." }] }
        let events = payload
            .get("events")
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();

        for event in &events {
            let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if event_type != "m.room.message" {
                continue;
            }

            let content = event.get("content").unwrap_or(&Value::Null);
            let msgtype = content
                .get("msgtype")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if msgtype != "m.text" {
                continue;
            }

            let body = content.get("body").and_then(|b| b.as_str()).unwrap_or("");
            let room_id = event
                .get("room_id")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_owned();
            let _sender = event
                .get("sender")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_owned();

            if let Some(prompt) = body.strip_prefix("!savfox ") {
                let prompt = prompt.trim().to_owned();
                if !prompt.is_empty() {
                    return Ok(BridgeAction::StartThread {
                        channel: room_id,
                        prompt,
                    });
                }
            }
        }

        Ok(BridgeAction::Ignore)
    }
}

/// Salvo handler for Matrix appservice webhook.
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

    let mut action = BridgeAction::Ignore;
    let mut dedupe_key: Option<String> = None;
    if let Some(events) = body.get("events").and_then(|e| e.as_array()) {
        for event in events {
            if event.get("type").and_then(|t| t.as_str()) != Some("m.room.message") {
                continue;
            }
            let content = event.get("content").unwrap_or(&Value::Null);
            if content.get("msgtype").and_then(|m| m.as_str()) != Some("m.text") {
                continue;
            }
            let text = content.get("body").and_then(|b| b.as_str()).unwrap_or("");
            if let Some(prompt) = text.strip_prefix("!savfox ").map(str::trim)
                && !prompt.is_empty()
            {
                let room_id = event
                    .get("room_id")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_owned();
                if room_id.is_empty() {
                    break;
                }
                dedupe_key = event
                    .get("event_id")
                    .and_then(|v| v.as_str())
                    .map(|id| format!("matrix:{id}"));
                action = BridgeAction::StartThread {
                    channel: room_id,
                    prompt: prompt.to_owned(),
                };
                break;
            }
        }
    }

    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if let BridgeAction::StartThread { channel, prompt } = action {
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
                "matrix",
                channel,
                prompt,
                None,
            )
            .await;
        });
    }

    info!("Matrix webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
