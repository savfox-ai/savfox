use std::time::Duration;

use serde::Deserialize;

use super::{parse_arguments, require_field, reqwest_error_without_url, sanitize_error_body};
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const SLACK_API_BASE: &str = "https://slack.com/api";
const MAX_HISTORY_LIMIT: u32 = 200;

#[derive(Deserialize)]
struct SlackActionsArgs {
    action: String,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    emoji: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

pub struct SlackActionsHandler;

#[async_trait::async_trait]
impl ToolHandler for SlackActionsHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &_invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("SlackActionsHandler received unsupported payload"),
        };
        let args: SlackActionsArgs = parse_arguments(&arguments)?;

        let token = std::env::var("SLACK_BOT_TOKEN").map_err(|_| {
            FunctionCallError::RespondToModel(
                "SLACK_BOT_TOKEN environment variable is not set".to_owned(),
            )
        })?;

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        match args.action.as_str() {
            "send_message" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let content = require_field(&args.content, "content")?;
                let url = format!("{SLACK_API_BASE}/chat.postMessage");
                let body = serde_json::json!({
                    "channel": channel_id,
                    "text": content,
                });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            "update_message" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let content = require_field(&args.content, "content")?;
                let url = format!("{SLACK_API_BASE}/chat.update");
                let body = serde_json::json!({
                    "channel": channel_id,
                    "ts": message_id,
                    "text": content,
                });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            "delete_message" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let url = format!("{SLACK_API_BASE}/chat.delete");
                let body = serde_json::json!({
                    "channel": channel_id,
                    "ts": message_id,
                });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            "add_reaction" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let emoji = require_field(&args.emoji, "emoji")?;
                let url = format!("{SLACK_API_BASE}/reactions.add");
                let body = serde_json::json!({
                    "channel": channel_id,
                    "timestamp": message_id,
                    "name": emoji,
                });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            "remove_reaction" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let emoji = require_field(&args.emoji, "emoji")?;
                let url = format!("{SLACK_API_BASE}/reactions.remove");
                let body = serde_json::json!({
                    "channel": channel_id,
                    "timestamp": message_id,
                    "name": emoji,
                });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            "pin_item" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let url = format!("{SLACK_API_BASE}/pins.add");
                let body = serde_json::json!({
                    "channel": channel_id,
                    "timestamp": message_id,
                });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            "unpin_item" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let url = format!("{SLACK_API_BASE}/pins.remove");
                let body = serde_json::json!({
                    "channel": channel_id,
                    "timestamp": message_id,
                });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            "get_channel_history" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let channel_id = urlencoding::encode(channel_id);
                let limit = args.limit.unwrap_or(50).clamp(1, MAX_HISTORY_LIMIT);
                let url = format!(
                    "{SLACK_API_BASE}/conversations.history?channel={channel_id}&limit={limit}"
                );
                send_request(&client, reqwest::Method::GET, &url, &token, None).await
            }
            "set_channel_topic" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let topic = require_field(&args.topic, "topic")?;
                let url = format!("{SLACK_API_BASE}/conversations.setTopic");
                let body = serde_json::json!({
                    "channel": channel_id,
                    "topic": topic,
                });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            other => model_err(format!("unknown slack action: {other}")),
        }
    }
}

async fn send_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> Result<ToolOutput, FunctionCallError> {
    let mut request = client
        .request(method, url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");

    if let Some(json) = body {
        request = request.json(&json);
    }

    let response = request
        .send()
        .await
        .map_err(|err| reqwest_error_without_url("slack API request failed", err))?;

    let status = response.status();
    let body_bytes = response
        .bytes()
        .await
        .map_err(|err| reqwest_error_without_url("failed to read slack response", err))?;

    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

    if !status.is_success() {
        let body_text = sanitize_error_body(&body_text, &[token]);
        return model_err(format!("slack API returned HTTP {status}: {body_text}"));
    }

    Ok(ToolOutput::ok(body_text))
}
