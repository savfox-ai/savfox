use std::time::Duration;

use serde::Deserialize;

use super::{parse_arguments, require_field};
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

/// WhatsApp actions tool for message reactions via the WhatsApp Channel API.
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

pub struct WhatsAppActionsHandler;

#[async_trait::async_trait]
impl ToolHandler for WhatsAppActionsHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &_invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("WhatsAppActionsHandler received unsupported payload"),
        };
        let args: WhatsAppActionsArgs = parse_arguments(&arguments)?;
        let api_url = std::env::var("WHATSAPP_API_URL")
            .or_else(|_| model_err("WHATSAPP_API_URL environment variable is not set"))?;

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .or_else(|err| model_err(format!("failed to build HTTP client: {err}")))?;

        match args.action.as_str() {
            "react" => {
                let chat_jid = require_field(&args.chat_jid, "chat_jid")?;
                let message_id = require_field(&args.message_id, "message_id")?;

                let mut body = serde_json::json!({
                    "chatJid": chat_jid,
                    "messageId": message_id,
                });

                if let Some(remove) = args.remove
                    && remove {
                        body["remove"] = serde_json::Value::Bool(true);
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
                    .or_else(|err| model_err(format!("WhatsApp API request failed: {err}")))?;

                let status = response.status();
                let body_bytes = response
                    .bytes()
                    .await
                    .or_else(|err| model_err(format!("failed to read WhatsApp response: {err}")))?;
                let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

                if !status.is_success() {
                    return model_err(format!("WhatsApp API returned HTTP {status}: {body_text}"));
                }

                Ok(ToolOutput::ok(body_text))
            }
            other => model_err(format!(
                "unknown whatsapp_actions action: {other}. Valid actions: react"
            )),
        }
    }
}
