use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use feishu_sdk::core::{
    Config as FeishuSdkConfig, Error as FeishuSdkError, FEISHU_BASE_URL, LARK_BASE_URL, LogLevel,
};
use feishu_sdk::event::models::MessageEvent as FeishuMessageEvent;
use feishu_sdk::event::{
    Event as FeishuEvent, EventDispatcher, EventDispatcherConfig,
    EventHandler as FeishuEventHandler, EventResp as FeishuEventResp,
};
use feishu_sdk::ws::{StreamClient, StreamConfig};
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{Channel, RichMessage, runtime};
use crate::bridge::GatewayBridge;
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FeishuInboundMode {
    Webhook,
    Stream,
}

#[derive(Debug, Clone)]
pub(crate) struct FeishuChannelConfig {
    pub(crate) kind: String,
    pub(crate) base_url: String,
    pub(crate) app_access_token: Option<String>,
    pub(crate) app_id: Option<String>,
    pub(crate) app_secret: Option<String>,
    pub(crate) verification_token: Option<String>,
    pub(crate) encrypt_key: Option<String>,
    pub(crate) receive_id_type: String,
    pub(crate) inbound_mode: FeishuInboundMode,
    pub(crate) stream_locale: Option<String>,
    pub(crate) stream_auto_reconnect: Option<bool>,
    pub(crate) stream_reconnect_count: Option<i32>,
    pub(crate) stream_reconnect_interval_secs: Option<u64>,
    pub(crate) stream_ping_interval_secs: Option<u64>,
}

impl FeishuChannelConfig {
    pub(crate) fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled
            || (!config.kind.eq_ignore_ascii_case("feishu")
                && !config.kind.eq_ignore_ascii_case("lark"))
        {
            return None;
        }

        let raw = config.config.as_object()?;
        let app_access_token = first_non_empty_config_string(
            raw,
            &[
                "appAccessToken",
                "app_access_token",
                "tenantAccessToken",
                "tenant_access_token",
                "access_token",
                "token",
            ],
        );
        let app_id = first_non_empty_config_string(raw, &["appId", "app_id"]);
        let app_secret = first_non_empty_config_string(raw, &["appSecret", "app_secret"]);
        let verification_token = first_non_empty_config_string(
            raw,
            &["verificationToken", "verification_token", "verify_token"],
        );
        let encrypt_key =
            first_non_empty_config_string(raw, &["encryptKey", "encrypt_key", "event_encrypt_key"]);
        let receive_id_type =
            first_non_empty_config_string(raw, &["receiveIdType", "receive_id_type", "id_type"])
                .unwrap_or_else(|| "chat_id".to_string());
        let inbound_mode = feishu_inbound_mode(raw);
        let stream_locale =
            first_non_empty_config_string(raw, &["streamLocale", "stream_locale", "locale"]);
        let stream_auto_reconnect =
            first_config_bool(raw, &["streamAutoReconnect", "stream_auto_reconnect"]);
        let stream_reconnect_count =
            first_config_i32(raw, &["streamReconnectCount", "stream_reconnect_count"]);
        let stream_reconnect_interval_secs = first_config_u64(
            raw,
            &[
                "streamReconnectIntervalSecs",
                "stream_reconnect_interval_secs",
                "streamReconnectIntervalSeconds",
            ],
        );
        let stream_ping_interval_secs = first_config_u64(
            raw,
            &[
                "streamPingIntervalSecs",
                "stream_ping_interval_secs",
                "streamPingIntervalSeconds",
            ],
        );

