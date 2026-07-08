use salvo::prelude::*;
use savfox_channels::mattermost::{parse_start_meta, parse_webhook_payload};
use serde_json::json;
use tracing::info;

use super::{ensure_inbound_channel_enabled, render_error, runtime};
use crate::protocol::ChannelAction;

fn to_runtime_start_meta(
    meta: savfox_channels::mattermost::MattermostStartMeta,
) -> runtime::StartThreadMeta {
    // Use root_id as the thread context; fall back to post_id so replies
    // chain into the same thread.
    let thread_id = meta.root_id.clone().or_else(|| meta.post_id.clone());
    runtime::StartThreadMeta {
        peer_id: meta.user_id,
        thread_id: thread_id.clone(),
        parent_thread_id: thread_id,
        reply_target: meta.post_id,
        ..runtime::StartThreadMeta::default()
    }
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !ensure_inbound_channel_enabled(depot, res, "mattermost").await {
        return;
    }

    let Some(body) = super::parse_json_body(req, res, "mattermost").await else {
        return;
    };

    let verify_token = {
        let savfox_home = depot
            .get_typed::<std::sync::Arc<crate::channel::GatewayChannel>>()
            .ok()
            .map(|channel| channel.config().savfox_home.clone());
        if let Some(savfox_home) = savfox_home {
            savfox_channels::mattermost::resolve_mattermost_verification_token(&savfox_home)
                .await
                .ok()
                .flatten()
        } else {
            None
        }
    };
    if let Some(expected) = verify_token {
        let provided = body
            .get("token")
            .or_else(|| body.get("verification_token"))
            .or_else(|| body.get("verify_token"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if provided != expected {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "Mattermost webhook token verification failed",
            );
            return;
        }
    }

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
        let Some((gateway_channel, session_store)) = super::obtain_channel_and_store(depot, res)
        else {
            return;
        };
        let meta = to_runtime_start_meta(parse_start_meta(&body));
        let name = body
            .get("user_name")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                gateway_channel,
                session_store,
                "mattermost",
                channel,
                prompt,
                name,
                Some(meta),
            )
            .await;
        });
    }

    info!("Mattermost webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({ "status": "ok" })));
}
