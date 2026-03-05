use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{Channel, RichMessage, runtime};
use crate::bridge::GatewayBridge;
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

#[derive(Debug, Clone)]
pub(crate) struct FeishuOutboundConfig {
    pub(crate) app_access_token: Option<String>,
    pub(crate) app_id: Option<String>,
    pub(crate) app_secret: Option<String>,
    pub(crate) receive_id_type: String,
}

impl FeishuOutboundConfig {
    fn from_channel_config(
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
                "tenant_access_token",
                "access_token",
                "token",
            ],
        );
        let app_id = first_non_empty_config_string(raw, &["appId", "app_id"]);
        let app_secret = first_non_empty_config_string(raw, &["appSecret", "app_secret"]);
        if app_access_token.is_none() && (app_id.is_none() || app_secret.is_none()) {
            return None;
        }

        let receive_id_type = first_non_empty_config_string(
            raw,
            &["receiveIdType", "receive_id_type", "id_type"],
        )
        .unwrap_or_else(|| "chat_id".to_string());

        Some(Self {
            app_access_token,
            app_id,
            app_secret,
            receive_id_type,
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

pub(crate) async fn resolve_feishu_outbound_config(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<FeishuOutboundConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let feishu_configs: Vec<FeishuOutboundConfig> = all_configs
        .iter()
        .filter_map(FeishuOutboundConfig::from_channel_config)
        .collect();
    if feishu_configs.is_empty() {
        return Ok(None);
    }
    if let Some(config) = feishu_configs
        .iter()
        .find(|cfg| cfg.app_access_token.as_deref().is_some_and(|token| !token.is_empty()))
    {
        return Ok(Some(config.clone()));
    }
    if let Some(config) = feishu_configs.iter().find(|cfg| {
        cfg.app_id.as_deref().is_some_and(|value| !value.is_empty())
            && cfg
                .app_secret
                .as_deref()
                .is_some_and(|value| !value.is_empty())
    }) {
        return Ok(Some(config.clone()));
    }
    Ok(feishu_configs.first().cloned())
}

pub(crate) async fn fetch_feishu_tenant_access_token(
    http_client: &reqwest::Client,
    app_id: &str,
    app_secret: &str,
) -> anyhow::Result<String> {
    let response = http_client
        .post("https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal")
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

    let parsed: Value =
        serde_json::from_slice(&body).context("failed to parse Feishu tenant access token response")?;
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

/// Feishu/Lark bot bridge using the Feishu Open Platform API.
pub(crate) struct FeishuChannel {
    app_access_token: Option<String>,
    app_id: Option<String>,
    app_secret: Option<String>,
    http_client: reqwest::Client,
}

impl FeishuChannel {
    #[must_use]
    pub(crate) fn new(
        app_access_token: Option<String>,
        app_id: Option<String>,
        app_secret: Option<String>,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            app_access_token,
            app_id,
            app_secret,
            http_client,
        }
    }

    async fn resolve_access_token(&self) -> anyhow::Result<Option<String>> {
        if let Some(token) = self
            .app_access_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(Some(token.to_string()));
        }

        let app_id = self
            .app_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let app_secret = self
            .app_secret
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let (Some(app_id), Some(app_secret)) = (app_id, app_secret) else {
            return Ok(None);
        };

        let response = self
            .http_client
            .post("https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal")
            .header("Content-Type", "application/json")
            .json(&json!({
                "app_id": app_id,
                "app_secret": app_secret,
            }))
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            anyhow::bail!(
                "Feishu tenant access token API error: HTTP {}: {}",
                status,
                String::from_utf8_lossy(&body)
            );
        }
        let body: Value = response.json().await?;
        let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        if code != 0 {
            anyhow::bail!(
                "Feishu tenant access token API failed: {}",
                body.get("msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
            );
        }
        Ok(body
            .get("tenant_access_token")
            .and_then(|v| v.as_str())
            .map(ToString::to_string))
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

#[async_trait]
impl Channel for FeishuChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Feishu/Lark bridge starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let Some(access_token) = self.resolve_access_token().await? else {
            anyhow::bail!(
                "Feishu credentials missing: provide app_access_token or app_id/app_secret"
            );
        };
        let url = "https://open.feishu.cn/open-apis/im/v1/messages?receive_id_type=chat_id";
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
        // Feishu event callback format.
        // Challenge verification: { "challenge": "...", "token": "...", "type": "url_verification"
        // }
        if payload.get("type").and_then(|t| t.as_str()) == Some("url_verification") {
            return Ok(BridgeAction::Ignore);
        }

        let event = payload.get("event").unwrap_or(&Value::Null);
        let message = event.get("message").unwrap_or(&Value::Null);
        let msg_type = message
            .get("message_type")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if msg_type != "text" {
            return Ok(BridgeAction::Ignore);
        }

        let content_str = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("{}");
        let content: Value = serde_json::from_str(content_str).unwrap_or(Value::Null);
        let text = content.get("text").and_then(|t| t.as_str()).unwrap_or("");

        let chat_id = message
            .get("chat_id")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_owned();
        let _sender = event
            .get("sender")
            .and_then(|s| s.get("sender_id"))
            .and_then(|s| s.get("open_id"))
            .and_then(|o| o.as_str())
            .unwrap_or("")
            .to_owned();

        if let Some(prompt) = text.strip_prefix("/savfox ") {
            let prompt = prompt.trim().to_owned();
            if !prompt.is_empty() {
                return Ok(BridgeAction::StartThread {
                    channel: chat_id,
                    prompt,
                });
            }
        }
        Ok(BridgeAction::Ignore)
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

    // Handle Feishu URL verification challenge.
    if body.get("type").and_then(|t| t.as_str()) == Some("url_verification") {
        let challenge = body.get("challenge").and_then(|c| c.as_str()).unwrap_or("");
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "challenge": challenge })));
        return;
    }

    let event = body.get("event").unwrap_or(&Value::Null);
    let message = event.get("message").unwrap_or(&Value::Null);
    let mut chat_id = String::new();
    let mut prompt = String::new();
    if message.get("message_type").and_then(|t| t.as_str()) == Some("text") {
        let content_str = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("{}");
        let content: Value = serde_json::from_str(content_str).unwrap_or(Value::Null);
        let text = content.get("text").and_then(|t| t.as_str()).unwrap_or("");
        if let Some(stripped) = text.strip_prefix("/savfox ").map(str::trim)
            && !stripped.is_empty()
        {
            chat_id = message
                .get("chat_id")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_owned();
            prompt = stripped.to_owned();
        }
    }

    let dedupe_key = body
        .get("header")
        .and_then(|h| h.get("event_id"))
        .and_then(|v| v.as_str())
        .or_else(|| message.get("message_id").and_then(|v| v.as_str()))
        .map(|id| format!("feishu:{id}"));

    if runtime::should_drop_duplicate(dedupe_key).await {
        res.status_code(StatusCode::OK);
        res.render(Json(json!({ "status": "duplicate_ignored" })));
        return;
    }

    if !chat_id.is_empty() && !prompt.is_empty() {
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
                "feishu",
                chat_id,
                prompt,
                None,
            )
            .await;
        });
    }

    info!("Feishu webhook received");
    res.status_code(StatusCode::OK);
    res.render(Json(json!({})));
}
