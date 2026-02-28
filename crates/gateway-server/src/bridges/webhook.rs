use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{error, info, warn};

use super::{ChatBridge, RichMessage, runtime};
use crate::bridge::GatewayBridge;
use crate::config::{GatewayConfig, WebhookBridgeConfig};
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

/// Generic webhook bridge for custom integrations.
///
/// Accepts JSON payloads in a simple format:
/// ```json
/// {
///   "action": "start_thread",
///   "channel": "my-channel",
///   "prompt": "Explain this code..."
/// }
/// ```
pub(crate) struct GenericWebhookBridge {
    config: WebhookBridgeConfig,
    http_client: reqwest::Client,
    callback_url: Option<String>,
    secret: Option<String>,
}

impl GenericWebhookBridge {
    #[must_use]
    pub(crate) fn new(config: WebhookBridgeConfig, http_client: reqwest::Client) -> Self {
        let callback_url = config.callback_url.clone();
        let secret = config.secret.clone();
        Self {
            config,
            http_client,
            callback_url,
            secret,
        }
    }

    /// Verify HMAC-SHA256 signature if a secret is configured.
    fn verify_signature(secret: &str, signature: &str, body: &[u8]) -> bool {
        crate::bridge::verify_webhook_hmac(secret, signature, body)
    }

    /// Parse a generic webhook payload.
    fn parse_payload(payload: &Value) -> anyhow::Result<BridgeAction> {
        let action = payload.get("action").and_then(|a| a.as_str()).unwrap_or("");

        match action {
            "start_thread" => {
                let channel = payload
                    .get("channel")
                    .and_then(|c| c.as_str())
                    .unwrap_or("default")
                    .to_owned();
                let prompt = payload
                    .get("prompt")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_owned();

                if prompt.is_empty() {
                    Ok(BridgeAction::Ignore)
                } else {
                    Ok(BridgeAction::StartThread { channel, prompt })
                }
            }

            "send_message" => {
                let thread_id = payload
                    .get("thread_id")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_owned();
                let message = payload
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_owned();

                if thread_id.is_empty() || message.is_empty() {
                    Ok(BridgeAction::Ignore)
                } else {
                    Ok(BridgeAction::SendToThread { thread_id, message })
                }
            }

            "approve" => {
                let thread_id = payload
                    .get("thread_id")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_owned();
                let decision = payload
                    .get("decision")
                    .and_then(|d| d.as_bool())
                    .unwrap_or(false);

                if thread_id.is_empty() {
                    Ok(BridgeAction::Ignore)
                } else {
                    Ok(BridgeAction::Approve {
                        thread_id,
                        decision,
                    })
                }
            }

            _ => Ok(BridgeAction::Ignore),
        }
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

#[async_trait]
impl ChatBridge for GenericWebhookBridge {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Generic webhook bridge initialized");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let Some(ref callback_url) = self.callback_url else {
            warn!(channel = %channel, "Webhook send_message: no callback_url configured, skipping");
            return Ok(());
        };

        let body = json!({
            "channel": channel,
            "text": message,
            "type": "message",
        });

        let response = self
            .http_client
            .post(callback_url)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(channel = %channel, url = %callback_url, "Webhook API error: HTTP {status}: {resp_body}");
            anyhow::bail!("Webhook API error: HTTP {status}");
        }

        info!(channel = %channel, url = %callback_url, "Webhook message sent");
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        self.send_message(channel, &msg.text).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction> {
        Self::parse_payload(&payload)
    }
}

/// `POST /webhooks/webhook`: Handle generic webhook requests.
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let raw_body = match req.payload().await {
        Ok(bytes) => bytes.clone(),
        Err(err) => {
            warn!("Generic webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_body",
                format!("failed to read request body: {err}"),
            );
            return;
        }
    };
    let body = match serde_json::from_slice::<Value>(raw_body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            warn!("Generic webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse webhook payload: {err}"),
            );
            return;
        }
    };

    let configured_secret = depot
        .obtain::<Arc<GatewayConfig>>()
        .ok()
        .and_then(|cfg| cfg.bridges.webhook.as_ref().and_then(|b| b.secret.clone()))
        .or_else(|| std::env::var("WEBHOOK_SIGNING_SECRET").ok());
    if let Some(secret) = configured_secret {
        let signature = req
            .headers()
            .get("x-signature")
            .or_else(|| req.headers().get("x-hub-signature-256"))
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();

        if signature.is_empty() {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "missing_signature",
                "missing webhook signature header",
            );
            return;
        }
        if !GenericWebhookBridge::verify_signature(&secret, signature, raw_body.as_ref()) {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "webhook signature verification failed",
            );
            return;
        }
    }

    let action = match GenericWebhookBridge::parse_payload(&body) {
        Ok(action) => action,
        Err(err) => {
            warn!("Generic webhook: failed to parse payload: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse webhook action: {err}"),
            );
            return;
        }
    };

    let dedupe_key = body
        .get("event_id")
        .or_else(|| body.get("request_id"))
        .and_then(|v| v.as_str())
        .map(|id| format!("webhook:{id}"));
    if runtime::should_drop_duplicate(dedupe_key).await {
        res.render(Text::Json(
            json!({ "status": "duplicate_ignored" }).to_string(),
        ));
        return;
    }

    match &action {
        BridgeAction::StartThread { channel, prompt } => {
            info!(channel = %channel, "Webhook: starting thread with prompt: {prompt}");
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
            let channel = channel.clone();
            let prompt = prompt.clone();
            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline(
                    bridge,
                    session_store,
                    "webhook",
                    channel,
                    prompt,
                    None,
                )
                .await;
            });
        }
        BridgeAction::SendToThread {
            thread_id,
            message: _,
        } => {
            info!(thread_id = %thread_id, "Webhook: sending message to thread");
        }
        BridgeAction::Approve {
            thread_id,
            decision,
        } => {
            info!(thread_id = %thread_id, decision = %decision, "Webhook: approval response");
        }
        BridgeAction::Ignore => {}
    }

    res.render(Text::Json(
        json!({
            "status": "accepted",
            "action": format!("{action:?}"),
        })
        .to_string(),
    ));
}
