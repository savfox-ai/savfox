use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::bridge::GatewayBridge;
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

use super::{ChatBridge, RichMessage, runtime};

/// Mattermost chat bridge using the Mattermost REST API v4.
pub(crate) struct MattermostBridge {
    server_url: String,
    access_token: String,
    http_client: reqwest::Client,
}

impl MattermostBridge {
    #[must_use]
    pub(crate) fn new(
        server_url: String,
        access_token: String,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            server_url,
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
impl ChatBridge for MattermostBridge {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!(server = %self.server_url, "Mattermost bridge starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = format!("{}/api/v4/posts", self.server_url);
        let body = json!({ "channel_id": channel, "message": message });
        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Mattermost send error: HTTP {status}: {}",
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
        let text = payload.get("text").and_then(|t| t.as_str()).unwrap_or("");
        let channel_id = payload
            .get("channel_id")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_owned();
        let _user_id = payload
            .get("user_id")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_owned();

        if let Some(prompt) = text.strip_prefix("/savfox ") {
            let prompt = prompt.trim().to_owned();
            if !prompt.is_empty() {
                return Ok(BridgeAction::StartThread {
                    channel: channel_id,
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

    let text = body.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let channel = body
        .get("channel_id")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_owned();
    let dedupe_key = body
        .get("post_id")
        .or_else(|| body.get("trigger_id"))
        .and_then(|v| v.as_str())
        .map(|id| format!("mattermost:{id}"));

    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if let Some(prompt) = text.strip_prefix("/savfox ").map(str::trim)
        && !prompt.is_empty()
        && !channel.is_empty()
    {
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
        let prompt = prompt.to_owned();
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline(
                bridge,
                session_store,
                "mattermost",
                channel,
                prompt,
                None,
            )
            .await;
        });
    }

    info!("Mattermost webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({ "status": "ok" })));
}
