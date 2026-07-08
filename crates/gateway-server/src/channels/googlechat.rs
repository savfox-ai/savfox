use salvo::prelude::*;
use serde_json::json;
use tracing::info;

use super::{ensure_inbound_channel_enabled, render_error, runtime};

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
            .get_typed::<std::sync::Arc<crate::channel::GatewayChannel>>()
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
