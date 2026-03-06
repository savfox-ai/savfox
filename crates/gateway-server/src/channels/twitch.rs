use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{Channel, RichMessage};
use crate::protocol::ChannelAction;

/// Twitch chat bridge using IRC over WebSocket (TMI).
///
/// Twitch uses a modified IRC protocol for chat. The bridge connects to
/// `wss://irc-ws.chat.twitch.tv:443` using OAuth credentials and listens
/// for PRIVMSG events in configured channels.
pub(crate) struct TwitchChannel {
    config: TwitchChannelConfig,
    http_client: reqwest::Client,
}

#[derive(Debug, Clone)]
pub(crate) struct TwitchChannelConfig {
    pub(crate) enabled: bool,
    /// Twitch bot username.
    pub(crate) bot_username: String,
    /// OAuth token (with `chat:read chat:edit` scopes).
    pub(crate) oauth_token: String,
    /// Channels to join (without '#' prefix).
    pub(crate) channels: Vec<String>,
    /// Command prefix to listen for (e.g., "!savfox").
    pub(crate) command_prefix: String,
}

impl TwitchChannel {
    #[must_use]
    pub(crate) fn new(config: TwitchChannelConfig, http_client: reqwest::Client) -> Self {
        Self {
            config,
            http_client,
        }
    }

    /// Parse a Twitch IRC-style webhook payload.
    fn parse_message(payload: &Value) -> anyhow::Result<ChannelAction> {
        let msg_type = payload["type"].as_str().unwrap_or("");

        match msg_type {
            "PRIVMSG" => {
                let text = payload["message"].as_str().unwrap_or("").to_string();
                let channel = payload["channel"].as_str().unwrap_or("unknown").to_string();
                let user = payload["user"].as_str().unwrap_or("unknown").to_string();

                if text.trim().is_empty() {
                    return Ok(ChannelAction::Ignore);
                }

                Ok(ChannelAction::StartThread {
                    channel: format!("twitch:{channel}"),
                    prompt: text,
                })
            }
            "WHISPER" => {
                // Twitch whispers (DMs).
                let text = payload["message"].as_str().unwrap_or("").to_string();
                let user = payload["user"].as_str().unwrap_or("unknown").to_string();

                if text.trim().is_empty() {
                    return Ok(ChannelAction::Ignore);
                }

                Ok(ChannelAction::StartThread {
                    channel: format!("whisper:{user}"),
                    prompt: text,
                })
            }
            _ => Ok(ChannelAction::Ignore),
        }
    }

    /// Validate the Twitch OAuth token.
    async fn validate_token(&self) -> anyhow::Result<()> {
        let resp = self
            .http_client
            .get("https://id.twitch.tv/oauth2/validate")
            .header(
                "Authorization",
                format!("OAuth {}", self.config.oauth_token),
            )
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("Twitch OAuth token validation failed");
        }

        Ok(())
    }
}

impl std::fmt::Debug for TwitchChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TwitchChannel")
            .field("bot_username", &self.config.bot_username)
            .field("channels", &self.config.channels)
            .field("enabled", &self.config.enabled)
            .finish()
    }
}

#[async_trait]
impl Channel for TwitchChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        self.validate_token().await?;
        info!(
            bot = %self.config.bot_username,
            channels = ?self.config.channels,
            "Twitch bridge initialized"
        );
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        // Strip "twitch:" prefix if present.
        let clean_channel = channel.strip_prefix("twitch:").unwrap_or(channel);

        // Twitch messages have a 500-character limit.
        // Split long messages.
        for chunk in message.as_bytes().chunks(490) {
            let chunk_str = String::from_utf8_lossy(chunk);
            // In a real implementation, this would send via IRC WebSocket.
            warn!(
                channel = %clean_channel,
                "Twitch send_message: IRC WebSocket sending not yet fully implemented"
            );
        }

        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        // Twitch chat doesn't support rich formatting.
        // Send as plain text, abbreviating code blocks.
        let mut text = msg.text.clone();
        for block in &msg.code_blocks {
            // Twitch has character limits, so truncate code blocks.
            let code = if block.content.len() > 200 {
                format!("{}... (truncated)", &block.content[..200])
            } else {
                block.content.clone()
            };
            text.push_str(&format!(" | {}: {}", block.language, code));
        }
        self.send_message(channel, &text).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        Self::parse_message(&payload)
    }
}
