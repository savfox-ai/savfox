use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
pub(crate) use savfox_channels::dingtalk::{
    DingtalkActionSink, DingtalkChannelConfig, DingtalkMessageMeta, load_dingtalk_channel_config,
    parse_inbound_payload, resolve_dingtalk_outbound_config, start_dingtalk_stream,
};
use serde_json::{Value, json};
use tracing::info;

use super::runtime;
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
                reply_target: meta.reply_target.or_else(|| message_id.map(str::to_string)),
                chat_type: meta.chat_type,
                ..runtime::StartThreadMeta::default()
            };
            let gateway_channel = Arc::clone(&self.channel);
            let session_store = Arc::clone(&self.session_store);
            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta(
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
            runtime::spawn_start_thread_pipeline_with_meta(
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
