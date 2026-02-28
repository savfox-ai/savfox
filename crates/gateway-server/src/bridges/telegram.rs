use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{error, info, warn};

use super::{ChatBridge, RichMessage, runtime};
use crate::auto_reply::CommandRegistry;
use crate::bridge::{GatewayBridge, verify_telegram_webhook_secret};
use crate::config::{GatewayConfig, TelegramBridgeConfig};
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

/// Telegram bot bridge using the Bot API with webhook mode.
pub(crate) struct TelegramBridge {
    config: TelegramBridgeConfig,
    http_client: reqwest::Client,
    bot_token: String,
}

fn split_telegram_command(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let (head, rest) = if let Some(idx) = trimmed.find(char::is_whitespace) {
        let (head, tail) = trimmed.split_at(idx);
        (head, tail.trim())
    } else {
        (trimmed, "")
    };
    let command = head
        .trim_start_matches('/')
        .split('@')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if command.is_empty() {
        return None;
    }
    Some((command, rest.to_string()))
}

fn normalize_registry_command(text: &str) -> Option<String> {
    let (command, args) = split_telegram_command(text)?;
    let registry = CommandRegistry::new();
    let canonical = registry.resolve_command_name(&command)?;
    let mut prompt = format!("/{canonical}");
    let args = args.trim();
    if !args.is_empty() {
        prompt.push(' ');
        prompt.push_str(args);
    }
    Some(prompt)
}

impl TelegramBridge {
    #[must_use]
    pub(crate) fn new(config: TelegramBridgeConfig, http_client: reqwest::Client) -> Self {
        let bot_token = config.bot_token.clone();
        Self {
            config,
            http_client,
            bot_token,
        }
    }

