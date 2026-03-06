use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Map, Value, json};
use tracing::{error, info, warn};

use super::{Channel, RichMessage, runtime};
use crate::auto_reply::CommandRegistry;
use crate::bridge::{GatewayBridge, is_slack_timestamp_fresh};
use crate::config::{GatewayConfig, SlackChannelConfig};
use crate::protocol::ChannelAction;
use crate::session::SessionStore;

/// Slack bridge using Events API and slash commands.
pub(crate) struct SlackChannel {
    config: SlackChannelConfig,
    http_client: reqwest::Client,
    bot_token: String,
    signing_secret: String,
}

fn split_head_and_tail(s: &str) -> (&str, &str) {
    if let Some(idx) = s.find(char::is_whitespace) {
        let (head, tail) = s.split_at(idx);
        (head, tail.trim())
    } else {
        (s, "")
    }
}

fn normalize_command_prompt(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let (head, tail) = split_head_and_tail(trimmed);
    let registry = CommandRegistry::new();
    let canonical = registry.resolve_command_name(head)?;
    let mut prompt = format!("/{canonical}");
    if !tail.is_empty() {
        prompt.push(' ');
        prompt.push_str(tail);
    }
    Some(prompt)
}

fn slash_command_prompt(command: &str, text: &str) -> Option<String> {
    let command = command.trim();
    let text = text.trim();

    if command.eq_ignore_ascii_case("/savfox") {
        if text.is_empty() {
            return None;
        }
        return Some(normalize_command_prompt(text).unwrap_or_else(|| text.to_string()));
    }

    let registry = CommandRegistry::new();
    let canonical = registry.resolve_command_name(command)?;
    let mut prompt = format!("/{canonical}");
    if !text.is_empty() {
        prompt.push(' ');
        prompt.push_str(text);
    }
    Some(prompt)
}

impl SlackChannel {
    #[must_use]
    pub(crate) fn new(config: SlackChannelConfig, http_client: reqwest::Client) -> Self {
        let bot_token = config.bot_token.clone();
        let signing_secret = config.signing_secret.clone();
        Self {
            config,
            http_client,
            bot_token,
            signing_secret,
        }
    }

    /// Verify a Slack request signature using the signing secret.
    fn verify_signature(
        signing_secret: &str,
        timestamp: &str,
        signature: &str,
        body: &[u8],
    ) -> bool {
        crate::bridge::verify_slack_signature(signing_secret, timestamp, signature, body)
    }

    /// Parse a Slack event or slash command payload into a `ChannelAction`.
    fn parse_event(payload: &Value) -> anyhow::Result<ChannelAction> {
        let event_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            // URL verification challenge.
            "url_verification" => Ok(ChannelAction::Ignore),

            // Event callback.
            "event_callback" => {
                let event = payload.get("event").unwrap_or(&Value::Null);
                let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match event_type {
                    "app_mention" | "message" => {
                        let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");

                        let channel = event
                            .get("channel")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_owned();

                        // Look for @savfox or /savfox prefix.
                        let prompt = text
                            .trim()
                            .strip_prefix("/savfox ")
                            .or_else(|| {
                                // Strip bot mention (e.g., "<@U12345> prompt")
                                text.find('>').map(|i| text[i + 1..].trim())
                            })
                            .unwrap_or("")
                            .to_owned();

                        if prompt.is_empty() {
                            Ok(ChannelAction::Ignore)
                        } else if let Some(command_prompt) = normalize_command_prompt(&prompt) {
                            Ok(ChannelAction::StartThread {
                                channel,
                                prompt: command_prompt,
                            })
                        } else {
                            Ok(ChannelAction::StartThread { channel, prompt })
                        }
                    }
                    _ => Ok(ChannelAction::Ignore),
                }
            }

            _ => Ok(ChannelAction::Ignore),
        }
    }

    /// Parse a slash command form payload.
    fn parse_slash_command(payload: &Value) -> anyhow::Result<ChannelAction> {
        let command = payload
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("");
        let text = payload.get("text").and_then(|t| t.as_str()).unwrap_or("");

        let channel = payload
            .get("channel_id")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_owned();

        if let Some(prompt) = slash_command_prompt(command, text) {
            Ok(ChannelAction::StartThread { channel, prompt })
        } else {
            Ok(ChannelAction::Ignore)
        }
    }

    fn parse_payload_bytes(body: &[u8]) -> anyhow::Result<Value> {
        if let Ok(json_value) = serde_json::from_slice::<Value>(body) {
            return Ok(json_value);
        }

        // Slack slash commands are usually x-www-form-urlencoded.
        let mut map = Map::new();
        for (key, value) in form_urlencoded::parse(body).into_owned() {
            map.insert(key, Value::String(value));
        }
        if map.is_empty() {
            anyhow::bail!("unsupported payload format");
        }
        Ok(Value::Object(map))
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

fn append_string_values(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.to_string()),
        Value::Array(items) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    out.push(s.to_string());
                }
            }
        }
        _ => {}
    }
}

