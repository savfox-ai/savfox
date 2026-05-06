use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{error, info};

use super::{Channel, RichMessage};
use crate::protocol::ChannelAction;

/// Tlon channel for the Urbit ecosystem.
///
/// Connects to a running Urbit ship via the Eyre HTTP API
/// to send and receive messages in Tlon (formerly Landscape) channels.
#[derive(Debug)]
pub(crate) struct TlonChannel {
    config: TlonChannelConfig,
    http: reqwest::Client,
    /// Authentication cookie from Urbit.
    auth_cookie: tokio::sync::RwLock<Option<String>>,
}

#[derive(Debug, Clone)]
pub(crate) struct TlonChannelConfig {
    /// Urbit ship URL (e.g., "http://localhost:8080").
    pub(crate) ship_url: String,
    /// Access code for the ship (from +code in dojo).
    pub(crate) access_code: String,
    /// Channels to monitor (Tlon channel paths).
    pub(crate) channels: Vec<String>,
    /// Our ship name (e.g., "~zod").
    pub(crate) ship_name: String,
}

impl TlonChannel {
    pub(crate) fn new(config: TlonChannelConfig) -> Self {
        // SSRF-aware HTTP client: inherits timeout + no-auto-redirect.
        // Per-call URL pinning (`build_pinned_client`) is a follow-up
        // since the channel keeps a single client across many polls.
        let cfg = crate::ssrf::SsrfConfig::from_env();
        let http =
            crate::ssrf::build_guarded_client(&cfg).unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            config,
            auth_cookie: tokio::sync::RwLock::new(None),
        }
    }

    /// Authenticate with the Urbit ship.
    async fn authenticate(&self) -> anyhow::Result<String> {
        let url = format!("{}/~/login", self.config.ship_url);
        let body = format!("password={}", self.config.access_code);
        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("Urbit authentication failed: {}", resp.status());
        }

        // Extract the urbauth cookie
        let cookie = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v: &reqwest::header::HeaderValue| {
                let s = v.to_str().ok()?;
                if s.starts_with("urbauth") {
                    Some(s.split(';').next().unwrap_or(s).to_owned())
                } else {
                    None
                }
            })
            .next()
            .unwrap_or_default();

        Ok(cookie)
    }

    /// Send a poke (action) to the ship.
    async fn poke(&self, app: &str, mark: &str, json_data: Value) -> anyhow::Result<()> {
        let url = format!("{}/~/channel/savfox-{}", self.config.ship_url, uuid());
        let event = json!([{
            "id": 1,
            "action": "poke",
            "ship": self.config.ship_name.trim_start_matches('~'),
            "app": app,
            "mark": mark,
            "json": json_data,
        }]);

        let cookie = self.auth_cookie.read().await;
        let mut req = self.http.put(&url).json(&event);
        if let Some(c) = cookie.as_ref() {
            req = req.header("Cookie", c);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("Urbit poke failed: {status} {body}");
            anyhow::bail!("Poke failed: {status}");
        }

        Ok(())
    }

    /// Send a message to a Tlon channel.
    async fn send_to_channel(&self, channel_path: &str, content: &str) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let msg = json!({
            "add-nodes": {
                "resource": {
                    "ship": self.config.ship_name.trim_start_matches('~'),
                    "name": channel_path,
                },
                "nodes": {
                    &format!("/{now}"): {
                        "post": {
                            "author": self.config.ship_name.trim_start_matches('~'),
                            "index": format!("/{now}"),
                            "time-sent": now,
                            "contents": [{"text": content}],
                            "hash": null,
                            "signatures": [],
                        },
                        "children": null,
                    }
                }
            }
        });

        self.poke("graph-store", "graph-update-3", msg).await
    }
}

/// Generate a simple unique ID.
fn uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{ts:x}")
}

impl std::fmt::Display for TlonChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TlonChannel({} @ {})",
            self.config.ship_name, self.config.ship_url
        )
    }
}

#[async_trait]
impl Channel for TlonChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!(
            "Tlon channel starting for ship {} at {}",
            self.config.ship_name, self.config.ship_url
        );

        let cookie = self.authenticate().await?;
        *self.auth_cookie.write().await = Some(cookie);

        info!(
            "Tlon channel authenticated, monitoring {} channels",
            self.config.channels.len()
        );

        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        self.send_to_channel(channel, message).await
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        let mut formatted = String::new();

        if let Some(title) = &msg.title {
            formatted.push_str(&format!("**{title}**\n\n"));
        }

        formatted.push_str(&msg.text);

        for block in &msg.code_blocks {
            formatted.push_str(&format!("\n```\n{}\n```", block.content));
        }

        self.send_message(channel, &formatted).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        // Parse incoming Urbit graph-store update
        let nodes = payload
            .get("graph-update")
            .and_then(|u| u.get("add-nodes"))
            .and_then(|an| an.get("nodes"));

        if let Some(nodes) = nodes.and_then(|n| n.as_object()) {
            for (_index, node) in nodes {
                let contents = node
                    .get("post")
                    .and_then(|p| p.get("contents"))
                    .and_then(|c| c.as_array());

                if let Some(contents) = contents {
                    let text: String = contents
                        .iter()
                        .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join(" ");

                    if !text.is_empty() {
                        let author = node
                            .get("post")
                            .and_then(|p| p.get("author"))
                            .and_then(|a| a.as_str())
                            .unwrap_or("unknown");

                        // Skip our own messages
                        if author == self.config.ship_name.trim_start_matches('~') {
                            continue;
                        }

                        return Ok(ChannelAction::StartThread {
                            channel: format!("tlon:{author}"),
                            prompt: text,
                        });
                    }
                }
            }
        }

        Ok(ChannelAction::Ignore)
    }
}
