use std::sync::Arc;

use salvo::prelude::*;
pub(crate) use savfox_channel_matrix::MatrixChannel;
use savfox_channel_matrix::parse_webhook_payload;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::runtime;
use crate::bridge::GatewayBridge;
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

    let parsed = parse_webhook_payload(&body);

    if !parsed.rooms_to_auto_join.is_empty() {
        match depot.obtain::<Arc<GatewayBridge>>() {
            Ok(bridge) => {
                let bridge = bridge.clone();
                for (room_id, invited_user_id) in parsed.rooms_to_auto_join {
                    if let Err(err) = bridge
                        .auto_join_matrix_invited_room(&room_id, invited_user_id.as_deref())
                        .await
                    {
                        warn!(
                            room_id,
                            invited_user_id = invited_user_id.as_deref().unwrap_or(""),
                            error = %err,
                            "Matrix invite auto-join failed"
                        );
                    }
                }
            }
            Err(_) => {
                warn!("Matrix invite received but gateway bridge state is unavailable");
            }
        }
    }

    if runtime::should_drop_duplicate(parsed.dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if let ChannelAction::StartThread { channel, prompt } = parsed.action {
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
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline(
                bridge,
                session_store,
                "matrix",
                channel,
                prompt,
                None,
            )
            .await;
        });
    }

    info!("Matrix webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
