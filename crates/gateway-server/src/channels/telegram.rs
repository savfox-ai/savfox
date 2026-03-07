use std::sync::Arc;

use salvo::prelude::*;
use savfox_channels::telegram::{
    TelegramStartMeta, parse_display_name, parse_start_meta, parse_update_with_resolver,
};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::runtime;
use crate::auto_reply::CommandRegistry;
use crate::channel::{GatewayChannel, verify_telegram_webhook_secret};
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

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
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

    let action = match parse_update_with_resolver(&body, resolve_registry_command_name) {
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
        ChannelAction::StartThread {
            channel: channel_id,
            prompt,
        } => {
            info!(chat_id = %channel_id, "Telegram: starting thread with prompt: {prompt}");
            let update_id = body
                .get("update_id")
                .and_then(|v| v.as_i64())
                .map(|id| format!("telegram:{id}"));
            if runtime::should_drop_duplicate(update_id).await {
                res.status_code(StatusCode::OK);
                return;
            }

            let gateway_channel = match depot.obtain::<Arc<GatewayChannel>>() {
                Ok(channel) => channel.clone(),
                Err(err) => {
                    warn!("Telegram webhook: missing gateway channel state: {err:?}");
                    render_error(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "state_unavailable",
                        "gateway channel state unavailable",
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
            let meta = to_runtime_start_meta(parse_start_meta(&body));
            let name = parse_display_name(&body);

            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta(
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
            info!(thread_id = %thread_id, decision = %decision, "Telegram: approval response");
        }
        ChannelAction::Ignore | ChannelAction::SendToThread { .. } => {}
    }

    res.status_code(StatusCode::OK);
}
