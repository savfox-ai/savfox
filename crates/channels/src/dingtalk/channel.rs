use async_trait::async_trait;
use base64::Engine;
use hmac::{Hmac, Mac};
use savfox_core::channel::{Channel, ChannelAction};
use serde_json::{Value, json};
use sha2::Sha256;
use tracing::info;

use super::parse::parse_inbound_payload;
use crate::http::warn_on_error;

pub async fn send_dingtalk_text_message(
    http_client: &reqwest::Client,
    channel: &str,
    webhook_secret: Option<&str>,
    access_token: Option<&str>,
    message: &str,
) -> anyhow::Result<()> {
    let channel = channel.trim();
    let Some(target) = resolve_target(channel, access_token) else {
        anyhow::bail!("dingtalk webhook target is not configured");
    };
    let signed_url = with_signature_if_needed(&target, webhook_secret)?;
    let body = json!({
        "msgtype": "text",
        "text": {
            "content": message,
        }
    });
    let response = http_client
        .post(&signed_url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    warn_on_error(response, "Dingtalk send error").await;
    Ok(())
}

fn resolve_target(channel: &str, access_token: Option<&str>) -> Option<String> {
    if channel.starts_with("https://") || channel.starts_with("http://") {
        return Some(channel.to_string());
    }
    if let Some(token) = access_token.map(str::trim).filter(|v| !v.is_empty()) {
        return Some(format!(
            "https://oapi.dingtalk.com/robot/send?access_token={token}"
        ));
    }
    None
}

fn with_signature_if_needed(url: &str, webhook_secret: Option<&str>) -> anyhow::Result<String> {
    let secret = match webhook_secret.map(str::trim).filter(|v| !v.is_empty()) {
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
}

#[async_trait]
impl Channel for DingtalkChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Dingtalk channel starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        send_dingtalk_text_message(
            &self.http_client,
            channel,
            self.webhook_secret.as_deref(),
            self.access_token.as_deref(),
            message,
        )
        .await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        Ok(parse_inbound_payload(&payload).action)
    }
}
