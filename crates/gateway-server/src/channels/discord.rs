use std::sync::Arc;

use async_trait::async_trait;
use salvo::prelude::*;
use serde_json::{Value, json};
use tracing::{error, info, warn};

use super::{Channel, RichMessage, runtime};
use crate::auto_reply::CommandRegistry;
use crate::bridge::{GatewayBridge, verify_discord_signature};
use crate::config::{DiscordChannelConfig, GatewayConfig};
use crate::protocol::BridgeAction;
use crate::session::SessionStore;

/// Discord bot bridge that handles slash commands and interactions via webhooks.
pub(crate) struct DiscordChannel {
    config: DiscordChannelConfig,
    http_client: reqwest::Client,
    bot_token: String,
}

fn quote_discord_arg(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_string();
    }
    if value
        .chars()
        .all(|ch| !ch.is_whitespace() && ch != '"' && ch != '\\')
    {
        return value.to_string();
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn append_discord_option_parts(option: &Value, out: &mut Vec<String>) {
    if let Some(nested) = option.get("options").and_then(|o| o.as_array()) {
        for child in nested {
            append_discord_option_parts(child, out);
        }
    }

    let Some(name) = option.get("name").and_then(|n| n.as_str()) else {
        return;
    };
    let Some(value) = option.get("value") else {
        return;
    };
    match value {
        Value::Bool(true) => out.push(format!("--{name}")),
        Value::Bool(false) => {}
        Value::Number(n) => out.push(format!("--{name} {n}")),
        Value::String(s) => {
            let value = s.trim();
            if !value.is_empty() {
                out.push(format!("--{name} {}", quote_discord_arg(value)));
            }
        }
        _ => {}
    }
}

fn parse_savfox_prompt(data: &Value) -> Option<String> {
    let prompt = data
        .get("options")
        .and_then(|opts| opts.as_array())
        .and_then(|opts| opts.first())
        .and_then(|opt| opt.get("value"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if prompt.is_empty() {
        None
    } else {
        Some(prompt)
    }
}

fn build_registry_prompt(command_name: &str, data: &Value) -> Option<String> {
    let registry = CommandRegistry::new();
    let canonical = registry
        .resolve_command_name(command_name)
        .or_else(|| registry.resolve_command_name(&format!("/{command_name}")))?;

    let mut prompt = format!("/{canonical}");
    let mut parts = Vec::new();
    if let Some(options) = data.get("options").and_then(|opts| opts.as_array()) {
        for option in options {
            append_discord_option_parts(option, &mut parts);
        }
    }
    if !parts.is_empty() {
        prompt.push(' ');
        prompt.push_str(&parts.join(" "));
    }

    Some(prompt)
}

impl DiscordChannel {
    #[must_use]
    pub(crate) fn new(config: DiscordChannelConfig, http_client: reqwest::Client) -> Self {
        let bot_token = config.bot_token.clone();
        Self {
            config,
            http_client,
            bot_token,
        }
    }

    /// Parse a Discord interaction payload into a `BridgeAction`.
    fn parse_interaction(payload: &Value) -> anyhow::Result<BridgeAction> {
        let interaction_type = payload.get("type").and_then(|t| t.as_u64()).unwrap_or(0);

        match interaction_type {
            // Type 1: PING - Discord verification handshake.
            1 => Ok(BridgeAction::Ignore),

            // Type 2: APPLICATION_COMMAND - Slash command invocation.
            2 => {
                let data = payload.get("data").unwrap_or(&Value::Null);
                let command_name = data.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let channel = payload
                    .get("channel_id")
                    .and_then(|c| c.as_str())
                    .unwrap_or("unknown")
                    .to_owned();

                if command_name.eq_ignore_ascii_case("savfox") {
                    if let Some(prompt) = parse_savfox_prompt(data) {
                        return Ok(BridgeAction::StartThread { channel, prompt });
                    }
                    return Ok(BridgeAction::Ignore);
                }

                if let Some(prompt) = build_registry_prompt(command_name, data) {
                    return Ok(BridgeAction::StartThread { channel, prompt });
                }

                Ok(BridgeAction::Ignore)
            }

            // Type 3: MESSAGE_COMPONENT - Button click (for approvals).
            3 => {
                let data = payload.get("data").unwrap_or(&Value::Null);
                let custom_id = data.get("custom_id").and_then(|c| c.as_str()).unwrap_or("");

                if let Some(thread_id) = custom_id.strip_prefix("approve:") {
                    Ok(BridgeAction::Approve {
                        thread_id: thread_id.to_owned(),
                        decision: true,
                    })
                } else if let Some(thread_id) = custom_id.strip_prefix("deny:") {
                    Ok(BridgeAction::Approve {
                        thread_id: thread_id.to_owned(),
                        decision: false,
                    })
                } else {
                    Ok(BridgeAction::Ignore)
                }
            }

            _ => Ok(BridgeAction::Ignore),
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

fn parse_start_meta(payload: &Value) -> runtime::StartThreadMeta {
    let peer_id = payload
        .pointer("/member/user/id")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/user/id").and_then(Value::as_str))
        .map(str::to_string);
    let guild_id = payload
        .get("guild_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let role_ids = payload
        .pointer("/member/roles")
        .and_then(Value::as_array)
        .map(|roles| {
            roles
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let parent_thread_id = payload
        .pointer("/message/message_reference/channel_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let reply_target = payload
        .pointer("/message/message_reference/message_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let parent_sender_id = payload
        .pointer("/message/referenced_message/author/id")
        .and_then(Value::as_str)
        .map(str::to_string);

    let is_group_chat = guild_id.is_some();
    runtime::StartThreadMeta {
        peer_id,
        group_id: guild_id.clone(),
        guild_id,
        role_ids,
        parent_thread_id,
        reply_target,
        parent_sender_id,
        chat_type: Some(if is_group_chat { "group" } else { "dm" }.to_string()),
        ..runtime::StartThreadMeta::default()
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!("Discord bridge initialized (webhook mode)");
        // In webhook mode, no persistent connection is needed.
        // Slash command registration should be done separately via Discord API.
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

        // Build description with code blocks appended.
        let mut description = msg.text.clone();
        for block in &msg.code_blocks {
            description.push_str(&format!("\n```{}\n{}\n```", block.language, block.content));
        }

        // Parse color from hex string (e.g. "#5865F2") to integer, default to Discord blurple.
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

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction> {
        Self::parse_interaction(&payload)
    }
}

/// `POST /webhooks/discord`: Handle Discord interaction webhooks.
#[handler]
pub(crate) async fn webhook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let raw_body = match req.payload().await {
        Ok(bytes) => bytes.clone(),
        Err(err) => {
            warn!("Discord webhook: failed to read body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_body",
                format!("failed to read request body: {err}"),
            );
            return;
        }
    };
    let body = match serde_json::from_slice::<Value>(raw_body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            warn!("Discord webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse Discord payload: {err}"),
            );
            return;
        }
    };

    let public_key = depot
        .obtain::<Arc<GatewayConfig>>()
        .ok()
        .and_then(|cfg| {
            cfg.bridges
                .discord
                .as_ref()
                .and_then(|b| b.application_public_key.clone())
        })
        .or_else(|| std::env::var("DISCORD_APPLICATION_PUBLIC_KEY").ok());
    if let Some(public_key) = public_key {
        let signature = req
            .headers()
            .get("x-signature-ed25519")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let timestamp = req
            .headers()
            .get("x-signature-timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();

        if signature.is_empty() || timestamp.is_empty() {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "missing_signature",
                "missing Discord signature headers",
            );
            return;
        }
        if !verify_discord_signature(&public_key, signature, timestamp, raw_body.as_ref()) {
            render_error(
                res,
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "Discord request signature verification failed",
            );
            return;
        }
    }

    // Discord PING verification (type 1).
    if body.get("type").and_then(|t| t.as_u64()) == Some(1) {
        res.render(Text::Json(json!({"type": 1}).to_string()));
        return;
    }

    let action = match DiscordChannel::parse_interaction(&body) {
        Ok(action) => action,
        Err(err) => {
            warn!("Discord webhook: failed to parse interaction: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse Discord interaction: {err}"),
            );
            return;
        }
    };

    match action {
        BridgeAction::StartThread { channel, prompt } => {
            info!(channel = %channel, "Discord: starting thread with prompt: {prompt}");
            let interaction_id = body
                .get("id")
                .and_then(|v| v.as_str())
                .map(|id| format!("discord:{id}"));
            if runtime::should_drop_duplicate(interaction_id).await {
                res.render(Text::Json(json!({"type": 1}).to_string()));
                return;
            }

            let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
                Ok(bridge) => bridge.clone(),
                Err(err) => {
                    warn!("Discord webhook: missing gateway bridge state: {err:?}");
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
                    warn!("Discord webhook: missing session store state: {err:?}");
                    render_error(
                        res,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "state_unavailable",
                        "session store state unavailable",
                    );
                    return;
                }
            };

            // Respond with deferred message (type 5) while processing.
            res.render(Text::Json(json!({"type": 5}).to_string()));

            let start_meta = parse_start_meta(&body);
            tokio::spawn(async move {
                runtime::spawn_start_thread_pipeline_with_meta(
                    bridge,
                    session_store,
                    "discord",
                    channel,
                    prompt,
                    None,
                    Some(start_meta),
                )
                .await;
            });
        }
        BridgeAction::Approve {
            thread_id,
            decision,
        } => {
            info!(thread_id = %thread_id, decision = %decision, "Discord: approval response");
            res.render(Text::Json(
                json!({"type": 6}).to_string(), // Deferred update
            ));
        }
        BridgeAction::Ignore | BridgeAction::SendToThread { .. } => {
            res.render(Text::Json(json!({"type": 1}).to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::DiscordChannel;
    use crate::protocol::BridgeAction;

    #[test]
    fn parses_native_registry_slash_command() {
        let payload = json!({
            "type": 2,
            "channel_id": "123",
            "data": {
                "name": "status",
            }
        });

        let action = DiscordChannel::parse_interaction(&payload).expect("parse should succeed");
        match action {
            BridgeAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "123");
                assert_eq!(prompt, "/status");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn keeps_savfox_prompt_flow() {
        let payload = json!({
            "type": 2,
            "channel_id": "123",
            "data": {
                "name": "savfox",
                "options": [
                    { "name": "prompt", "value": "hello world" }
                ]
            }
        });

        let action = DiscordChannel::parse_interaction(&payload).expect("parse should succeed");
        match action {
            BridgeAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "123");
                assert_eq!(prompt, "hello world");
            }
            _ => panic!("expected start thread action"),
        }
    }
}
