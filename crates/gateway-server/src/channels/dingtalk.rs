use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
pub(crate) use savfox_channels::dingtalk::{
    DingtalkActionSink, DingtalkChannelConfig, DingtalkMessageMeta, load_dingtalk_channel_config,
    parse_inbound_payload, resolve_dingtalk_outbound_config, start_dingtalk_stream,
};
use serde_json::json;
use tracing::info;

use super::{obtain_channel_and_store, parse_json_body, runtime};
use crate::channel::GatewayChannel;
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

struct DingtalkRuntimeSink {
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
}

#[async_trait]
impl DingtalkActionSink for DingtalkRuntimeSink {
    async fn handle_action(
        &self,
        action: ChannelAction,
        event_id: Option<&str>,
        message_id: Option<&str>,
        meta: DingtalkMessageMeta,
    ) {
        let dedupe_key = event_id
            .filter(|value| !value.trim().is_empty())
            .or(message_id.filter(|value| !value.trim().is_empty()))
            .map(|id| format!("dingtalk:{id}"));

        if runtime::should_drop_duplicate(dedupe_key).await {
            return;
        }

        if let ChannelAction::StartThread {
            channel: channel_id,
            prompt,
        } = action
        {
            let is_group = matches!(meta.chat_type.as_deref(), Some("group" | "chat"));
            let start_meta = runtime::StartThreadMeta {
                peer_id: meta.sender_id,
                group_id: if is_group {
                    meta.thread_id.clone()
                } else {
                    None
                },
                thread_id: meta.thread_id.clone(),
                parent_thread_id: meta.thread_id,
                reply_target: meta.reply_target.or_else(|| message_id.map(str::to_owned)),
                chat_type: meta.chat_type,
                ..runtime::StartThreadMeta::default()
            };
            let gateway_channel = Arc::clone(&self.channel);
            let session_store = Arc::clone(&self.session_store);
            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                    gateway_channel,
                    session_store,
                    "dingtalk",
                    channel_id,
                    prompt,
                    meta.sender_name,
                    Some(start_meta),
                )
                .await;
            });
        }
    }
}

pub(crate) fn dingtalk_sink(
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) -> Arc<dyn DingtalkActionSink> {
    Arc::new(DingtalkRuntimeSink {
        channel,
        session_store,
    })
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let Some(body) = parse_json_body(req, res, "dingtalk").await else {
        return;
    };

    if let Some(challenge) = body.get("challenge").and_then(|v| v.as_str()) {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "challenge": challenge })));
        return;
    }

    let parsed = parse_inbound_payload(&body);
    if runtime::should_drop_duplicate(parsed.dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if let ChannelAction::StartThread {
        channel: channel_id,
        prompt,
    } = parsed.action
    {
        let Some((gateway_channel, session_store)) = obtain_channel_and_store(depot, res) else {
            return;
        };

        let meta = parsed.meta;
        let is_group = matches!(meta.chat_type.as_deref(), Some("group" | "chat"));
        let start_meta = runtime::StartThreadMeta {
            peer_id: meta.sender_id,
            group_id: if is_group {
                meta.thread_id.clone()
            } else {
                None
            },
            thread_id: meta.thread_id.clone(),
            parent_thread_id: meta.thread_id,
            reply_target: meta.reply_target,
            chat_type: meta.chat_type,
            ..runtime::StartThreadMeta::default()
        };
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline_with_meta_coordinated(
                gateway_channel,
                session_store,
                "dingtalk",
                channel_id,
                prompt,
                meta.sender_name,
                Some(start_meta),
            )
            .await;
        });
    }

    info!("Dingtalk webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