fn parse_start_meta(payload: &Value) -> runtime::StartThreadMeta {
    let peer_id = payload
        .get("user_id")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/event/user").and_then(Value::as_str))
        .map(str::to_string);
    let team_id = payload
        .get("team_id")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/team/id").and_then(Value::as_str))
        .map(str::to_string);
    let account_id = payload
        .get("api_app_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let thread_id = payload
        .pointer("/event/thread_ts")
        .and_then(Value::as_str)
        .or_else(|| payload.get("thread_ts").and_then(Value::as_str))
        .map(str::to_string);
    let reply_target = payload
        .pointer("/event/ts")
        .and_then(Value::as_str)
        .map(str::to_string);
    let parent_sender_id = payload
        .pointer("/event/parent_user_id")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut slack_groups = Vec::new();
    for ptr in [
        "/event/user_groups",
        "/user_groups",
        "/event/user_profile/user_groups",
        "/user_profile/user_groups",
    ] {
        if let Some(value) = payload.pointer(ptr) {
            append_string_values(value, &mut slack_groups);
        }
    }

    let channel_id = payload
        .pointer("/event/channel")
        .and_then(Value::as_str)
        .or_else(|| payload.get("channel_id").and_then(Value::as_str))
        .map(str::to_string);
    let chat_type = channel_id
        .as_deref()
        .map(|channel| {
            if channel.starts_with('D') {
                "dm".to_string()
            } else {
                "group".to_string()
            }
        })
        .or_else(|| Some("group".to_string()));

    runtime::StartThreadMeta {
        peer_id,
        group_id: channel_id.filter(|channel| !channel.starts_with('D')),
        thread_id: thread_id.clone(),
        parent_thread_id: thread_id,
        reply_target,
        team_id,
        account_id,
        parent_sender_id,
        slack_groups,
        chat_type,
        ..runtime::StartThreadMeta::default()
    }
}

#[async_trait]
impl Channel for SlackChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Slack bridge initialized (Events API + slash commands)");
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

        // Build Slack Block Kit blocks: a section for the main text and code blocks.
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
        // Check if this is a slash command (has "command" field) or an event.
        if payload.get("command").is_some() {
            Self::parse_slash_command(&payload)
        } else {
            Self::parse_event(&payload)
        }
    }
}

