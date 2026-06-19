use std::time::Duration;

use serde::Deserialize;

use super::{parse_arguments, require_field, reqwest_error_without_url, sanitize_error_body};
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const DISCORD_API_BASE: &str = "https://discord.com/api/v10";
const MAX_LIST_LIMIT: u32 = 100;

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

pub struct DiscordActionsHandler;

#[async_trait::async_trait]
impl ToolHandler for DiscordActionsHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &_invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("DiscordActionsHandler received unsupported payload"),
        };
        let args: DiscordActionsArgs = parse_arguments(&arguments)?;
        let token = std::env::var("DISCORD_BOT_TOKEN")
            .or_else(|_| model_err("DISCORD_BOT_TOKEN environment variable is not set"))?;

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .or_else(|err| model_err(format!("failed to build HTTP client: {err}")))?;

        match args.action.as_str() {
            "send_message" => {
                let channel_id = encoded_required(&args.channel_id, "channel_id")?;
                let content = require_field(&args.content, "content")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/messages");
                let body = serde_json::json!({ "content": content });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            "edit_message" => {
                let channel_id = encoded_required(&args.channel_id, "channel_id")?;
                let message_id = encoded_required(&args.message_id, "message_id")?;
                let content = require_field(&args.content, "content")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}");
                let body = serde_json::json!({ "content": content });
                send_request(&client, reqwest::Method::PATCH, &url, &token, Some(body)).await
            }
            "delete_message" => {
                let channel_id = encoded_required(&args.channel_id, "channel_id")?;
                let message_id = encoded_required(&args.message_id, "message_id")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}");
                send_request(&client, reqwest::Method::DELETE, &url, &token, None).await
            }
            "add_reaction" => {
                let channel_id = encoded_required(&args.channel_id, "channel_id")?;
                let message_id = encoded_required(&args.message_id, "message_id")?;
                let emoji = encoded_required(&args.emoji, "emoji")?;
                let url = format!(
                    "{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me"
                );
                send_request(&client, reqwest::Method::PUT, &url, &token, None).await
            }
            "remove_reaction" => {
                let channel_id = encoded_required(&args.channel_id, "channel_id")?;
                let message_id = encoded_required(&args.message_id, "message_id")?;
                let emoji = encoded_required(&args.emoji, "emoji")?;
                let url = format!(
                    "{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me"
                );
                send_request(&client, reqwest::Method::DELETE, &url, &token, None).await
            }
            "pin_message" => {
                let channel_id = encoded_required(&args.channel_id, "channel_id")?;
                let message_id = encoded_required(&args.message_id, "message_id")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/pins/{message_id}");
                send_request(&client, reqwest::Method::PUT, &url, &token, None).await
            }
            "unpin_message" => {
                let channel_id = encoded_required(&args.channel_id, "channel_id")?;
                let message_id = encoded_required(&args.message_id, "message_id")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/pins/{message_id}");
                send_request(&client, reqwest::Method::DELETE, &url, &token, None).await
            }
            "create_session" => {
                let channel_id = encoded_required(&args.channel_id, "channel_id")?;
                let name = require_field(&args.name, "name")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}/sessions");
                let body = serde_json::json!({ "name": name, "type": 11 });
                send_request(&client, reqwest::Method::POST, &url, &token, Some(body)).await
            }
            "list_members" => {
                let guild_id = encoded_required(&args.guild_id, "guild_id")?;
                let limit = clamp_limit(args.limit, MAX_LIST_LIMIT);
                let url = format!("{DISCORD_API_BASE}/guilds/{guild_id}/members?limit={limit}");
                send_request(&client, reqwest::Method::GET, &url, &token, None).await
            }
            "kick_member" => {
                let guild_id = encoded_required(&args.guild_id, "guild_id")?;
                let user_id = encoded_required(&args.user_id, "user_id")?;
                let url = format!("{DISCORD_API_BASE}/guilds/{guild_id}/members/{user_id}");
                send_request(&client, reqwest::Method::DELETE, &url, &token, None).await
            }
            "ban_member" => {
                let guild_id = encoded_required(&args.guild_id, "guild_id")?;
                let user_id = encoded_required(&args.user_id, "user_id")?;
                let url = format!("{DISCORD_API_BASE}/guilds/{guild_id}/bans/{user_id}");
                let body = serde_json::json!({ "reason": args.reason.as_deref().unwrap_or("") });
                send_request(&client, reqwest::Method::PUT, &url, &token, Some(body)).await
            }
            "get_channel_info" => {
                let channel_id = encoded_required(&args.channel_id, "channel_id")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}");
                send_request(&client, reqwest::Method::GET, &url, &token, None).await
            }
            "get_message_history" => {
                let channel_id = encoded_required(&args.channel_id, "channel_id")?;
                let limit = clamp_limit(args.limit, 50);
                let url =
                    format!("{DISCORD_API_BASE}/channels/{channel_id}/messages?limit={limit}");
                send_request(&client, reqwest::Method::GET, &url, &token, None).await
            }
            "set_channel_topic" => {
                let channel_id = encoded_required(&args.channel_id, "channel_id")?;
                let topic = require_field(&args.topic, "topic")?;
                let url = format!("{DISCORD_API_BASE}/channels/{channel_id}");
                let body = serde_json::json!({ "topic": topic });
                send_request(&client, reqwest::Method::PATCH, &url, &token, Some(body)).await
            }
            other => model_err(format!("unknown discord action: {other}")),
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
        .header("Authorization", format!("Bot {token}"))
        .header("Content-Type", "application/json");

    if let Some(json) = body {
        request = request.json(&json);
    }

    let response = request
        .send()
        .await
        .map_err(|err| reqwest_error_without_url("discord API request failed", err))?;

    let status = response.status();
    let body_bytes = response
        .bytes()
        .await
        .map_err(|err| reqwest_error_without_url("failed to read discord response", err))?;

    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

    if !status.is_success() {
        let body_text = sanitize_error_body(&body_text, &[token]);
        return model_err(format!("discord API returned HTTP {status}: {body_text}"));
    }

    Ok(ToolOutput::ok(body_text))
}

fn encoded_required(field: &Option<String>, name: &str) -> Result<String, FunctionCallError> {
    require_field(field, name).map(|value| urlencoding::encode(value).into_owned())
}

fn clamp_limit(limit: Option<u32>, default: u32) -> u32 {
    limit.unwrap_or(default).clamp(1, MAX_LIST_LIMIT)
}
