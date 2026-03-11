use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use savfox_channels::telegram::{
    TelegramStartMeta, TelegramUpdateSink, parse_display_name, parse_start_meta,
    parse_update_with_resolver,
};
use serde_json::Value;
use tracing::{info, warn};

use super::{render_error, runtime};
use crate::auto_reply::CommandRegistry;
use crate::channel::GatewayChannel;
use crate::channel::verify_telegram_webhook_secret;
use crate::config::GatewayConfig;
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

fn resolve_registry_command_name(command: &str) -> Option<String> {
    let registry = CommandRegistry::new();
    registry.resolve_command_name(command)
}

fn to_runtime_start_meta(meta: TelegramStartMeta) -> runtime::StartThreadMeta {
    let is_group = matches!(
        meta.chat_type.as_deref(),
        Some("group" | "supergroup" | "channel")
    );
    runtime::StartThreadMeta {
        peer_id: meta.peer_id,
        group_id: if is_group { meta.chat_id } else { None },
        thread_id: meta.thread_id.clone(),
        parent_thread_id: meta.thread_id,
        reply_target: meta.reply_target,
        chat_type: meta.chat_type,
        topic: meta.topic,
        ..runtime::StartThreadMeta::default()
    }
}

fn telegram_log_preview(text: &str, max_chars: usize) -> String {
    let mut preview = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

fn telegram_update_summary(body: &Value) -> (String, String, String, String) {
    let kind = if body.get("message").is_some() {
        "message"
    } else if body.get("channel_post").is_some() {
        "channel_post"
    } else if body.get("callback_query").is_some() {
        "callback_query"
    } else {
        "unknown"
    };
    let message = body
        .get("message")
        .or_else(|| body.get("channel_post"))
        .or_else(|| body.get("callback_query").and_then(|v| v.get("message")))
        .unwrap_or(&Value::Null);
    let chat_id = message
        .get("chat")
        .and_then(|chat| chat.get("id"))
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let from_id = message
        .get("from")
        .and_then(|from| from.get("id"))
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let text = message
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| message.get("caption").and_then(Value::as_str))
        .or_else(|| {
            body.get("callback_query")
                .and_then(|query| query.get("data"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    (
        kind.to_string(),
        chat_id,
        from_id,
        telegram_log_preview(text, 160),
    )
}

async fn process_update_payload(
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    body: Value,
) {
    let update_id = body
        .get("update_id")
        .and_then(|v| v.as_i64())
        .map(|id| format!("telegram:{id}"));
    let (kind, chat_id, from_id, preview) = telegram_update_summary(&body);
    println!(
        "[telegram] Processing update: update_id={:?}, kind={}, chat_id={}, from_id={}, preview={}",
        update_id, kind, chat_id, from_id, preview
    );
    let action = match parse_update_with_resolver(&body, resolve_registry_command_name) {
        Ok(action) => action,
        Err(err) => {
            println!("[telegram] Failed to parse update: {err}");
            warn!("Telegram update: failed to parse update: {err}");
            return;
        }
    };

    match action {
        ChannelAction::StartThread {
            channel: channel_id,
            prompt,
        } => {
            println!("[telegram] Inbound message: chat_id={channel_id}, prompt={prompt}");
            info!(chat_id = %channel_id, "Telegram: starting thread with prompt: {prompt}");
            if runtime::should_drop_duplicate(update_id.clone()).await {
                println!(
                    "[telegram] Duplicate update ignored before thread start: update_id={:?}",
                    update_id
                );
                return;
            }

            let meta = to_runtime_start_meta(parse_start_meta(&body));
            let name = parse_display_name(&body);
            println!(
                "[telegram] StartThread accepted: update_id={:?}, channel_id={}, peer_id={:?}, thread_id={:?}, reply_target={:?}, sender_name={:?}",
                update_id, channel_id, meta.peer_id, meta.thread_id, meta.reply_target, name
            );

            tokio::spawn(async move {
                println!(
                    "[telegram] Spawning runtime pipeline: channel_id={}, thread_id={:?}, reply_target={:?}",
                    channel_id, meta.thread_id, meta.reply_target
                );
                runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                    gateway_channel,
                    session_store,
                    "telegram",
                    channel_id,
                    prompt,
                    name,
                    Some(meta),
                )
                .await;
            });
        }
        ChannelAction::Approve {
            thread_id,
            decision,
        } => {
            println!("[telegram] Approval response: thread_id={thread_id}, decision={decision}");
            info!(thread_id = %thread_id, decision = %decision, "Telegram: approval response");
        }
        ChannelAction::Ignore => {
            println!("[telegram] Update ignored (no actionable content)");
        }
        ChannelAction::SendToThread { .. } => {
            println!("[telegram] SendToThread action received");
        }
    }
}

struct TelegramRuntimeSink {
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
}

#[async_trait]
impl TelegramUpdateSink for TelegramRuntimeSink {
    async fn handle_update(&self, payload: Value) {
        process_update_payload(
            Arc::clone(&self.channel),
            Arc::clone(&self.session_store),
            payload,
        )
        .await;
    }
}

pub(crate) fn telegram_sink(
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) -> Arc<dyn TelegramUpdateSink> {
    println!("[telegram] Creating runtime sink for polling updates");
    Arc::new(TelegramRuntimeSink {
        channel,
        session_store,
    })
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    println!("[telegram] Webhook request received");

    let expected_secret = depot
        .obtain::<Arc<GatewayConfig>>()
        .ok()
        .and_then(|cfg| {
            cfg.channels
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
            println!("[telegram] Webhook secret verification FAILED");
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "Telegram webhook secret token verification failed",
            );
            return;
        }
        println!("[telegram] Webhook secret verified OK");
    }

    let Some(body) = super::parse_json_body(req, res, "telegram").await else {
        return;
    };

    println!(
        "[telegram] Received webhook update payload: {}",
        telegram_log_preview(&body.to_string(), 220)
    );
    let Some((gateway_channel, session_store)) = super::obtain_channel_and_store(depot, res) else {
        return;
    };
    process_update_payload(gateway_channel, session_store, body).await;

    res.status_code(StatusCode::OK);
}