/// `POST /webhooks/slack`: Handle Slack Events API and slash command webhooks.
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let raw_body = match req.payload().await {
        Ok(bytes) => bytes.clone(),
        Err(err) => {
            warn!("Slack webhook: failed to read body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_body",
                format!("failed to read request body: {err}"),
            );
            return;
        }
    };
    let body = match SlackChannel::parse_payload_bytes(raw_body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            warn!("Slack webhook: failed to parse payload: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse payload: {err}"),
            );
            return;
        }
    };

    let signing_secret = depot
        .obtain::<Arc<GatewayConfig>>()
        .ok()
        .and_then(|cfg| cfg.bridges.slack.as_ref().map(|b| b.signing_secret.clone()))
        .or_else(|| std::env::var("SLACK_SIGNING_SECRET").ok());
    if let Some(secret) = signing_secret {
        let signature = req
            .headers()
            .get("x-slack-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let timestamp = req
            .headers()
            .get("x-slack-request-timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if signature.is_empty() || timestamp.is_empty() {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "missing_signature",
                "missing Slack signature headers",
            );
            return;
        }
        let now_epoch_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if !is_slack_timestamp_fresh(timestamp, 300, now_epoch_secs) {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "stale_signature",
                "Slack request timestamp is outside the allowed replay window",
            );
            return;
        }
        if !SlackChannel::verify_signature(&secret, timestamp, signature, raw_body.as_ref()) {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "Slack request signature verification failed",
            );
            return;
        }
    }

    // Handle Slack URL verification challenge.
    if body.get("type").and_then(|t| t.as_str()) == Some("url_verification") {
        let challenge = body.get("challenge").and_then(|c| c.as_str()).unwrap_or("");
        res.render(Text::Json(json!({"challenge": challenge}).to_string()));
        return;
    }

    let action = if body.get("command").is_some() {
        SlackChannel::parse_slash_command(&body)
    } else {
        SlackChannel::parse_event(&body)
    };

    match action {
        Ok(ChannelAction::StartThread { channel, prompt }) => {
            info!(channel = %channel, "Slack: starting thread with prompt: {prompt}");
            let dedupe_key = body
                .get("event")
                .and_then(|e| e.get("event_ts"))
                .and_then(|v| v.as_str())
                .map(|id| format!("slack:{id}"))
                .or_else(|| {
                    body.get("trigger_id")
                        .and_then(|v| v.as_str())
                        .map(|id| format!("slack:{id}"))
                });
            if runtime::should_drop_duplicate(dedupe_key).await {
                res.status_code(StatusCode::OK);
                return;
            }

            let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
                Ok(bridge) => bridge.clone(),
                Err(err) => {
                    warn!("Slack webhook: missing gateway bridge state: {err:?}");
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
                    warn!("Slack webhook: missing session store state: {err:?}");
                    render_error(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "state_unavailable",
                        "session store state unavailable",
                    );
                    return;
                }
            };

            // Acknowledge immediately, process asynchronously.
            res.render(Text::Json(
                json!({
                    "response_type": "in_channel",
                    "text": "Starting Savfox agent..."
                })
                .to_string(),
            ));

            let start_meta = parse_start_meta(&body);
            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta(
                    bridge,
                    session_store,
                    "slack",
                    channel,
                    prompt,
                    None,
                    Some(start_meta),
                )
                .await;
            });
        }
        Ok(ChannelAction::Approve {
            thread_id,
            decision,
        }) => {
            info!(thread_id = %thread_id, decision = %decision, "Slack: approval response");
            res.status_code(StatusCode::OK);
        }
        Ok(ChannelAction::Ignore | ChannelAction::SendToThread { .. }) => {
            res.status_code(StatusCode::OK);
        }
        Err(err) => {
            warn!("Slack webhook parse error: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse Slack action: {err}"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::SlackChannel;
    use crate::protocol::ChannelAction;

    #[test]
    fn slash_command_supports_native_registry_command() {
        let payload = json!({
            "command": "/status",
            "channel_id": "C123",
            "text": ""
        });

        let action = SlackChannel::parse_slash_command(&payload).expect("parse should succeed");
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "C123");
                assert_eq!(prompt, "/status");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn savfox_slash_command_accepts_command_text() {
        let payload = json!({
            "command": "/savfox",
            "channel_id": "C123",
            "text": "/help"
        });

        let action = SlackChannel::parse_slash_command(&payload).expect("parse should succeed");
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "C123");
                assert_eq!(prompt, "/help");
            }
            _ => panic!("expected start thread action"),
        }
    }
}
