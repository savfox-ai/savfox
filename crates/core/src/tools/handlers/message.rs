use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

pub struct MessageHandler;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct MessageArgs {
    /// Channel identifier in the format `platform:id` (e.g., `discord:12345`).
    channel: String,
    /// The message text to send.
    text: String,
    /// Optional format hint: "plain" (default), "markdown", "html".
    #[serde(default = "defaults::format")]
    format: String,
}

mod defaults {
    pub fn format() -> String {
        "plain".to_string()
    }
}

#[async_trait]
impl ToolHandler for MessageHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation { payload, .. } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "message handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: MessageArgs = parse_arguments(&arguments)?;

        if args.channel.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "channel must not be empty".to_string(),
            ));
        }

        if args.text.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "text must not be empty".to_string(),
            ));
        }

        // Send the message via the gateway's /api/message endpoint.
        // The gateway URL is determined from the SAVFOX_GATEWAY_URL environment variable,
        // defaulting to localhost:18881.
        let gateway_url = std::env::var("SAVFOX_GATEWAY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18881".to_string());

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        let body = serde_json::json!({
            "channel": args.channel,
            "text": args.text,
            "format": args.format,
        });

        let url = format!("{gateway_url}/api/message");
        let response = client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to send message via gateway: {err}"
                ))
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.bytes().await.unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body);
            return Err(FunctionCallError::RespondToModel(format!(
                "gateway returned HTTP {status}: {body_str}"
            )));
        }

        Ok(ToolOutput::Function {
            content: format!("Message sent to {}", args.channel),
            content_items: None,
            success: Some(true),
        })
    }
}
