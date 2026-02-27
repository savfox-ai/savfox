use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct TelegramActionsHandler;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

#[derive(Deserialize)]
#[allow(dead_code)]
struct TelegramActionsArgs {
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

#[async_trait]
impl ToolHandler for TelegramActionsHandler {
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
                    "telegram_actions handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: TelegramActionsArgs = parse_arguments(&arguments)?;

        let token = std::env::var("TELEGRAM_BOT_TOKEN").map_err(|_| {
            FunctionCallError::RespondToModel(
                "TELEGRAM_BOT_TOKEN environment variable is not set".to_string(),
            )
        })?;

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        let base = format!("{TELEGRAM_API_BASE}/bot{token}");

        match args.action.as_str() {
            "send_message" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let content = require_field(&args.content, "content")?;
                let url = format!("{base}/sendMessage");
                let body = serde_json::json!({
                    "chat_id": channel_id,
                    "text": content,
                });
                send_request(&client, reqwest::Method::POST, &url, Some(body)).await
            }
            "edit_message" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let content = require_field(&args.content, "content")?;
                let url = format!("{base}/editMessageText");
                let body = serde_json::json!({
                    "chat_id": channel_id,
                    "message_id": message_id,
                    "text": content,
                });
                send_request(&client, reqwest::Method::POST, &url, Some(body)).await
            }
            "delete_message" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let message_id = require_field(&args.message_id, "message_id")?;
                let url = format!("{base}/deleteMessage");
                let body = serde_json::json!({
                    "chat_id": channel_id,
                    "message_id": message_id,
                });
                send_request(&client, reqwest::Method::POST, &url, Some(body)).await
            }
            "send_sticker" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let content = require_field(&args.content, "content")?;
                let url = format!("{base}/sendSticker");
                let body = serde_json::json!({
                    "chat_id": channel_id,
                    "sticker": content,
                });
                send_request(&client, reqwest::Method::POST, &url, Some(body)).await
            }
            "get_chat_info" => {
                let channel_id = require_field(&args.channel_id, "channel_id")?;
                let url = format!("{base}/getChat?chat_id={channel_id}");
                send_request(&client, reqwest::Method::GET, &url, None).await
            }
            other => Err(FunctionCallError::RespondToModel(format!(
                "unknown telegram action: {other}"
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
    body: Option<serde_json::Value>,
) -> Result<ToolOutput, FunctionCallError> {
    let mut request = client
        .request(method, url)
        .header("Content-Type", "application/json");

    if let Some(json) = body {
        request = request.json(&json);
    }

    let response = request.send().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("telegram API request failed: {err}"))
    })?;

    let status = response.status();
    let body_bytes = response.bytes().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read telegram response: {err}"))
    })?;

    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "telegram API returned HTTP {status}: {body_text}"
        )));
    }

    Ok(ToolOutput::Function {
        content: body_text,
        content_items: None,
        success: Some(true),
    })
}
