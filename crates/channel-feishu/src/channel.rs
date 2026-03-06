use async_trait::async_trait;
use feishu_sdk::event::models::MessageEvent as FeishuMessageEvent;
use savfox_core::channel::{Channel, ChannelAction, RichMessage};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::config::{FeishuChannelConfig, fetch_feishu_tenant_access_token};
use crate::parse::extract_bridge_action;

#[derive(Debug)]
pub struct FeishuChannel {
    config: FeishuChannelConfig,
    http_client: reqwest::Client,
}

impl FeishuChannel {
    #[must_use]
    pub fn new(config: FeishuChannelConfig, http_client: reqwest::Client) -> Self {
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

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        if payload.get("type").and_then(Value::as_str) == Some("url_verification") {
            return Ok(ChannelAction::Ignore);
        }

        let event = payload.get("event").cloned().unwrap_or(Value::Null);
        let message_event: FeishuMessageEvent = match serde_json::from_value(event) {
            Ok(event) => event,
            Err(_) => return Ok(ChannelAction::Ignore),
        };
        Ok(extract_bridge_action(&message_event).unwrap_or(ChannelAction::Ignore))
    }
}