        Some(Self {
            kind: config.kind.clone(),
            base_url: configured_base_url(&config.kind, raw),
            app_access_token,
            app_id,
            app_secret,
            verification_token,
            encrypt_key,
            receive_id_type,
            inbound_mode,
            stream_locale,
            stream_auto_reconnect,
            stream_reconnect_count,
            stream_reconnect_interval_secs,
            stream_ping_interval_secs,
        })
    }

    fn has_outbound_auth(&self) -> bool {
        self.app_access_token
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || (self
                .app_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                && self
                    .app_secret
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()))
    }

    pub(crate) fn stream_enabled(&self) -> bool {
        self.inbound_mode == FeishuInboundMode::Stream
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FeishuOutboundConfig {
    pub(crate) base_url: String,
    pub(crate) app_access_token: Option<String>,
    pub(crate) app_id: Option<String>,
    pub(crate) app_secret: Option<String>,
    pub(crate) receive_id_type: String,
}

#[derive(Debug)]
pub(crate) struct FeishuChannel {
    config: FeishuChannelConfig,
    http_client: reqwest::Client,
}

impl FeishuChannel {
    #[must_use]
    pub(crate) fn new(config: FeishuChannelConfig, http_client: reqwest::Client) -> Self {
        Self {
            config,
            http_client,
        }
    }

    async fn resolve_access_token(&self) -> anyhow::Result<Option<String>> {
        if let Some(token) = self
            .config
            .app_access_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(Some(token.to_string()));
        }

        let app_id = self
            .config
            .app_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let app_secret = self
            .config
            .app_secret
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let (Some(app_id), Some(app_secret)) = (app_id, app_secret) else {
            return Ok(None);
        };

        fetch_feishu_tenant_access_token(
            &self.http_client,
            &self.config.base_url,
            app_id,
            app_secret,
        )
        .await
        .map(Some)
    }
}

fn default_base_url(kind: &str) -> &'static str {
    if kind.eq_ignore_ascii_case("lark") {
        LARK_BASE_URL
    } else {
        FEISHU_BASE_URL
    }
}

fn default_stream_locale(kind: &str) -> &'static str {
    if kind.eq_ignore_ascii_case("lark") {
        "en"
    } else {
        "zh"
    }
}

fn configured_base_url(kind: &str, raw: &serde_json::Map<String, Value>) -> String {
    first_non_empty_config_string(raw, &["baseUrl", "base_url", "apiBaseUrl", "api_base_url"])
        .unwrap_or_else(|| default_base_url(kind).to_string())
}

fn feishu_inbound_mode(raw: &serde_json::Map<String, Value>) -> FeishuInboundMode {
    if first_config_bool(raw, &["streamMode", "stream_mode", "stream"]).unwrap_or(false) {
        return FeishuInboundMode::Stream;
    }

    let mode = first_non_empty_config_string(
        raw,
        &[
            "eventMode",
            "event_mode",
            "connectionMode",
            "connection_mode",
            "receiveMode",
            "receive_mode",
        ],
    );
    match mode
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("stream" | "long_connection" | "long-connection" | "longconnection") => {
            FeishuInboundMode::Stream
        }
        _ => FeishuInboundMode::Webhook,
    }
}

fn first_non_empty_config_string(
    map: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            let text = value.as_str()?.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        })
    })
}

fn first_config_bool(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(serde_json::Value::as_bool))
}

fn first_config_i32(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<i32> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            value
                .as_i64()
                .and_then(|number| i32::try_from(number).ok())
                .or_else(|| value.as_str()?.trim().parse::<i32>().ok())
        })
    })
}

fn first_config_u64(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
                .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
        })
    })
}

pub(crate) async fn resolve_feishu_outbound_config(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<FeishuOutboundConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let feishu_configs: Vec<FeishuChannelConfig> = all_configs
        .iter()
        .filter_map(FeishuChannelConfig::from_channel_config)
        .filter(FeishuChannelConfig::has_outbound_auth)
        .collect();
    if feishu_configs.is_empty() {
        return Ok(None);
    }
    let pick = feishu_configs
        .iter()
        .find(|cfg| {
            cfg.app_access_token
                .as_deref()
                .is_some_and(|token| !token.trim().is_empty())
        })
        .or_else(|| {
            feishu_configs.iter().find(|cfg| {
                cfg.app_id
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                    && cfg
                        .app_secret
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
            })
        })
        .or_else(|| feishu_configs.first())
        .cloned();

    Ok(pick.map(|cfg| FeishuOutboundConfig {
        base_url: cfg.base_url,
        app_access_token: cfg.app_access_token,
        app_id: cfg.app_id,
        app_secret: cfg.app_secret,
        receive_id_type: cfg.receive_id_type,
    }))
}

