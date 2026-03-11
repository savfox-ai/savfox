use async_trait::async_trait;
use salvo::prelude::*;
use savfox_channels::http::warn_on_error;
use serde_json::{Value, json};
use tracing::info;

use super::{Channel, ensure_inbound_channel_enabled, render_error, runtime};
use crate::protocol::ChannelAction;

/// Google Chat channel using webhook and Spaces API.
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

#[async_trait]
impl Channel for GoogleChatChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Google Chat channel starting");
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
        warn_on_error(response, "Google Chat send error").await;
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
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
                return Ok(ChannelAction::StartThread {
                    channel: space,
                    prompt,
                });
            }
        }
        Ok(ChannelAction::Ignore)
    }
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !ensure_inbound_channel_enabled(depot, res, "googlechat").await {
        return;
    }

    let Some(body) = super::parse_json_body(req, res, "googlechat").await else {
        return;
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

    let (expected_space_id, verification_token) = {
        let savfox_home = depot
            .obtain::<std::sync::Arc<crate::channel::GatewayChannel>>()
            .ok()
            .map(|channel| channel.config().savfox_home.clone());
        if let Some(savfox_home) = savfox_home {
            (
                savfox_channels::googlechat::resolve_googlechat_space_id(&savfox_home)
                    .await
                    .ok()
                    .flatten(),
                savfox_channels::googlechat::resolve_googlechat_verification_token(&savfox_home)
                    .await
                    .ok()
                    .flatten(),
            )
        } else {
            (None, None)
        }
    };
    if let Some(expected) = verification_token {
        let provided = req
            .headers()
            .get("x-goog-chat-token")
            .and_then(|v| v.to_str().ok())
            .or_else(|| {
                req.headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
            })
            .unwrap_or_default();
        if provided != expected {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "Google Chat verification token mismatch",
            );
            return;
        }
    }
    if let Some(expected_space_id) = expected_space_id.as_deref()
        && !channel.is_empty()
        && channel != expected_space_id
    {
        render_error(
            res,
            StatusCode::FORBIDDEN,
            "unexpected_space",
            format!("unexpected Google Chat space: {channel}"),
        );
        return;
    }

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
        let Some((gateway_channel, session_store)) = super::obtain_channel_and_store(depot, res)
        else {
            return;
        };
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline(
                gateway_channel,
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
