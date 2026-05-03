use salvo::prelude::*;
use savfox_channels::line::{parse_start_meta, parse_webhook_payload};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{ensure_inbound_channel_enabled, render_error, runtime};
use crate::protocol::ChannelAction;

fn to_runtime_start_meta(meta: savfox_channels::line::LineStartMeta) -> runtime::StartThreadMeta {
    // For LINE, thread context comes from the source: group > room > user.
    let thread_id = meta
        .group_id
        .clone()
        .or_else(|| meta.room_id.clone())
        .or_else(|| meta.user_id.clone());
    runtime::StartThreadMeta {
        peer_id: meta.user_id,
        group_id: meta.group_id,
        thread_id: thread_id.clone(),
        parent_thread_id: thread_id,
        reply_target: meta.reply_token,
        ..runtime::StartThreadMeta::default()
    }
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !ensure_inbound_channel_enabled(depot, res, "line").await {
        return;
    }

    let body_bytes = match req.payload().await {
        Ok(bytes) => bytes.to_vec(),
        Err(err) => {
            warn!("LINE webhook: failed to read body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                "failed to read request body",
            );
            return;
        }
    };

    let channel_secret = {
        let savfox_home = depot
            .obtain::<std::sync::Arc<crate::channel::GatewayChannel>>()
            .ok()
            .map(|channel| channel.config().savfox_home.clone());
        if let Some(savfox_home) = savfox_home {
            savfox_channels::line::resolve_line_channel_secret(&savfox_home)
                .await
                .ok()
                .flatten()
                .or_else(|| std::env::var("LINE_CHANNEL_SECRET").ok())
        } else {
            std::env::var("LINE_CHANNEL_SECRET").ok()
        }
    };
    if let Some(secret) = channel_secret {
        let signature = req
            .headers()
            .get("x-line-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if signature.is_empty()
            || !savfox_channels::line::client::verify_signature(&secret, &body_bytes, signature)
        {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "LINE webhook signature verification failed",
            );
            return;
        }
    }

    let body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(body) => body,
        Err(err) => {
            warn!("LINE webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                "failed to parse LINE webhook JSON",
            );
            return;
        }
    };

    if let ChannelAction::StartThread { channel, prompt } = parse_webhook_payload(&body)
        && !prompt.is_empty()
        && !channel.is_empty()
    {
        let line_meta = parse_start_meta(&body);

        let dedupe_key = line_meta
            .message_id
            .as_deref()
            .map(|id| format!("line:{id}"));

        if runtime::should_drop_duplicate(dedupe_key).await {
            res.status_code(StatusCode::OK);
            res.render(Json(json!({ "status": "duplicate_ignored" })));
            return;
        }

        let Some((gateway_channel, session_store)) = super::obtain_channel_and_store(depot, res)
        else {
            return;
        };
        let meta = to_runtime_start_meta(line_meta);
        let name = body
            .get("events")
            .and_then(|e| e.as_array())
            .and_then(|events| {
                events.iter().find_map(|event| {
                    event
                        .get("source")
                        .and_then(|s| s.get("userId"))
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                })
            });
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                gateway_channel,
                session_store,
                "line",
                channel,
                prompt,
                name,
                Some(meta),
            )
            .await;
        });
    } else {
        // No matching prompt — still check for duplicate before responding.
        let dedupe_key = body
            .get("events")
            .and_then(|e| e.as_array())
            .and_then(|events| events.first())
            .and_then(|e| e.get("message"))
            .and_then(|m| m.get("id"))
            .and_then(|v| v.as_str())
            .map(|id| format!("line:{id}"));

        if runtime::should_drop_duplicate(dedupe_key).await {
            res.status_code(StatusCode::OK);
            res.render(Json(json!({ "status": "duplicate_ignored" })));
            return;
        }
    }

    info!("LINE webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