pub(crate) async fn load_feishu_channel_config(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<FeishuChannelConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    Ok(all_configs
        .iter()
        .filter_map(FeishuChannelConfig::from_channel_config)
        .next())
}

pub(crate) async fn fetch_feishu_tenant_access_token(
    http_client: &reqwest::Client,
    base_url: &str,
    app_id: &str,
    app_secret: &str,
) -> anyhow::Result<String> {
    let response = http_client
        .post(format!(
            "{}/open-apis/auth/v3/tenant_access_token/internal",
            base_url.trim_end_matches('/')
        ))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "app_id": app_id,
            "app_secret": app_secret,
        }))
        .send()
        .await
        .context("failed to call Feishu tenant_access_token API")?;

    let status = response.status();
    let body = response.bytes().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "Feishu tenant access token API error: HTTP {}: {}",
            status,
            String::from_utf8_lossy(&body)
        );
    }

    let parsed: Value = serde_json::from_slice(&body)
        .context("failed to parse Feishu tenant access token response")?;
    let code = parsed.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 {
        let msg = parsed
            .get("msg")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("Feishu tenant access token API returned code {code}: {msg}");
    }
    let token = parsed
        .get("tenant_access_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Feishu tenant access token response missing token"))?;
    Ok(token.to_string())
}

fn parse_text_command(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let stripped = trimmed.strip_prefix("/savfox")?;
    if stripped
        .chars()
        .next()
        .is_some_and(|first| !first.is_whitespace())
    {
        return None;
    }
    let prompt = stripped.trim();
    if prompt.is_empty() {
        None
    } else {
        Some(prompt.to_string())
    }
}

fn extract_bridge_action(message_event: &FeishuMessageEvent) -> Option<BridgeAction> {
    let message = &message_event.message;
    if message.message_type.as_deref() != Some("text") {
        return None;
    }

    let content = message.content.as_deref().unwrap_or("{}");
    let payload: Value = serde_json::from_str(content).ok()?;
    let text = payload.get("text").and_then(Value::as_str)?;
    let prompt = parse_text_command(text)?;
    let chat_id = message.chat_id.as_deref()?.trim();
    if chat_id.is_empty() {
        return None;
    }

    Some(BridgeAction::StartThread {
        channel: chat_id.to_string(),
        prompt,
    })
}

async fn dispatch_bridge_action(
    action: BridgeAction,
    event_id: Option<&str>,
    message_id: Option<&str>,
    bridge: Arc<GatewayBridge>,
    session_store: Arc<SessionStore>,
) {
    let dedupe_key = event_id
        .filter(|value| !value.trim().is_empty())
        .or(message_id.filter(|value| !value.trim().is_empty()))
        .map(|id| format!("feishu:{id}"));

    if runtime::should_drop_duplicate(dedupe_key).await {
        return;
    }

    if let BridgeAction::StartThread { channel, prompt } = action {
        tokio::spawn(async move {
            runtime::spawn_start_thread_pipeline(
                bridge,
                session_store,
                "feishu",
                channel,
                prompt,
                None,
            )
            .await;
        });
    }
}

struct SavfoxFeishuEventHandler {
    bridge: Arc<GatewayBridge>,
    session_store: Arc<SessionStore>,
}

impl FeishuEventHandler for SavfoxFeishuEventHandler {
    fn event_type(&self) -> &str {
        "im.message.receive_v1"
    }

