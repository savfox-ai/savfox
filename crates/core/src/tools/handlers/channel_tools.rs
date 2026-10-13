use std::time::Duration;

use serde::Deserialize;

use super::{gateway_endpoint, gateway_http_client, parse_arguments, require_field};
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct ChannelToolsArgs {
    /// Action to perform. Only "send" is currently implemented.
    action: String,
    /// Target platform: "discord", "slack", "telegram", "whatsapp", "webhook".
    platform: String,
    /// Channel/chat identifier.
    #[serde(default)]
    channel_id: Option<String>,
    /// Message content.
    #[serde(default)]
    content: Option<String>,
    /// Session/reply-to ID.
    #[serde(default)]
    session_id: Option<String>,
}

/// Unified multi-platform channel tool that routes actions to the correct
/// platform (Discord, Slack, Telegram, WhatsApp, etc.) based on a `platform`
/// field, providing a single entry point for cross-platform messaging.
pub struct ChannelToolsHandler;

#[async_trait::async_trait]
impl ToolHandler for ChannelToolsHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("ChannelToolsHandler received unsupported payload"),
        };
        let args: ChannelToolsArgs = parse_arguments(&arguments)?;
        // Route to the gateway's message API for platform-specific dispatch.
        let gateway_url = std::env::var("SAVFOX_GATEWAY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18881".to_owned());

        let client = gateway_http_client(REQUEST_TIMEOUT)?;

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
                        "session_id".to_owned(),
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
            "react" | "edit" | "delete" | "history" | "list_channels" => model_err(format!(
                "channel_tools action '{}' is not implemented by the gateway; only 'send' is currently supported",
                args.action
            )),
            other => model_err(format!(
                "unknown channel_tools action: {other}. Valid actions: send, react, \
                 edit, delete, history, list_channels"
            )),
        }
    }
}

async fn send_post(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<ToolOutput, FunctionCallError> {
    let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
    let url = gateway_endpoint(base_url, &segments)?;
    let response = client
        .post(url.as_str())
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .or_else(|err| model_err(format!("channel request failed: {err}")))?;

    let status = response.status();
    let body_bytes = response
        .bytes()
        .await
        .or_else(|err| model_err(format!("failed to read channel response: {err}")))?;
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

    if !status.is_success() {
        return model_err(format!(
            "channel action returned HTTP {status}: {body_text}"
        ));
    }

    Ok(ToolOutput::ok(body_text))
}
