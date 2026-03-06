use async_trait::async_trait;
use savfox_core::channel::{Channel, ChannelAction, RichMessage};
use serde_json::{Value, json};
use tracing::{info, warn};

#[derive(Debug)]
pub struct MattermostChannel {
    server_url: String,
    access_token: String,
    http_client: reqwest::Client,
}

impl MattermostChannel {
    #[must_use]
    pub fn new(server_url: String, access_token: String, http_client: reqwest::Client) -> Self {
        Self {
            server_url,
            access_token,
            http_client,
        }
    }
}

pub fn parse_webhook_payload(payload: &Value) -> ChannelAction {
    let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
    let channel_id = payload
        .get("channel_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    if let Some(prompt) = text.strip_prefix("/savfox ") {
        let prompt = prompt.trim().to_owned();
        if !prompt.is_empty() {
            return ChannelAction::StartThread {
                channel: channel_id,
                prompt,
            };
        }
    }
    ChannelAction::Ignore
}

#[async_trait]
impl Channel for MattermostChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!(server = %self.server_url, "Mattermost bridge starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = format!("{}/api/v4/posts", self.server_url);
        let body = json!({ "channel_id": channel, "message": message });
        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Mattermost send error: HTTP {status}: {}",
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
        Ok(parse_webhook_payload(&payload))
    }
}
