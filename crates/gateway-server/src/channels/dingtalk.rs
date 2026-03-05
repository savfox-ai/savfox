use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use base64::Engine;
use hmac::{Hmac, Mac};
use salvo::prelude::*;
use serde_json::{Value, json};
use sha2::Sha256;
use tracing::{info, warn};

use super::{Channel, RichMessage, runtime};
use crate::bridge::GatewayBridge;
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

#[derive(Debug, Clone)]
pub(crate) struct DingtalkOutboundConfig {
    pub(crate) webhook_url: Option<String>,
    pub(crate) access_token: Option<String>,
    pub(crate) secret: Option<String>,
}

impl DingtalkOutboundConfig {
    fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("dingtalk") {
            return None;
        }

        let raw = config.config.as_object()?;
        let webhook_url = first_non_empty_config_string(
            raw,
            &["webhook", "webhook_url", "robot_webhook", "webhookUrl"],
        );
        let access_token = first_non_empty_config_string(
            raw,
            &["access_token", "accessToken", "token", "robot_token"],
        );
        let secret =
            first_non_empty_config_string(raw, &["secret", "sign_secret", "webhook_secret"]);

        if webhook_url.is_none() && access_token.is_none() {
            return None;
        }

        Some(Self {
            webhook_url,
            access_token,
            secret,
        })
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

pub(crate) async fn resolve_dingtalk_outbound_config(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<DingtalkOutboundConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    Ok(all_configs
        .iter()
        .filter_map(DingtalkOutboundConfig::from_channel_config)
        .next())
}

/// DingTalk chatbot/webhook channel.
pub(crate) struct DingtalkChannel {
    webhook_secret: Option<String>,
    access_token: Option<String>,
    http_client: reqwest::Client,
}

impl DingtalkChannel {
    #[must_use]
    pub(crate) fn new(
        webhook_secret: Option<String>,
        access_token: Option<String>,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            webhook_secret,
            access_token,
            http_client,
        }
    }

    fn resolve_target(&self, channel: &str) -> Option<String> {
        let channel = channel.trim();
        if channel.starts_with("https://") || channel.starts_with("http://") {
            return Some(channel.to_string());
        }
        if let Some(token) = self
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Some(format!(
                "https://oapi.dingtalk.com/robot/send?access_token={token}"
            ));
        }
        None
    }

    fn with_signature_if_needed(&self, url: &str) -> anyhow::Result<String> {
        let secret = match self
            .webhook_secret
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            Some(secret) => secret,
            None => return Ok(url.to_string()),
        };

        let timestamp = chrono::Utc::now().timestamp_millis().to_string();
        let sign_content = format!("{timestamp}\n{secret}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())?;
        mac.update(sign_content.as_bytes());
        let sign = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        let sign_encoded: String = url::form_urlencoded::byte_serialize(sign.as_bytes()).collect();
        let separator = if url.contains('?') { '&' } else { '?' };
        Ok(format!(
            "{url}{separator}timestamp={timestamp}&sign={sign_encoded}"
        ))
    }
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

fn extract_dingtalk_text(body: &Value) -> Option<String> {
    if let Some(text) = body
        .get("text")
        .and_then(|t| t.get("content"))
        .and_then(|v| v.as_str())
    {
        return Some(text.to_string());
    }
    if let Some(text) = body
        .get("text")
        .and_then(|t| t.get("text"))
        .and_then(|v| v.as_str())
    {
        return Some(text.to_string());
    }
    if let Some(text) = body.get("text").and_then(|v| v.as_str()) {
        return Some(text.to_string());
    }
    let content = body.get("content").and_then(|v| v.as_str())?;
    if let Ok(parsed) = serde_json::from_str::<Value>(content)
        && let Some(text) = parsed.get("text").and_then(|v| v.as_str())
    {
        return Some(text.to_string());
    }
    Some(content.to_string())
}

fn extract_dingtalk_channel(body: &Value) -> Option<String> {
    body.get("sessionWebhook")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            body.get("session_webhook")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string)
        })
        .or_else(|| {
            body.get("conversationId")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string)
        })
        .or_else(|| {
            body.get("conversation_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string)
        })
}

fn parse_start_thread_action(body: &Value) -> BridgeAction {
    let text = extract_dingtalk_text(body).unwrap_or_default();
    let text = text.trim();
    if text.is_empty() {
        return BridgeAction::Ignore;
    }

    let prompt = text
        .strip_prefix("/savfox ")
        .or_else(|| text.strip_prefix("!savfox "))
        .map(str::trim)
        .unwrap_or_default();
    if prompt.is_empty() {
        return BridgeAction::Ignore;
    }

    let Some(channel) = extract_dingtalk_channel(body) else {
        return BridgeAction::Ignore;
    };

    BridgeAction::StartThread {
        channel,
        prompt: prompt.to_string(),
    }
}

#[async_trait]
impl Channel for DingtalkChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Dingtalk channel starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let Some(target) = self.resolve_target(channel) else {
            anyhow::bail!("dingtalk webhook target is not configured");
        };
        let signed_url = self.with_signature_if_needed(&target)?;
        let body = json!({
            "msgtype": "text",
            "text": {
                "content": message,
            }
        });
        let response = self
            .http_client
            .post(&signed_url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Dingtalk send error: HTTP {status}: {}",
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
        Ok(parse_start_thread_action(&payload))
    }
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

    let dedupe_key = body
        .get("msgId")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("messageId").and_then(|v| v.as_str()))
        .map(|id| format!("dingtalk:{id}"));
    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if let BridgeAction::StartThread { channel, prompt } = parse_start_thread_action(&body) {
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
                "dingtalk",
                channel,
                prompt,
                None,
            )
            .await;
        });
    }

    info!("Dingtalk webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
