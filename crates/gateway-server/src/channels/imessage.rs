use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{debug, error, info, warn};

use super::{Channel, RichMessage, runtime};
use crate::bridge::GatewayBridge;
use crate::config::IMessageChannelConfig;
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

/// Default polling interval for fetching new messages (in seconds).
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;

/// iMessage bridge using the BlueBubbles REST API.
///
/// BlueBubbles (<https://bluebubbles.app>) exposes a local REST API on macOS that
/// provides access to iMessage. This bridge polls for new messages and sends
/// replies through that API.
pub(crate) struct IMessageChannel {
    config: IMessageChannelConfig,
    http_client: reqwest::Client,
    bridge: Arc<GatewayBridge>,
    session_store: Arc<SessionStore>,
    /// Tracks the most recent message timestamp (epoch ms) to avoid re-processing.
    last_message_ts: Arc<AtomicI64>,
    /// Signal to stop the polling loop on shutdown.
    running: Arc<AtomicBool>,
}

impl fmt::Debug for IMessageChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IMessageChannel")
            .field("config", &self.config)
            .field("last_message_ts", &self.last_message_ts)
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

impl IMessageChannel {
    #[must_use]
    pub(crate) fn new(
        config: IMessageChannelConfig,
        http_client: reqwest::Client,
        bridge: Arc<GatewayBridge>,
        session_store: Arc<SessionStore>,
    ) -> Self {
        Self {
            config,
            http_client,
            bridge,
            session_store,
            last_message_ts: Arc::new(AtomicI64::new(0)),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Build a URL with the password query parameter appended.
    fn api_url(&self, path: &str) -> String {
        let base = self.config.api_url.trim_end_matches('/');
        format!("{base}{path}?password={}", self.config.password)
    }

    /// Fetch recent messages from BlueBubbles, returning the raw JSON array.
    async fn fetch_messages(&self, after: i64) -> anyhow::Result<Vec<Value>> {
        let url = self.api_url("/api/v1/message");
        let response = self
            .http_client
            .get(&url)
            .query(&[
                ("after", after.to_string()),
                ("sort", "ASC".to_string()),
                ("limit", "50".to_string()),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("BlueBubbles GET /api/v1/message error: HTTP {status}: {body}");
        }

        let body: Value = response.json().await?;
        let messages = body
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(messages)
    }

    /// Parse a BlueBubbles message object into a `ChannelAction`.
    fn parse_message(message: &Value) -> anyhow::Result<ChannelAction> {
        // Skip messages sent by us (isFromMe == true).
        let is_from_me = message
            .get("isFromMe")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_from_me {
            return Ok(ChannelAction::Ignore);
        }

        let text = message.get("text").and_then(|t| t.as_str()).unwrap_or("");

        if text.is_empty() {
            return Ok(ChannelAction::Ignore);
        }

        // Use the chat GUID as the channel identifier (e.g. "iMessage;-;+1234567890").
        let channel = message
            .get("chats")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|chat| chat.get("chatIdentifier"))
            .and_then(|id| id.as_str())
            .unwrap_or_default();

        if channel.is_empty() {
            return Ok(ChannelAction::Ignore);
        }

        Ok(ChannelAction::StartThread {
            channel: channel.to_owned(),
            prompt: text.to_owned(),
        })
    }

    /// Spawn the polling loop that checks BlueBubbles for new messages.
    fn spawn_poll_loop(&self) {
        let http_client = self.http_client.clone();
        let bridge = self.bridge.clone();
        let session_store = self.session_store.clone();
        let last_ts = self.last_message_ts.clone();
        let running = self.running.clone();
        let api_url = self.config.api_url.clone();
        let password = self.config.password.clone();
        let interval_secs = self
            .config
            .poll_interval_secs
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECS);

        tokio::spawn(async move {
            let poll_interval = std::time::Duration::from_secs(interval_secs);
            info!(
                interval_secs = interval_secs,
                "iMessage: polling loop started"
            );

            while running.load(Ordering::Relaxed) {
                tokio::time::sleep(poll_interval).await;

                if !running.load(Ordering::Relaxed) {
                    break;
                }

                let after = last_ts.load(Ordering::Relaxed);
                let base = api_url.trim_end_matches('/');
                let url = format!("{base}/api/v1/message?password={password}");

                let response = match http_client
                    .get(&url)
                    .query(&[
                        ("after", after.to_string()),
                        ("sort", "ASC".to_string()),
                        ("limit", "50".to_string()),
                    ])
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(err) => {
                        warn!("iMessage poll: request failed: {err}");
                        continue;
                    }
                };

                if !response.status().is_success() {
                    let status = response.status();
                    warn!("iMessage poll: HTTP {status}");
                    continue;
                }

                let body: Value = match response.json().await {
                    Ok(v) => v,
                    Err(err) => {
                        warn!("iMessage poll: failed to parse response: {err}");
                        continue;
                    }
                };

                let messages = body
                    .get("data")
                    .and_then(|d| d.as_array())
                    .cloned()
                    .unwrap_or_default();

                debug!(count = messages.len(), "iMessage poll: fetched messages");

                for msg in &messages {
                    // Update the high-water mark for the next poll cycle.
                    if let Some(ts) = msg.get("dateCreated").and_then(|d| d.as_i64()) {
                        let current = last_ts.load(Ordering::Relaxed);
                        if ts > current {
                            last_ts.store(ts, Ordering::Relaxed);
                        }
                    }

                    let action = match Self::parse_message(msg) {
                        Ok(a) => a,
                        Err(err) => {
                            warn!("iMessage poll: failed to parse message: {err}");
                            continue;
                        }
                    };

                    if let ChannelAction::StartThread { channel, prompt } = action {
                        let guid = msg
                            .get("guid")
                            .and_then(|g| g.as_str())
                            .map(|g| format!("imessage:{g}"));

                        if runtime::should_drop_duplicate(guid).await {
                            continue;
                        }

                        info!(channel = %channel, "iMessage: starting thread from polled message");

                        let bridge = bridge.clone();
                        let session_store = session_store.clone();
                        tokio::spawn(async move {
                            runtime::spawn_start_thread_pipeline(
                                bridge,
                                session_store,
                                "imessage",
                                channel,
                                prompt,
                                None,
                            )
                            .await;
                        });
                    }
                }
            }

            info!("iMessage: polling loop stopped");
        });
    }
}