    fn handle(
        &self,
        event: FeishuEvent,
    ) -> Pin<Box<dyn Future<Output = Result<Option<FeishuEventResp>, FeishuSdkError>> + Send + '_>>
    {
        Box::pin(async move {
            let event_id = event.event_id().map(str::to_string);
            let payload = event.event.ok_or_else(|| {
                FeishuSdkError::InvalidEventFormat("missing event payload".to_string())
            })?;
            let message_event: FeishuMessageEvent = serde_json::from_value(payload)
                .map_err(|e| FeishuSdkError::InvalidEventFormat(e.to_string()))?;

            if let Some(action) = extract_bridge_action(&message_event) {
                dispatch_bridge_action(
                    action,
                    event_id.as_deref(),
                    message_event.message.message_id.as_deref(),
                    Arc::clone(&self.bridge),
                    Arc::clone(&self.session_store),
                )
                .await;
            }

            Ok(None)
        })
    }
}

pub(crate) async fn build_feishu_event_dispatcher(
    config: &FeishuChannelConfig,
    bridge: Arc<GatewayBridge>,
    session_store: Arc<SessionStore>,
) -> Arc<EventDispatcher> {
    let mut event_config = EventDispatcherConfig::new();
    if let Some(token) = config.verification_token.as_deref() {
        event_config = event_config.verification_token(token.to_string());
    }
    if let Some(key) = config.encrypt_key.as_deref() {
        event_config = event_config.encrypt_key(key.to_string());
    }

    let dispatcher = Arc::new(EventDispatcher::new(
        event_config,
        feishu_sdk::core::new_logger(LogLevel::Info),
    ));
    dispatcher
        .register_handler(Box::new(SavfoxFeishuEventHandler {
            bridge,
            session_store,
        }))
        .await;
    dispatcher
}

fn build_feishu_sdk_config(config: &FeishuChannelConfig) -> anyhow::Result<FeishuSdkConfig> {
    let app_id = config
        .app_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Feishu stream mode requires app_id"))?;
    let app_secret = config
        .app_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Feishu stream mode requires app_secret"))?;

    Ok(FeishuSdkConfig::builder(app_id, app_secret)
        .base_url(config.base_url.clone())
        .log_level(LogLevel::Info)
        .build())
}