    /// Parse a Telegram Update payload into a `BridgeAction`.
    fn parse_update(payload: &Value) -> anyhow::Result<BridgeAction> {
        let message = payload.get("message").unwrap_or(&Value::Null);
        let text = message.get("text").and_then(|t| t.as_str()).unwrap_or("");

        let chat_id = message
            .get("chat")
            .and_then(|c| c.get("id"))
            .and_then(|id| id.as_i64())
            .map(|id| id.to_string())
            .unwrap_or_default();

        if let Some((command, args)) = split_telegram_command(text) {
            if command == "savfox" {
                let prompt = args.trim().to_string();
                if prompt.is_empty() {
                    return Ok(BridgeAction::Ignore);
                }
                return Ok(BridgeAction::StartThread {
                    channel: chat_id,
                    prompt,
                });
            }

            if let Some(prompt) = normalize_registry_command(text) {
                return Ok(BridgeAction::StartThread {
                    channel: chat_id,
                    prompt,
                });
            }
        }

        // Handle callback queries (button presses for approvals).
        if let Some(callback) = payload.get("callback_query") {
            let data = callback.get("data").and_then(|d| d.as_str()).unwrap_or("");

            if let Some(thread_id) = data.strip_prefix("approve:") {
                return Ok(BridgeAction::Approve {
                    thread_id: thread_id.to_owned(),
                    decision: true,
                });
            }
            if let Some(thread_id) = data.strip_prefix("deny:") {
                return Ok(BridgeAction::Approve {
                    thread_id: thread_id.to_owned(),
                    decision: false,
                });
            }
        }

        Ok(BridgeAction::Ignore)
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

fn parse_display_name(payload: &Value) -> Option<String> {
    let from = payload.pointer("/message/from").unwrap_or(&Value::Null);
    let first = from
        .get("first_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let last = from
        .get("last_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let username = from
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if !first.is_empty() || !last.is_empty() {
        let full = format!("{first} {last}").trim().to_string();
        if !full.is_empty() {
            return Some(full);
        }
    }
    username.map(str::to_string)
}

fn parse_start_meta(payload: &Value) -> runtime::StartThreadMeta {
    let peer_id = payload
        .pointer("/message/from/id")
        .and_then(Value::as_i64)
        .map(|v| v.to_string());
    let chat_id = payload
        .pointer("/message/chat/id")
        .and_then(Value::as_i64)
        .map(|v| v.to_string());
    let chat_type = payload
        .pointer("/message/chat/type")
        .and_then(Value::as_str)
        .map(str::to_string);
    let is_group = matches!(
        chat_type.as_deref(),
        Some("group" | "supergroup" | "channel")
    );
    let thread_id = payload
        .pointer("/message/message_thread_id")
        .and_then(Value::as_i64)
        .map(|v| v.to_string());
    let reply_target = payload
        .pointer("/message/reply_to_message/message_id")
        .and_then(Value::as_i64)
        .map(|v| v.to_string());
    let topic = payload
        .pointer("/message/chat/title")
        .and_then(Value::as_str)
        .map(str::to_string);

    runtime::StartThreadMeta {
        peer_id,
        group_id: if is_group { chat_id } else { None },
        thread_id: thread_id.clone(),
        parent_thread_id: thread_id,
        reply_target,
        chat_type,
        topic,
        ..runtime::StartThreadMeta::default()
    }
}

#[async_trait]
impl ChatBridge for TelegramBridge {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Telegram bridge initialized (webhook mode)");
        // Webhook URL registration should be done via Telegram Bot API `setWebhook`.
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let body = json!({
            "chat_id": channel,
            "text": message,
        });

        let response = self.http_client.post(&url).json(&body).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(chat_id = %channel, "Telegram API error: HTTP {status}: {resp_body}");
            anyhow::bail!("Telegram API error: HTTP {status}");
        }

        info!(chat_id = %channel, "Telegram message sent");
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);

        // Format code blocks with Telegram MarkdownV2.
        let mut text = msg.text.clone();
        for block in &msg.code_blocks {
            text.push_str(&format!("\n```{}\n{}\n```", block.language, block.content));
        }

        let body = json!({
            "chat_id": channel,
            "text": text,
            "parse_mode": "MarkdownV2",
        });

        let response = self.http_client.post(&url).json(&body).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(chat_id = %channel, "Telegram API error: HTTP {status}: {resp_body}");
            anyhow::bail!("Telegram API error: HTTP {status}");
        }

        info!(chat_id = %channel, "Telegram rich message sent");
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction> {
        Self::parse_update(&payload)
    }
}

/// `POST /webhooks/telegram`: Handle Telegram Bot API webhook updates.
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let expected_secret = depot
        .obtain::<Arc<GatewayConfig>>()
        .ok()
        .and_then(|cfg| {
            cfg.bridges
                .telegram
                .as_ref()
                .and_then(|b| b.webhook_secret_token.clone())
        })
        .or_else(|| std::env::var("TELEGRAM_WEBHOOK_SECRET_TOKEN").ok());
    if let Some(expected_secret) = expected_secret {
        let received_secret = req
            .headers()
            .get("x-telegram-bot-api-secret-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if !verify_telegram_webhook_secret(&expected_secret, received_secret) {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "Telegram webhook secret token verification failed",
            );
            return;
        }
    }

    let body = match req.parse_json::<Value>().await {
        Ok(v) => v,
        Err(err) => {
            warn!("Telegram webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse Telegram payload: {err}"),
            );
            return;
        }
    };

    let action = match TelegramBridge::parse_update(&body) {
        Ok(action) => action,
        Err(err) => {
            warn!("Telegram webhook: failed to parse update: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse Telegram update: {err}"),
            );
            return;
        }
    };

    match action {
        BridgeAction::StartThread { channel, prompt } => {
            info!(chat_id = %channel, "Telegram: starting thread with prompt: {prompt}");
            let update_id = body
                .get("update_id")
                .and_then(|v| v.as_i64())
                .map(|id| format!("telegram:{id}"));
            if runtime::should_drop_duplicate(update_id).await {
                res.status_code(StatusCode::OK);
                return;
            }

            let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
                Ok(bridge) => bridge.clone(),
                Err(err) => {
                    warn!("Telegram webhook: missing gateway bridge state: {err:?}");
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
                    warn!("Telegram webhook: missing session store state: {err:?}");
                    render_error(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "state_unavailable",
                        "session store state unavailable",
                    );
                    return;
                }
            };
            let meta = parse_start_meta(&body);
            let display_name = parse_display_name(&body);

            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta(
                    bridge,
                    session_store,
                    "telegram",
                    channel,
                    prompt,
                    display_name,
                    Some(meta),
                )
                .await;
            });
        }
        BridgeAction::Approve {
            thread_id,
            decision,
        } => {
            info!(thread_id = %thread_id, decision = %decision, "Telegram: approval response");
        }
        BridgeAction::Ignore | BridgeAction::SendToThread { .. } => {}
    }

    // Telegram expects 200 OK for all webhook responses.
    res.status_code(StatusCode::OK);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::TelegramBridge;
    use crate::protocol::BridgeAction;

    #[test]
    fn supports_commands_alias_surface() {
        let payload = json!({
            "message": {
                "text": "/commands",
                "chat": { "id": 42 }
            }
        });

        let action = TelegramBridge::parse_update(&payload).expect("parse should succeed");
        match action {
            BridgeAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "42");
                assert_eq!(prompt, "/commands");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn supports_bot_qualified_savfox_command() {
        let payload = json!({
            "message": {
                "text": "/savfox@mybot summarize this",
                "chat": { "id": 42 }
            }
        });

        let action = TelegramBridge::parse_update(&payload).expect("parse should succeed");
        match action {
            BridgeAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "42");
                assert_eq!(prompt, "summarize this");
            }
            _ => panic!("expected start thread action"),
        }
    }
}