#[async_trait]
impl Channel for IMessageChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!(
            api_url = %self.config.api_url,
            "iMessage bridge initialized (BlueBubbles polling mode)"
        );

        // Seed the high-water mark with the current server time so we only process
        // messages that arrive after the bridge starts.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.last_message_ts.store(now_ms, Ordering::Relaxed);

        self.running.store(true, Ordering::Relaxed);
        self.spawn_poll_loop();

        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        let url = self.api_url("/api/v1/message/text");

        let body = json!({
            "chatGuid": channel,
            "message": message,
        });

        let response = self.http_client.post(&url).json(&body).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            error!(chat = %channel, "BlueBubbles send error: HTTP {status}: {resp_body}");
            anyhow::bail!("BlueBubbles API error: HTTP {status}");
        }

        info!(chat = %channel, "iMessage sent");
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        // iMessage does not support rich embeds; flatten to plain text with code blocks.
        let mut text = String::new();

        if let Some(title) = &msg.title {
            text.push_str(title);
            text.push_str("\n\n");
        }

        text.push_str(&msg.text);

        for block in &msg.code_blocks {
            text.push_str("\n\n");
            if !block.language.is_empty() {
                text.push_str(&format!("[{}]\n", block.language));
            }
            text.push_str(&block.content);
        }

        self.send_message(channel, &text).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction> {
        // BlueBubbles can be configured to send webhook callbacks on new messages.
        // The payload wraps the message in a "data" field for new-message events.
        let event_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "new-message" => {
                let data = payload.get("data").unwrap_or(&Value::Null);
                Self::parse_message(data)
            }
            _ => {
                debug!(event_type = %event_type, "iMessage webhook: ignoring event");
                Ok(ChannelAction::Ignore)
            }
        }
    }
}

fn render_error(res: &mut Response, status: StatusCode, code: &str, message: impl Into<String>) {
    res.status_code(status);
    res.render(Text::Json(
        json!({
            "error": {
                "code": code,
                "message": message.into(),
            }
        })
        .to_string(),
    ));
}

/// `POST /webhooks/imessage` -- Handle BlueBubbles webhook callbacks.
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let body = match req.parse_json::<Value>().await {
        Ok(v) => v,
        Err(err) => {
            warn!("iMessage webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse BlueBubbles payload: {err}"),
            );
            return;
        }
    };

    let event_type = body.get("type").and_then(|t| t.as_str()).unwrap_or("");

    // Only process new-message events.
    if event_type != "new-message" {
        debug!(event_type = %event_type, "iMessage webhook: ignoring event");
        res.status_code(StatusCode::OK);
        return;
    }

    let data = body.get("data").unwrap_or(&Value::Null);

    let action = match IMessageChannel::parse_message(data) {
        Ok(action) => action,
        Err(err) => {
            warn!("iMessage webhook: failed to parse message: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse iMessage payload: {err}"),
            );
            return;
        }
    };

    match action {
        ChannelAction::StartThread { channel, prompt } => {
            info!(chat = %channel, "iMessage: starting thread from webhook");

            let guid = data
                .get("guid")
                .and_then(|g| g.as_str())
                .map(|g| format!("imessage:{g}"));

            if runtime::should_drop_duplicate(guid).await {
                res.status_code(StatusCode::OK);
                return;
            }

            let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
                Ok(bridge) => bridge.clone(),
                Err(err) => {
                    warn!("iMessage webhook: missing gateway bridge state: {err:?}");
                    render_error(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "state_unavailable",
                        "gateway bridge state unavailable",
                    );
                    return;
                }
            };
            let session_store = match depot.obtain::<Arc<SessionStore>>() {
                Ok(store) => store.clone(),
                Err(err) => {
                    warn!("iMessage webhook: missing session store: {err:?}");
                    render_error(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "state_unavailable",
                        "session store unavailable",
                    );
                    return;
                }
            };

            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline(
                    bridge,
                    session_store,
                    "imessage",
                    channel,
                    prompt,
                    None,
                )
                .await;
            });
        }
        ChannelAction::Approve {
            thread_id,
            decision,
        } => {
            info!(thread_id = %thread_id, decision = %decision, "iMessage: approval response");
        }
        ChannelAction::Ignore | ChannelAction::SendToThread { .. } => {}
    }

    res.status_code(StatusCode::OK);
}