pub(crate) async fn start_feishu_stream(
    channel_id: &str,
    config: &FeishuChannelConfig,
    bridge: Arc<GatewayBridge>,
    session_store: Arc<SessionStore>,
) -> anyhow::Result<()> {
    if !config.stream_enabled() {
        return Ok(());
    }

    let dispatcher = build_feishu_event_dispatcher(config, bridge, session_store).await;
    let mut stream_config = StreamConfig::new().locale(
        config
            .stream_locale
            .clone()
            .unwrap_or_else(|| default_stream_locale(&config.kind).to_string()),
    );
    if let Some(auto_reconnect) = config.stream_auto_reconnect {
        stream_config = stream_config.auto_reconnect(auto_reconnect);
    }
    if let Some(reconnect_count) = config.stream_reconnect_count {
        stream_config = stream_config.reconnect_count(reconnect_count);
    }
    if let Some(reconnect_interval_secs) = config.stream_reconnect_interval_secs {
        stream_config =
            stream_config.reconnect_interval(Duration::from_secs(reconnect_interval_secs));
    }
    if let Some(ping_interval_secs) = config.stream_ping_interval_secs {
        stream_config = stream_config.ping_interval(Duration::from_secs(ping_interval_secs));
    }

    let stream_client = StreamClient::builder(build_feishu_sdk_config(config)?)
        .event_dispatcher_ref(dispatcher)
        .stream_config(stream_config)
        .build()
        .context("failed to build Feishu stream client")?;

    let channel_id = channel_id.to_string();
    tokio::spawn(async move {
        info!(channel_id = %channel_id, "Feishu/Lark stream bridge starting");
        if let Err(err) = stream_client.start().await {
            warn!(channel_id = %channel_id, error = %err, "Feishu/Lark stream bridge stopped");
        }
    });
    Ok(())
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

async fn request_to_event_req(
    req: &mut Request,
) -> Result<feishu_sdk::event::EventReq, anyhow::Error> {
    let request_uri = req.uri().path().to_string();
    let header = header_map_to_hashmap(req.headers());
    let body = req
        .payload()
        .await
        .context("failed to read Feishu webhook payload")?
        .to_vec();

    Ok(feishu_sdk::event::EventReq {
        header,
        body,
        request_uri,
    })
}

fn feishu_event_response_to_response(event_resp: FeishuEventResp, res: &mut Response) {
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

#[async_trait]
impl Channel for FeishuChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!(
            base_url = %self.config.base_url,
            inbound_mode = ?self.config.inbound_mode,
            "Feishu/Lark bridge starting"
        );
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let Some(access_token) = self.resolve_access_token().await? else {
            anyhow::bail!(
                "Feishu credentials missing: provide app_access_token or app_id/app_secret"
            );
        };
        let url = format!(
            "{}/open-apis/im/v1/messages?receive_id_type={}",
            self.config.base_url.trim_end_matches('/'),
            self.config.receive_id_type
        );
        let body = json!({
            "receive_id": channel,
            "msg_type": "text",
            "content": json!({"text": message}).to_string(),
        });
        let response = self
            .http_client
            .post(url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Feishu send error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let mut text = msg.text.clone();
        for block in &msg.code_blocks {
            text.push_str(&format!("\n```{}\n{}\n```", block.language, block.content));
        }
        self.send_message(channel, &text).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction> {
        if payload.get("type").and_then(|t| t.as_str()) == Some("url_verification") {
            return Ok(BridgeAction::Ignore);
        }

        let event = payload.get("event").cloned().unwrap_or(Value::Null);
        let message_event: FeishuMessageEvent = match serde_json::from_value(event) {
            Ok(event) => event,
            Err(_) => return Ok(BridgeAction::Ignore),
        };
        Ok(extract_bridge_action(&message_event).unwrap_or(BridgeAction::Ignore))
    }
}

#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
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

    let config = match load_feishu_channel_config(&bridge.config().savfox_home).await {
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

    let dispatcher = build_feishu_event_dispatcher(&config, bridge, session_store).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn message_event(text: &str) -> FeishuMessageEvent {
        serde_json::from_value(json!({
            "sender": {
                "sender_id": {
                    "open_id": "ou_xxx"
                }
            },
            "message": {
                "message_id": "om_xxx",
                "chat_id": "oc_xxx",
                "message_type": "text",
                "content": serde_json::json!({"text": text}).to_string()
            }
        }))
        .expect("message event")
    }

    #[test]
    fn parse_command_accepts_trimmed_suffix() {
        assert_eq!(
            parse_text_command("/savfox   summarize this "),
            Some("summarize this".to_string())
        );
        assert_eq!(parse_text_command("/savfox"), None);
        assert_eq!(parse_text_command("/savfoxhello"), None);
    }

    #[test]
    fn extract_bridge_action_builds_thread_request() {
        let action = extract_bridge_action(&message_event("/savfox hello world"));
        match action {
            Some(BridgeAction::StartThread { channel, prompt }) => {
                assert_eq!(channel, "oc_xxx");
                assert_eq!(prompt, "hello world");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn inbound_mode_defaults_to_webhook() {
        let raw = serde_json::Map::new();
        assert_eq!(feishu_inbound_mode(&raw), FeishuInboundMode::Webhook);
    }

    #[test]
    fn inbound_mode_detects_stream_aliases() {
        let raw = serde_json::from_value::<serde_json::Map<String, Value>>(json!({
            "event_mode": "stream"
        }))
        .expect("map");
        assert_eq!(feishu_inbound_mode(&raw), FeishuInboundMode::Stream);
    }
}
