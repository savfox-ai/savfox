use std::sync::Arc;

use salvo::prelude::*;
use savfox_channel_dingtalk::parse_start_thread_action;
pub(crate) use savfox_channel_dingtalk::resolve_dingtalk_outbound_config;
use serde_json::{Value, json};
use tracing::info;

use super::runtime;
use crate::channel::GatewayChannel;
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

fn render_error(res: &mut Response, status: StatusCode, code: &str, message: impl Into<String>) {
    res.status_code(status);
    res.render(Json(json!({
        "error": {
            "code": code,
            "message": message.into(),
        }
    })));
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

    if let Some(challenge) = body.get("challenge").and_then(|v| v.as_str()) {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "challenge": challenge })));
        return;
    }

    let dedupe_key = body
        .get("msgId")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("messageId").and_then(|v| v.as_str()))
        .map(|id| format!("dingtalk:{id}"));
    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if let ChannelAction::StartThread {
        channel: channel_id,
        prompt,
    } = parse_start_thread_action(&body)
    {
        let gateway_channel = match depot.obtain::<Arc<GatewayChannel>>() {
            Ok(channel) => channel.clone(),
            Err(_) => {
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
                gateway_channel,
                session_store,
                "dingtalk",
                channel_id,
                prompt,
                None,
            )
            .await;
        });
    }

    info!("Dingtalk webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
