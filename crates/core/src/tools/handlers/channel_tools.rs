use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

/// Unified multi-platform channel tool that routes actions to the correct
/// platform (Discord, Slack, Telegram, WhatsApp, etc.) based on a `platform`
/// field, providing a single entry point for cross-platform messaging.
///
/// This mirrors OpenClaw's `channel-tools.ts` — agents can send messages,
/// reactions, and manage channels across platforms using a consistent interface.
pub struct ChannelToolsHandler;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct ChannelToolsArgs {
    /// Action: "send", "react", "edit", "delete", "history", "list_channels".
    action: String,
    /// Target platform: "discord", "slack", "telegram", "whatsapp", "webhook".
    platform: String,
    /// Channel/chat identifier.
    #[serde(default)]
    channel_id: Option<String>,
    /// Message content.
    #[serde(default)]
    content: Option<String>,
    /// Message ID (for edit/delete/react).
    #[serde(default)]
    message_id: Option<String>,
    /// Emoji for reactions.
    #[serde(default)]
    emoji: Option<String>,
    /// Session/reply-to ID.
    #[serde(default)]
    session_id: Option<String>,
    /// Maximum messages to fetch for history.
    #[serde(default)]
    limit: Option<u32>,
}

#[async_trait]
impl ToolHandler for ChannelToolsHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "channel_tools requires function payload".to_string(),
                ));
            }
        };

        let args: ChannelToolsArgs = parse_arguments(&arguments)?;

        // Route to the gateway's message API for platform-specific dispatch.
        let gateway_url = std::env::var("SAVFOX_GATEWAY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18881".to_string());

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        match args.action.as_str() {
            "send" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let content = require_field(&args.content, "content")?;

                // Format as "platform:channel_id" for the gateway's message endpoint.
                let channel = format!("{}:{}", args.platform, channel_id);
                let body = serde_json::json!({
                    "channel": channel,
                    "text": content,
                });
                if let Some(session_id) = &args.session_id {
                    let mut body_map: serde_json::Map<String, serde_json::Value> =
                        serde_json::from_value(body).unwrap_or_default();
                    body_map.insert(
                        "session_id".to_string(),
                        serde_json::Value::String(session_id.clone()),
                    );
                    send_post(
                        &client,
                        &gateway_url,
                        "/api/message",
                        serde_json::Value::Object(body_map),
                    )
                    .await
                } else {
                    send_post(&client, &gateway_url, "/api/message", body).await
                }
            }
            "react" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let emoji = require_field(&args.emoji, "emoji")?;

                let body = serde_json::json!({
                    "platform": args.platform,
                    "action": "react",
                    "channel_id": channel_id,
                    "message_id": message_id,
                    "emoji": emoji,
                });
                send_post(&client, &gateway_url, "/api/channel/action", body).await
            }
            "edit" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let content = require_field(&args.content, "content")?;

                let body = serde_json::json!({
                    "platform": args.platform,
                    "action": "edit",
                    "channel_id": channel_id,
                    "message_id": message_id,
                    "content": content,
                });
                send_post(&client, &gateway_url, "/api/channel/action", body).await
            }
            "delete" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;

                let body = serde_json::json!({
                    "platform": args.platform,
                    "action": "delete",
                    "channel_id": channel_id,
                    "message_id": message_id,
                });
                send_post(&client, &gateway_url, "/api/channel/action", body).await
            }
            "history" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let limit = args.limit.unwrap_or(50);

                let body = serde_json::json!({
                    "platform": args.platform,
                    "action": "history",
                    "channel_id": channel_id,
                    "limit": limit,
                });
                send_post(&client, &gateway_url, "/api/channel/action", body).await
            }
            "list_channels" => {
                let body = serde_json::json!({
                    "platform": args.platform,
                    "action": "list_channels",
                });
                send_post(&client, &gateway_url, "/api/channel/action", body).await
            }
            other => Err(FunctionCallError::RespondToModel(format!(
                "unknown channel_tools action: {other}. Valid actions: send, react, \
                 edit, delete, history, list_channels"
            ))),
        }
    }
}

fn require_field<'a>(field: &'a Option<String>, name: &str) -> Result<&'a str, FunctionCallError> {
    field
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| FunctionCallError::RespondToModel(format!("missing required field: {name}")))
}

async fn send_post(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<ToolOutput, FunctionCallError> {
    let url = format!("{base_url}{path}");
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("channel request failed: {err}"))
        })?;

    let status = response.status();
    let body_bytes = response.bytes().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read channel response: {err}"))
    })?;
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "channel action returned HTTP {status}: {body_text}"
        )));
    }

    Ok(ToolOutput::Function {
        content: body_text,
        content_items: None,
        success: Some(true),
    })
}
