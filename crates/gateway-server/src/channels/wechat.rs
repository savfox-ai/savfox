use salvo::prelude::*;
use savfox_channels::wechat::{parse_start_meta, parse_webhook_payload};
use serde_json::json;
use tracing::info;

use super::{ensure_inbound_channel_enabled, runtime};
use crate::protocol::ChannelAction;

fn to_runtime_start_meta(
    meta: savfox_channels::wechat::WeChatStartMeta,
) -> runtime::StartThreadMeta {
    runtime::StartThreadMeta {
        peer_id: meta.user_id,
        group_id: meta.room_id,
        reply_target: meta.message_id.clone(),
        thread_id: meta.message_id.clone(),
        parent_thread_id: meta.message_id,
        chat_type: meta.chat_type,
        ..runtime::StartThreadMeta::default()
    }
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !ensure_inbound_channel_enabled(depot, res, "wechat").await {
        return;
    }

    let Some(body) = super::parse_json_body(req, res, "wechat").await else {
        return;
    };

    let verify_token = {
        let savfox_home = depot
            .get_typed::<std::sync::Arc<crate::channel::GatewayChannel>>()
            .ok()
            .map(|channel| channel.config().savfox_home.clone());
        if let Some(savfox_home) = savfox_home {
            savfox_channels::wechat::resolve_wechat_verify_token(&savfox_home)
                .await
                .ok()
                .flatten()
                .or_else(|| std::env::var("WECHAT_VERIFY_TOKEN").ok())
        } else {
            std::env::var("WECHAT_VERIFY_TOKEN").ok()
        }
    };
    if let Some(expected) = verify_token {
        let provided = req
            .headers()
            .get("x-wechat-token")
            .and_then(|v| v.to_str().ok())
            .or_else(|| body.get("token").and_then(|v| v.as_str()))
            .unwrap_or_default();
        if provided != expected {
            super::render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "WeChat webhook token verification failed",
            );
            return;
        }
    }

    let meta = parse_start_meta(&body);
    let dedupe_key = meta.message_id.as_deref().map(|id| format!("wechat:{id}"));
    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if let ChannelAction::StartThread { channel, prompt } = parse_webhook_payload(&body)
        && !channel.is_empty()
        && !prompt.is_empty()
    {
        let Some((gateway_channel, session_store)) = super::obtain_channel_and_store(depot, res)
        else {
            return;
        };
        let name = meta.sender_name.clone();
        let runtime_meta = to_runtime_start_meta(meta);
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                gateway_channel,
                session_store,
                "wechat",
                channel,
                prompt,
                name,
                Some(runtime_meta),
            )
            .await;
        });
    }

    info!("WeChat webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({ "status": "ok" })));
}
