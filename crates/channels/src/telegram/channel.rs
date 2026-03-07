use async_trait::async_trait;
use savfox_core::channel::{Channel, ChannelAction, RichMessage};
use serde_json::{Value, json};
use tracing::{error, info};

use super::parse::parse_update;

#[derive(Debug)]
pub struct TelegramChannel {
    http_client: reqwest::Client,
    bot_token: String,
}

impl TelegramChannel {
    #[must_use]
    pub fn new(bot_token: String, http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            bot_token,
        }
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[async_trait]
impl Channel for TelegramChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Telegram channel initialized (webhook mode)");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let body = json!({
            "chat_id": channel,
            "text": message,
        });

        let response = self.http_client.post(&url).json(&body).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(chat_id = %channel, "Telegram API error: HTTP {status}: {resp_body}");
            anyhow::bail!("Telegram API error: HTTP {status}");
        }

        info!(chat_id = %channel, "Telegram message sent");
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);

        let mut text = escape_html(&msg.text);
        for block in &msg.code_blocks {
            text.push_str("\n<pre><code");
            let language = block.language.trim();
            if !language.is_empty() {
                text.push_str(&format!(" class=\"language-{}\"", escape_html(language)));
            }
            text.push('>');
            text.push_str(&escape_html(&block.content));
            text.push_str("</code></pre>");
        }

        let body = json!({
            "chat_id": channel,
            "text": text,
            "parse_mode": "HTML",
        });

        let response = self.http_client.post(&url).json(&body).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(chat_id = %channel, "Telegram API error: HTTP {status}: {resp_body}");
            anyhow::bail!("Telegram API error: HTTP {status}");
        }

        info!(chat_id = %channel, "Telegram rich message sent");
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        parse_update(&payload)
    }
}
