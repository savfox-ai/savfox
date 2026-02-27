use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

/// WhatsApp actions tool for message reactions via the WhatsApp Bridge API.
pub struct WhatsAppActionsHandler;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct WhatsAppActionsArgs {
    /// Action to perform: "react".
    action: String,
    /// Chat JID (e.g., "1234567890@s.whatsapp.net").
    #[serde(default)]
    chat_jid: Option<String>,
    /// Message ID to react to.
    #[serde(default)]
    message_id: Option<String>,
    /// Emoji reaction (e.g., "\u{1f44d}").
    #[serde(default)]
    emoji: Option<String>,
    /// Whether to remove an existing reaction.
    #[serde(default)]
    remove: Option<bool>,
}

#[async_trait]
impl ToolHandler for WhatsAppActionsHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "whatsapp_actions requires function payload".to_string(),
                ));
            }
        };

        let args: WhatsAppActionsArgs = parse_arguments(&arguments)?;

        let api_url = std::env::var("WHATSAPP_API_URL").map_err(|_| {
            FunctionCallError::RespondToModel(
                "WHATSAPP_API_URL environment variable is not set".to_string(),
            )
        })?;

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        match args.action.as_str() {
            "react" => {
                let chat_jid = require_field(&args.chat_jid, "chat_jid")?;
                let message_id = require_field(&args.message_id, "message_id")?;

                let mut body = serde_json::json!({
                    "chatJid": chat_jid,
                    "messageId": message_id,
                });

                if let Some(remove) = args.remove {
                    if remove {
                        body["remove"] = serde_json::Value::Bool(true);
                    }
                }

                if let Some(emoji) = &args.emoji {
                    body["emoji"] = serde_json::Value::String(emoji.clone());
                }

                let url = format!("{api_url}/api/react");
                let response = client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await
                    .map_err(|err| {
                        FunctionCallError::RespondToModel(format!(
                            "WhatsApp API request failed: {err}"
                        ))
                    })?;

                let status = response.status();
                let body_bytes = response.bytes().await.map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to read WhatsApp response: {err}"
                    ))
                })?;
                let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

                if !status.is_success() {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "WhatsApp API returned HTTP {status}: {body_text}"
                    )));
                }

                Ok(ToolOutput::Function {
                    content: body_text,
                    content_items: None,
                    success: Some(true),
                })
            }
            other => Err(FunctionCallError::RespondToModel(format!(
                "unknown whatsapp_actions action: {other}. Valid actions: react"
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
