use async_trait::async_trait;
use savfox_core::channel::{Channel, ChannelAction, RichMessage};
use serde_json::{Value, json};
use tracing::{error, info};

use super::parse::{parse_event, parse_slash_command};

#[derive(Debug)]
pub struct SlackChannel {
    http_client: reqwest::Client,
    bot_token: String,
}

impl SlackChannel {
    #[must_use]
    pub fn new(bot_token: String, http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            bot_token,
        }
    }
}

#[async_trait]
impl Channel for SlackChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Slack channel initialized (Events API + slash commands)");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = "https://slack.com/api/chat.postMessage";
        let body = json!({
            "channel": channel,
            "text": message,
        });

        let response = self
            .http_client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(channel = %channel, "Slack API error: HTTP {status}: {resp_body}");
            anyhow::bail!("Slack API error: HTTP {status}");
        }

        info!(channel = %channel, "Slack message sent");
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let url = "https://slack.com/api/chat.postMessage";

        let mut blocks = vec![json!({
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": msg.text,
            }
        })];

        for block in &msg.code_blocks {
            blocks.push(json!({
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!("```\n{}\n```", block.content),
                }
            }));
        }

        let body = json!({
            "channel": channel,
            "text": msg.text,
            "blocks": blocks,
        });

        let response = self
            .http_client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(channel = %channel, "Slack API error: HTTP {status}: {resp_body}");
            anyhow::bail!("Slack API error: HTTP {status}");
        }

        info!(channel = %channel, "Slack rich message sent");
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        if payload.get("command").is_some() {
            parse_slash_command(&payload)
        } else {
            parse_event(&payload)
        }
    }
}
