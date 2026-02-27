use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct DiscordActionsHandler;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

#[derive(Deserialize)]
struct DiscordActionsArgs {
    action: String,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    emoji: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    name: Option<String>,
}

#[async_trait]
impl ToolHandler for DiscordActionsHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            tool_name: _,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "discord_actions handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: DiscordActionsArgs = parse_arguments(&arguments)?;

        let token = std::env::var("DISCORD_BOT_TOKEN").map_err(|_| {
            FunctionCallError::RespondToModel(
                "DISCORD_BOT_TOKEN environment variable is not set".to_string(),
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
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/messages");
                let body = serde_json::json!({ "content": content });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            "edit_message" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let content = require_field(&args.content, "content")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}");
                let body = serde_json::json!({ "content": content });
                send_request(&client, reqwest::Method::PATCH, &url, &token, Some(body)).await
            }
            "delete_message" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}");
                send_request(&client, reqwest::Method::DELETE, &url, &token, None).await
            }
            "add_reaction" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let emoji = require_field(&args.emoji, "emoji")?;
                let url = format!(
                    "{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me"
                );
                send_request(&client, reqwest::Method::PUT, &url, &token, None).await
            }
            "remove_reaction" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let emoji = require_field(&args.emoji, "emoji")?;
                let url = format!(
                    "{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me"
                );
                send_request(&client, reqwest::Method::DELETE, &url, &token, None).await
            }
            "pin_message" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/pins/{message_id}");
                send_request(&client, reqwest::Method::PUT, &url, &token, None).await
            }
            "unpin_message" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/pins/{message_id}");
                send_request(&client, reqwest::Method::DELETE, &url, &token, None).await
            }
            "create_session" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let name = require_field(&args.name, "name")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/sessions");
                let body = serde_json::json!({ "name": name, "type": 11 });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            "list_members" => {
                let guild_id = require_field(&args.guild_id, "guild_id")?;
                let limit = args.limit.unwrap_or(100);
                let url = format!("{DISCORD_API_BASE}/guilds/{guild_id}/members?limit={limit}");
                send_request(&client, reqwest::Method::GET, &url, &token, None).await
            }
            "kick_member" => {
                let guild_id = require_field(&args.guild_id, "guild_id")?;
                let user_id = require_field(&args.user_id, "user_id")?;
                let url = format!("{DISCORD_API_BASE}/guilds/{guild_id}/members/{user_id}");
                send_request(&client, reqwest::Method::DELETE, &url, &token, None).await
            }
            "ban_member" => {
                let guild_id = require_field(&args.guild_id, "guild_id")?;
                let user_id = require_field(&args.user_id, "user_id")?;
                let url = format!("{DISCORD_API_BASE}/guilds/{guild_id}/bans/{user_id}");
                let body = serde_json::json!({ "reason": args.reason.as_deref().unwrap_or("") });
                send_request(&client, reqwest::Method::PUT, &url, &token, Some(body)).await
            }
            "get_channel_info" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}");
                send_request(&client, reqwest::Method::GET, &url, &token, None).await
            }
            "get_message_history" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let limit = args.limit.unwrap_or(50);
                let url =
                    format!("{DISCORD_API_BASE}/channels/{channel_id}/messages?limit={limit}");
                send_request(&client, reqwest::Method::GET, &url, &token, None).await
            }
            "set_channel_topic" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let topic = require_field(&args.topic, "topic")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}");
                let body = serde_json::json!({ "topic": topic });
                send_request(&client, reqwest::Method::PATCH, &url, &token, Some(body)).await
            }
            other => Err(FunctionCallError::RespondToModel(format!(
                "unknown discord action: {other}"
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

async fn send_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> Result<ToolOutput, FunctionCallError> {
    let mut request = client
        .request(method, url)
        .header("Authorization", format!("Bot {token}"))
        .header("Content-Type", "application/json");

    if let Some(json) = body {
        request = request.json(&json);
    }

    let response = request.send().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("discord API request failed: {err}"))
    })?;

    let status = response.status();
    let body_bytes = response.bytes().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read discord response: {err}"))
    })?;

    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "discord API returned HTTP {status}: {body_text}"
        )));
    }

    Ok(ToolOutput::Function {
        content: body_text,
        content_items: None,
        success: Some(true),
    })
}
