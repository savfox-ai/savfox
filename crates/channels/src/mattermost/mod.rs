pub mod client;
pub mod config;

use async_trait::async_trait;
use savfox_core::channel::{Channel, ChannelAction};
use serde_json::{Value, json};
use tracing::info;

use crate::http::warn_on_error;

pub use config::{
    MattermostChannelConfig, resolve_mattermost_outbound_config,
    resolve_mattermost_verification_token,
};

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

/// Metadata extracted from a Mattermost webhook payload for thread routing.
#[derive(Debug, Clone, Default)]
pub struct MattermostStartMeta {
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub channel_id: Option<String>,
    pub root_id: Option<String>,
    pub post_id: Option<String>,
}

/// Extract thread-routing metadata from the webhook payload.
pub fn parse_start_meta(payload: &Value) -> MattermostStartMeta {
    MattermostStartMeta {
        user_id: payload
            .get("user_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        username: payload
            .get("user_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        channel_id: payload
            .get("channel_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        root_id: payload
            .get("root_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        post_id: payload
            .get("post_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
    }
}

pub fn parse_webhook_payload(payload: &Value) -> ChannelAction {
    let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
    let channel_id = payload
        .get("channel_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    // Support /savfox and !savfox command prefixes.
    let prompt = text
        .strip_prefix("/savfox ")
        .or_else(|| text.strip_prefix("!savfox "));

    if let Some(prompt) = prompt {
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
        info!(server = %self.server_url, "Mattermost channel starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        self.send_message_with_root(channel, message, None).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        Ok(parse_webhook_payload(&payload))
    }
}

impl MattermostChannel {
    /// Send a message, optionally as a reply in a thread.
    pub async fn send_message_with_root(
        &self,
        channel: &str,
        message: &str,
        root_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let url = format!("{}/api/v4/posts", self.server_url);
        let mut body = json!({ "channel_id": channel, "message": message });
        if let Some(root) = root_id.filter(|s| !s.is_empty()) {
            body["root_id"] = json!(root);
        }
        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;
        warn_on_error(response, "Mattermost send error").await;
        Ok(())
    }
}
