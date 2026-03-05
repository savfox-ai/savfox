use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{error, info, warn};

use super::{Channel, RichMessage, runtime};
use crate::bridge::GatewayBridge;
use crate::config::ZaloChannelConfig;
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

/// Zalo OA (Official Account) API base URL.
const ZALO_OA_API_BASE: &str = "https://openapi.zalo.me/v3.0/oa";

/// Zalo OA bridge using webhook mode.
///
/// Receives incoming messages via webhook events from the Zalo OA platform and
/// sends replies using the Zalo OA Customer Service Message API.
pub(crate) struct ZaloChannel {
    config: ZaloChannelConfig,
    bridge: Arc<GatewayBridge>,
    http: reqwest::Client,
}

impl std::fmt::Debug for ZaloChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZaloChannel")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ZaloChannel {
    #[must_use]
    pub(crate) fn new(
        config: ZaloChannelConfig,
        bridge: Arc<GatewayBridge>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            config,
            bridge,
            http,
        }
    }

    /// Parse a Zalo OA webhook event payload into a `BridgeAction`.
    fn parse_event(payload: &Value) -> anyhow::Result<BridgeAction> {
        let event_name = payload
            .get("event_name")
            .and_then(|e| e.as_str())
            .unwrap_or("");

        // Only handle user-sent text messages.
        if event_name != "user_send_text" {
            return Ok(BridgeAction::Ignore);
        }

        let sender_id = payload
            .get("sender")
            .and_then(|s| s.get("id"))
            .and_then(|id| id.as_str())
            .unwrap_or("")
            .to_owned();

        let text = payload
            .get("message")
            .and_then(|m| m.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("");

        // Parse `/savfox <prompt>` commands.
        if let Some(prompt) = text.strip_prefix("/savfox ") {
            let prompt = prompt.trim().to_owned();
            if prompt.is_empty() {
                return Ok(BridgeAction::Ignore);
            }
            return Ok(BridgeAction::StartThread {
                channel: sender_id,
                prompt,
            });
        }

        // Bare `/savfox` with no arguments.
        if text == "/savfox" {
            return Ok(BridgeAction::Ignore);
        }

        Ok(BridgeAction::Ignore)
    }

    /// Verify the webhook signature using HMAC-SHA256 with the app secret.
    fn verify_signature(app_secret: &str, body: &[u8], signature: &str) -> bool {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let mut mac = match Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) {
            Ok(mac) => mac,
            Err(_) => return false,
        };
        mac.update(body);
        let computed = hex::encode(mac.finalize().into_bytes());

        let expected = signature.strip_prefix("mac=").unwrap_or(signature);
        computed == expected
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
impl Channel for ZaloChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Zalo OA bridge initialized (webhook mode)");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = format!("{ZALO_OA_API_BASE}/message/cs");
        let body = json!({
            "recipient": {
                "user_id": channel,
            },
            "message": {
                "text": message,
            },
        });

        let response = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("access_token", &self.config.access_token)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(user_id = %channel, "Zalo OA API error: HTTP {status}: {resp_body}");
            anyhow::bail!("Zalo OA API error: HTTP {status}");
        }

        // Zalo API returns `{ "error": 0, "message": "Success" }` on success.
        // Non-zero error codes indicate failure even with HTTP 200.
        let resp_text = response.text().await.unwrap_or_default();
        if let Ok(resp_json) = serde_json::from_str::<Value>(&resp_text) {
            let err_code = resp_json.get("error").and_then(|e| e.as_i64()).unwrap_or(0);
            if err_code != 0 {
                let err_msg = resp_json
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error");
                error!(user_id = %channel, "Zalo OA API error: code={err_code}, message={err_msg}");
                anyhow::bail!("Zalo OA API error: code={err_code}, message={err_msg}");
            }
        }

        info!(user_id = %channel, "Zalo OA message sent");
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let url = format!("{ZALO_OA_API_BASE}/message/cs");

        // Build message body with Zalo list template for code blocks,
        // or fall back to plain text if there are no code blocks.
        if msg.code_blocks.is_empty() {
            return self.send_message(channel, &msg.text).await;
        }

        // Format code blocks as plain text since Zalo does not support Markdown.
        let mut text = msg.text.clone();
        if let Some(title) = &msg.title {
            text = format!("{title}\n\n{text}");
        }
        for block in &msg.code_blocks {
            text.push_str(&format!("\n\n[{}]\n{}", block.language, block.content));
        }

        let body = json!({
            "recipient": {
                "user_id": channel,
            },
            "message": {
                "text": text,
            },
        });

        let response = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("access_token", &self.config.access_token)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(user_id = %channel, "Zalo OA API error: HTTP {status}: {resp_body}");
            anyhow::bail!("Zalo OA API error: HTTP {status}");
        }

        info!(user_id = %channel, "Zalo OA rich message sent");
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction> {
        Self::parse_event(&payload)
    }
}

