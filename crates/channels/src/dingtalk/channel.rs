use async_trait::async_trait;
use base64::Engine;
use hmac::{Hmac, Mac};
use savfox_core::channel::{Channel, ChannelAction, RichMessage};
use serde_json::{Value, json};
use sha2::Sha256;
use tracing::{info, warn};

use super::parse::parse_start_thread_action;

#[derive(Debug)]
pub struct DingtalkChannel {
    webhook_secret: Option<String>,
    access_token: Option<String>,
    http_client: reqwest::Client,
}

impl DingtalkChannel {
    #[must_use]
    pub fn new(
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

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        Ok(parse_start_thread_action(&payload))
    }
}
