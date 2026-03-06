use std::sync::Arc;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use salvo::prelude::*;
use serde_json::{Value, json};
use sha2::Sha256;
use tracing::{error, info, warn};

use super::{Channel, RichMessage, runtime};
use crate::channel::GatewayChannel;
use crate::config::{GatewayConfig, WhatsAppChannelConfig};
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

fn verify_whatsapp_signature(app_secret: &str, body: &[u8], signature: &str) -> bool {
    let expected = signature.strip_prefix("sha256=").unwrap_or(signature);

    let mut mac = match Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = hex::encode(mac.finalize().into_bytes());

    computed == expected
}

/// WhatsApp Business API channel using Cloud API with webhook mode.
pub(crate) struct WhatsAppChannel {
    config: WhatsAppChannelConfig,
    http_client: reqwest::Client,
    phone_number_id: String,
    access_token: String,
}

impl WhatsAppChannel {
    #[must_use]
    pub(crate) fn new(config: WhatsAppChannelConfig, http_client: reqwest::Client) -> Self {
        let phone_number_id = config.phone_number_id.clone();
        let access_token = config.access_token.clone();
        Self {
            config,
            http_client,
            phone_number_id,
            access_token,
        }
    }

    /// Parse a WhatsApp webhook payload into a `ChannelAction`.
    fn parse_webhook_payload(payload: &Value) -> anyhow::Result<ChannelAction> {
        let entry = payload
            .get("entry")
            .and_then(|e| e.as_array())
            .and_then(|arr| arr.first())
            .ok_or_else(|| anyhow::anyhow!("missing entry"))?;

        let changes = entry
            .get("changes")
            .and_then(|c| c.as_array())
            .ok_or_else(|| anyhow::anyhow!("missing changes"))?;

        for change in changes {
            let value = change.get("value").unwrap_or(&Value::Null);

            let messages = value.get("messages").and_then(|m| m.as_array());
            if let Some(messages) = messages {
                for message in messages {
                    let from = message
                        .get("from")
                        .and_then(|f| f.as_str())
                        .unwrap_or_default();

                    let text = message
                        .get("text")
                        .and_then(|t| t.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");

                    if !text.is_empty() {
                        return Ok(ChannelAction::StartThread {
                            channel: from.to_string(),
                            prompt: text.to_owned(),
                        });
                    }
                }
            }
        }

        Ok(ChannelAction::Ignore)
    }
}

#[async_trait]
impl Channel for WhatsAppChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("WhatsApp channel initialized (webhook mode)");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = format!(
            "https://graph.facebook.com/v18.0/{}/messages",
            self.phone_number_id
        );

        let body = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": channel,
            "type": "text",
            "text": {
                "preview_url": false,
                "body": message
            }
        });

        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(to = %channel, "WhatsApp API error: HTTP {status}: {resp_body}");
            anyhow::bail!("WhatsApp API error: HTTP {status}");
        }

        info!(to = %channel, "WhatsApp message sent");
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let mut text = msg.text.clone();
        for block in &msg.code_blocks {
            text.push_str(&format!("\n```\n{}\n```", block.content));
        }

        if text.len() > 4096 {
            text = text[..4093].to_string();
            text.push_str("...");
        }

        self.send_message(channel, &text).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        Self::parse_webhook_payload(&payload)
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

/// `GET /webhooks/whatsapp`: WhatsApp webhook verification.
#[handler]
pub(crate) async fn webhook_verification_handler(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) {
    let verify_token = depot
        .obtain::<Arc<GatewayConfig>>()
        .ok()
        .and_then(|cfg| {
            cfg.channels
                .whatsapp
                .as_ref()
                .and_then(|b| b.verify_token.clone())
        })
        .or_else(|| std::env::var("WHATSAPP_VERIFY_TOKEN").ok());

    let mode = req.query::<String>("hub.mode").unwrap_or_default();
    let token = req.query::<String>("hub.verify_token").unwrap_or_default();
    let challenge = req.query::<String>("hub.challenge").unwrap_or_default();

    if mode != "subscribe" {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(json!({"error": "invalid mode"}).to_string()));
        return;
    }

    if let Some(expected) = verify_token {
        if token != expected {
            res.status_code(StatusCode::FORBIDDEN);
            res.render(Text::Json(
                json!({"error": "verification failed"}).to_string(),
            ));
            return;
        }
    }

    res.status_code(StatusCode::OK);
    res.render(Text::Plain(challenge));
}

/// `POST /webhooks/whatsapp`: Handle WhatsApp Cloud API webhook events.
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let app_secret = depot
        .obtain::<Arc<GatewayConfig>>()
        .ok()
        .and_then(|cfg| {
            cfg.channels
                .whatsapp
                .as_ref()
                .and_then(|b| b.app_secret.clone())
        })
        .or_else(|| std::env::var("WHATSAPP_APP_SECRET").ok());

    let body_bytes = match req.payload().await {
        Ok(bytes) => bytes.to_vec(),
        Err(err) => {
            warn!("WhatsApp webhook: failed to read body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                "failed to read body",
            );
            return;
        }
    };

    if let Some(secret) = app_secret {
        let signature = req
            .headers()
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();

        if !verify_whatsapp_signature(&secret, &body_bytes, signature) {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "signature verification failed",
            );
            return;
        }
    }

    let body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(err) => {
            warn!("WhatsApp webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                "failed to parse JSON",
            );
            return;
        }
    };

    let action = match WhatsAppChannel::parse_webhook_payload(&body) {
        Ok(action) => action,
        Err(err) => {
            warn!("WhatsApp webhook: failed to parse payload: {err}");
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
            info!(from = %channel, "WhatsApp: starting thread with prompt");

            let message_id = body
                .get("entry")
                .and_then(|e| e.as_array())
                .and_then(|arr| arr.first())
                .and_then(|entry| entry.get("changes"))
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|change| change.get("value"))
                .and_then(|v| v.get("messages"))
                .and_then(|m| m.as_array())
                .and_then(|arr| arr.first())
                .and_then(|msg| msg.get("id"))
                .and_then(|id| id.as_str())
                .map(|id| format!("whatsapp:{id}"));

            if runtime::should_drop_duplicate(message_id).await {
                res.status_code(StatusCode::OK);
                return;
            }

            let channel = match depot.obtain::<Arc<GatewayChannel>>() {
                Ok(channel) => channel.clone(),
                Err(err) => {
                    warn!("WhatsApp webhook: missing gateway channel state: {err:?}");
                    render_error(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "state_unavailable",
                        "gateway channel unavailable",
                    );
                    return;
                }
            };
            let session_store = match depot.obtain::<Arc<SessionStore>>() {
                Ok(store) => store.clone(),
                Err(err) => {
                    warn!("WhatsApp webhook: missing session store: {err:?}");
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
                    channel,
                    session_store,
                    "whatsapp",
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
            info!(thread_id = %thread_id, decision = %decision, "WhatsApp: approval response");
        }
        ChannelAction::Ignore | ChannelAction::SendToThread { .. } => {}
    }

    res.status_code(StatusCode::OK);
}
