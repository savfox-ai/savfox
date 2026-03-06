use std::sync::Arc;

use salvo::prelude::*;
use savfox_channel_mattermost::parse_webhook_payload;
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

    if let ChannelAction::StartThread { prompt, .. } = parse_webhook_payload(&body)
        && !prompt.is_empty()
        && !channel.is_empty()
    {
        let channel = match depot.obtain::<Arc<GatewayChannel>>() {
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
                channel,
                session_store,
                "mattermost",
                channel,
                prompt,
                None,
            )
            .await;
        });
    }

    let _ = text;
    info!("Mattermost webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({ "status": "ok" })));
}
