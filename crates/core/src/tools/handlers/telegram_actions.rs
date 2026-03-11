use std::time::Duration;

use serde::Deserialize;

use super::{parse_arguments, require_field};
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

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

pub struct TelegramActionsHandler;

#[async_trait::async_trait]
impl ToolHandler for TelegramActionsHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &_invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("TelegramActionsHandler received unsupported payload"),
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
            other => model_err(format!("unknown telegram action: {other}")),
        }
    }
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
        return model_err(format!("telegram API returned HTTP {status}: {body_text}"));
    }

    Ok(ToolOutput::ok(body_text))
}
