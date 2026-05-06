use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{error, info, warn};

use super::{Channel, RichMessage};
use crate::protocol::ChannelAction;

/// NextCloud Talk channel.
///
/// Connects to a NextCloud instance via the Talk API (OCS).
/// Uses polling to check for new messages.
#[derive(Debug)]
pub(crate) struct NextCloudChannel {
    config: NextCloudChannelConfig,
    http: reqwest::Client,
    last_known_message_id: std::sync::atomic::AtomicU64,
}

#[derive(Debug, Clone)]
pub(crate) struct NextCloudChannelConfig {
    /// NextCloud server URL (e.g., "https://cloud.example.com").
    pub(crate) server_url: String,
    /// Username for authentication.
    pub(crate) username: String,
    /// App password or regular password.
    pub(crate) password: String,
    /// Rooms/conversations to monitor (by token).
    pub(crate) rooms: Vec<String>,
    /// Polling interval in seconds.
    pub(crate) poll_interval_secs: u64,
}

impl NextCloudChannel {
    pub(crate) fn new(config: NextCloudChannelConfig) -> Self {
        // Use the SSRF-aware client builder so the channel inherits the
        // standard timeout and no-auto-redirect policy. The full URL pin
        // is per-call (build_pinned_client) — left for follow-up since
        // this channel keeps a single client across many polls.
        let cfg = crate::ssrf::SsrfConfig::from_env();
        let http =
            crate::ssrf::build_guarded_client(&cfg).unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            config,
            last_known_message_id: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Build the base URL for the Talk API.
    fn api_url(&self, path: &str) -> String {
        format!(
            "{}/ocs/v2.php/apps/spreed/api/v4{}",
            self.config.server_url.trim_end_matches('/'),
            path
        )
    }

    /// Send a request with NextCloud auth headers.
    async fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .basic_auth(&self.config.username, Some(&self.config.password))
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
    }

    /// Fetch messages from a room.
    async fn fetch_messages(
        &self,
        room_token: &str,
        last_known: u64,
    ) -> anyhow::Result<Vec<Value>> {
        let url = self.api_url(&format!("/room/{room_token}/message"));
        let resp = self
            .request(reqwest::Method::GET, &url)
            .await
            .query(&[
                ("lookIntoFuture", "1"),
                ("lastKnownMessageId", &last_known.to_string()),
                ("limit", "100"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("NextCloud API error {status}: {body}");
        }

        let body: Value = resp.json().await?;
        let messages = body["ocs"]["data"].as_array().cloned().unwrap_or_default();

        Ok(messages)
    }

    /// Send a message to a room.
    async fn send_to_room(&self, room_token: &str, message: &str) -> anyhow::Result<()> {
        let url = self.api_url(&format!("/room/{room_token}/message"));
        let resp = self
            .request(reqwest::Method::POST, &url)
            .await
            .json(&json!({
                "message": message,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("Failed to send NextCloud message: {status} {body}");
            anyhow::bail!("Failed to send message: {status}");
        }

        Ok(())
    }

    /// Parse a NextCloud Talk message into a ChannelAction.
    fn parse_message(msg: &Value) -> Option<ChannelAction> {
        let message_type = msg["messageType"].as_str().unwrap_or("comment");
        if message_type != "comment" {
            return None; // Skip system messages
        }

        let text = msg["message"].as_str().unwrap_or("");
        if text.is_empty() {
            return None;
        }

        let actor_id = msg["actorId"].as_str().unwrap_or("unknown");
        let room_token = msg["token"].as_str().unwrap_or("unknown");

        Some(ChannelAction::StartThread {
            channel: format!("nextcloud:{room_token}:{actor_id}"),
            prompt: text.to_owned(),
        })
    }
}

impl std::fmt::Display for NextCloudChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NextCloudChannel({})", self.config.server_url)
    }
}

#[async_trait]
impl Channel for NextCloudChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!(
            "NextCloud Talk channel starting for {} ({} rooms)",
            self.config.server_url,
            self.config.rooms.len()
        );

        // Verify connectivity by listing rooms
        let url = self.api_url("/room");
        let resp = self
            .request(reqwest::Method::GET, &url)
            .await
            .send()
            .await?;

        if resp.status().is_success() {
            info!("NextCloud Talk channel connected successfully");
        } else {
            let status = resp.status();
            warn!("NextCloud Talk channel connection check returned {status}");
        }

        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        // Channel format: "nextcloud:<room_token>:<actor_id>" or just room token
        let room_token = if channel.starts_with("nextcloud:") {
            channel.split(':').nth(1).unwrap_or(channel)
        } else {
            channel
        };

        self.send_to_room(room_token, message).await
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let mut formatted = String::new();

        if let Some(title) = &msg.title {
            formatted.push_str(&format!("**{title}**\n\n"));
        }

        formatted.push_str(&msg.text);

        for block in &msg.code_blocks {
            formatted.push_str(&format!("\n```{}\n{}\n```", block.language, block.content));
        }

        self.send_message(channel, &formatted).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        // NextCloud Talk can send webhook notifications for new messages
        if let Some(action) = Self::parse_message(&payload) {
            Ok(action)
        } else {
            Ok(ChannelAction::Ignore)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_comment_message() {
        let msg = json!({
            "id": 42,
            "token": "abc123",
            "actorId": "user1",
            "actorType": "users",
            "message": "Hello bot!",
            "messageType": "comment",
        });

        let action = NextCloudChannel::parse_message(&msg);
        assert!(action.is_some());
        match action.unwrap() {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "nextcloud:abc123:user1");
                assert_eq!(prompt, "Hello bot!");
            }
            _ => panic!("Expected StartThread"),
        }
    }

    #[test]
    fn test_skip_system_message() {
        let msg = json!({
            "id": 43,
            "token": "abc123",
            "actorId": "system",
            "message": "User joined",
            "messageType": "system",
        });

        assert!(NextCloudChannel::parse_message(&msg).is_none());
    }

    #[test]
    fn test_skip_empty_message() {
        let msg = json!({
            "id": 44,
            "token": "abc123",
            "actorId": "user1",
            "message": "",
            "messageType": "comment",
        });

        assert!(NextCloudChannel::parse_message(&msg).is_none());
    }
}
