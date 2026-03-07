use async_trait::async_trait;
use savfox_core::channel::{Channel, ChannelAction, RichMessage};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::parse::parse_webhook_payload;

#[derive(Debug)]
pub struct MatrixChannel {
    homeserver_url: String,
    access_token: String,
    http_client: reqwest::Client,
}

impl MatrixChannel {
    #[must_use]
    pub fn new(homeserver_url: String, access_token: String, http_client: reqwest::Client) -> Self {
        Self {
            homeserver_url,
            access_token,
            http_client,
        }
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        println!(
            "Matrix channel starting with homeserver URL: {}",
            self.homeserver_url
        );
        info!(homeserver = %self.homeserver_url, "Matrix channel starting");
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let txn_id = uuid::Uuid::now_v7().to_string();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver_url, channel, txn_id
        );
        let body = json!({
            "msgtype": "m.text",
            "body": message,
        });

        let response = self
            .http_client
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Matrix send error: HTTP {status}: {}",
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

        let txn_id = uuid::Uuid::now_v7().to_string();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver_url, channel, txn_id
        );

        let html = text.replace('\n', "<br/>");
        let body = json!({
            "msgtype": "m.text",
            "body": text,
            "format": "org.matrix.custom.html",
            "formatted_body": html,
        });

        let response = self
            .http_client
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Matrix rich send error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        Ok(parse_webhook_payload(&payload).action)
    }
}