/// `POST /webhooks/zalo` -- Handle Zalo OA webhook events.
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    // Verify webhook signature if app_secret is configured.
    let config = depot
        .obtain::<Arc<crate::config::GatewayConfig>>()
        .ok()
        .and_then(|cfg| cfg.bridges.zalo.as_ref().cloned());

    if let Some(ref cfg) = config {
        let signature = req
            .headers()
            .get("x-zalooa-signature")
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);

        if let Some(signature) = signature {
            // Read raw body bytes for signature verification.
            let body_bytes = match req.payload().await {
                Ok(bytes) => bytes.to_vec(),
                Err(err) => {
                    warn!("Zalo webhook: failed to read body for signature verification: {err}");
                    render_error(
                        res,
                        StatusCode::BAD_REQUEST,
                        "invalid_payload",
                        "failed to read request body",
                    );
                    return;
                }
            };

            if !ZaloChannel::verify_signature(&cfg.app_secret, &body_bytes, &signature) {
                render_error(
                    res,
                    StatusCode::UNAUTHORIZED,
                    "invalid_signature",
                    "Zalo webhook signature verification failed",
                );
                return;
            }
        }
    }

    let body = match req.parse_json::<Value>().await {
        Ok(v) => v,
        Err(err) => {
            warn!("Zalo webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse Zalo payload: {err}"),
            );
            return;
        }
    };

    let action = match ZaloChannel::parse_event(&body) {
        Ok(action) => action,
        Err(err) => {
            warn!("Zalo webhook: failed to parse event: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse Zalo event: {err}"),
            );
            return;
        }
    };

    match action {
        BridgeAction::StartThread { channel, prompt } => {
            info!(user_id = %channel, "Zalo: starting thread with prompt: {prompt}");
            let event_key = body
                .get("message")
                .and_then(|m| m.get("msg_id"))
                .and_then(|id| id.as_str())
                .map(|id| format!("zalo:{id}"));
            if runtime::should_drop_duplicate(event_key).await {
                res.status_code(StatusCode::OK);
                return;
            }

            let name = body
                .get("sender")
                .and_then(|s| s.get("id"))
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned);

            let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
                Ok(bridge) => bridge.clone(),
                Err(err) => {
                    warn!("Zalo webhook: missing gateway bridge state: {err:?}");
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
                Err(err) => {
                    warn!("Zalo webhook: missing session store state: {err:?}");
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
                    "zalo",
                    channel,
                    prompt,
                    name,
                )
                .await;
            });
        }
        BridgeAction::Approve {
            thread_id,
            decision,
        } => {
            info!(thread_id = %thread_id, decision = %decision, "Zalo: approval response");
        }
        BridgeAction::Ignore | BridgeAction::SendToThread { .. } => {}
    }

    // Zalo expects 200 OK for all webhook responses.
    res.status_code(StatusCode::OK);
}
