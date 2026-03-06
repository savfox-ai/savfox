use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use feishu_sdk::core::Error as FeishuSdkError;
use feishu_sdk::event::{EventDispatcher, EventReq, EventResp};
use salvo::prelude::*;
use savfox_channel_feishu::{
    FeishuActionSink, build_feishu_event_dispatcher as build_dispatcher_inner,
    start_feishu_stream as start_stream_inner,
};
pub(crate) use savfox_channel_feishu::{
    FeishuChannel, FeishuChannelConfig, fetch_feishu_tenant_access_token,
    load_feishu_channel_config, resolve_feishu_outbound_config,
};
use serde_json::{Value, json};

use super::runtime;
use crate::channel::GatewayChannel;
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

struct FeishuRuntimeSink {
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
}

#[async_trait]
impl FeishuActionSink for FeishuRuntimeSink {
    async fn handle_action(
        &self,
        action: ChannelAction,
        event_id: Option<&str>,
        message_id: Option<&str>,
    ) {
        let dedupe_key = event_id
            .filter(|value| !value.trim().is_empty())
            .or(message_id.filter(|value| !value.trim().is_empty()))
            .map(|id| format!("feishu:{id}"));

        if runtime::should_drop_duplicate(dedupe_key).await {
            return;
        }

        if let ChannelAction::StartThread {
            channel: channel_id,
            prompt,
        } = action
        {
            let gateway_channel = Arc::clone(&self.channel);
            let session_store = Arc::clone(&self.session_store);
            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline(
                    gateway_channel,
                    session_store,
                    "feishu",
                    channel_id,
                    prompt,
                    None,
                )
                .await;
            });
        }
    }
}

fn feishu_sink(
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) -> Arc<dyn FeishuActionSink> {
    Arc::new(FeishuRuntimeSink {
        channel,
        session_store,
    })
}

async fn build_feishu_event_dispatcher(
    config: &FeishuChannelConfig,
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) -> Arc<EventDispatcher> {
    build_dispatcher_inner(config, feishu_sink(channel, session_store)).await
}

pub(crate) async fn start_feishu_stream(
    channel_id: &str,
    config: &FeishuChannelConfig,
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) -> anyhow::Result<()> {
    start_stream_inner(channel_id, config, feishu_sink(channel, session_store)).await
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

fn header_map_to_hashmap(
    headers: &salvo::http::headers::HeaderMap,
) -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    for (key, value) in headers {
        if let Ok(v) = value.to_str() {
            map.entry(key.to_string())
                .or_insert_with(Vec::new)
                .push(v.to_string());
        }
    }
    map
}

async fn request_to_event_req(req: &mut Request) -> Result<EventReq, anyhow::Error> {
    let request_uri = req.uri().path().to_string();
    let header = header_map_to_hashmap(req.headers());
    let body = req
        .payload()
        .await
        .map_err(|err| anyhow::anyhow!("failed to read Feishu webhook payload: {err}"))?
        .to_vec();

    Ok(EventReq {
        header,
        body,
        request_uri,
    })
}

fn feishu_event_response_to_response(event_resp: EventResp, res: &mut Response) {
    res.status_code(
        StatusCode::from_u16(event_resp.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
    );

    for (key, values) in event_resp.headers {
        if let Ok(header_name) = key.parse::<salvo::http::headers::HeaderName>() {
            for value in values {
                if let Ok(header_value) = value.parse() {
                    res.headers_mut().append(header_name.clone(), header_value);
                }
            }
        }
    }

    let content_type = res
        .headers()
        .get("Content-Type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_ascii_lowercase();

    if content_type.contains("application/json") {
        match serde_json::from_slice::<Value>(&event_resp.body) {
            Ok(body) => res.render(Json(body)),
            Err(_) => res.render(Text::Plain(
                String::from_utf8_lossy(&event_resp.body).into_owned(),
            )),
        }
    } else {
        res.render(Text::Plain(
            String::from_utf8_lossy(&event_resp.body).into_owned(),
        ));
    }
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
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

    let config = match load_feishu_channel_config(&channel.config().savfox_home).await {
        Ok(Some(config)) => config,
        Ok(None) => {
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "config_missing",
                "Feishu channel is not configured",
            );
            return;
        }
        Err(err) => {
            render_error(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "config_load_failed",
                format!("failed to load Feishu config: {err}"),
            );
            return;
        }
    };

    let event_req = match request_to_event_req(req).await {
        Ok(event_req) => event_req,
        Err(err) => {
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to read request body: {err}"),
            );
            return;
        }
    };

    let dispatcher = build_feishu_event_dispatcher(&config, channel, session_store).await;
    match dispatcher.dispatch(event_req).await {
        Ok(event_resp) => {
            feishu_event_response_to_response(event_resp, res);
        }
        Err(err) => {
            let status = match err {
                FeishuSdkError::EventDecryption(_)
                | FeishuSdkError::EventSignatureVerification
                | FeishuSdkError::InvalidEventFormat(_) => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            render_error(res, status, "feishu_event_error", err.to_string());
        }
    }
}
