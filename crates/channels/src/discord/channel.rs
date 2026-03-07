use async_trait::async_trait;
use savfox_core::channel::{Channel, ChannelAction, RichMessage};
use serde_json::{Value, json};
use tracing::{error, info};

use super::parse::parse_interaction;

#[derive(Debug)]
pub struct DiscordChannel {
    http_client: reqwest::Client,
    bot_token: String,
}

impl DiscordChannel {
    #[must_use]
    pub fn new(bot_token: String, http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            bot_token,
        }
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Discord channel initialized (webhook mode)");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = format!("https://discord.com/api/v10/channels/{channel}/messages");
        let body = json!({ "content": message });

        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(channel = %channel, "Discord API error: HTTP {status}: {resp_body}");
            anyhow::bail!("Discord API error: HTTP {status}");
        }

        info!(channel = %channel, "Discord message sent");
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let url = format!("https://discord.com/api/v10/channels/{channel}/messages");

        let mut description = msg.text.clone();
        for block in &msg.code_blocks {
            description.push_str(&format!("\n```{}\n{}\n```", block.language, block.content));
        }

        let color: u32 = msg
            .color
            .as_deref()
            .and_then(|c| {
                let hex_str = c.strip_prefix('#').unwrap_or(c);
                u32::from_str_radix(hex_str, 16).ok()
            })
            .unwrap_or(0x5865F2);

        let body = json!({
            "embeds": [{
                "title": msg.title.as_deref().unwrap_or("Savfox"),
                "description": description,
                "color": color,
            }]
        });

        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(channel = %channel, "Discord API embed error: HTTP {status}: {resp_body}");
            anyhow::bail!("Discord API error: HTTP {status}");
        }

        info!(channel = %channel, "Discord rich message sent");
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        parse_interaction(&payload)
    }
}
