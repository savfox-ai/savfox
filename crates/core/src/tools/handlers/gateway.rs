use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

/// Gateway tool that allows an agent to invoke any gateway server method.
///
/// Unlike `gateway_status` (read-only status/config), this tool provides full
/// access to the gateway's JSON-RPC methods from within agent context:
/// - Agent management (agents.list, agents.create, agents.update, agents.delete)
/// - Session management (sessions.list, sessions.patch, sessions.reset, sessions.delete)
/// - Chat operations (chat.send, chat.history, chat.abort)
/// - Cron management (cron.list, cron.add, cron.update, cron.remove, cron.run)
/// - Node control (node.list, node.describe, node.invoke)
/// - Channel management (channels.status, channels.logout, send)
/// - TTS control (tts.status, tts.convert, tts.enable, tts.disable)
/// - Config (config.get, config.set, config.patch, config.apply, config.schema)
/// - And any other registered gateway method
pub struct GatewayToolHandler;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct GatewayToolArgs {
    /// The gateway method to call (e.g., "agents.list", "chat.send", "cron.add").
    method: String,
    /// Method parameters as a JSON object. Passed as-is to the gateway.
    #[serde(default)]
    params: Option<serde_json::Value>,
}

#[async_trait]
impl ToolHandler for GatewayToolHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "gateway tool requires function payload".to_string(),
                ));
            }
        };

        let args: GatewayToolArgs = parse_arguments(&arguments)?;
        let gateway_url = std::env::var("SAVFOX_GATEWAY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18881".to_string());

        // Read the gateway token from the environment.
        let token = std::env::var("SAVFOX_GATEWAY_TOKEN").ok();

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        // Build a JSON-RPC 2.0 request to POST to the gateway's /tools/invoke
        // or the method-based endpoint.
        let rpc_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": uuid::Uuid::now_v7().to_string(),
            "method": args.method,
            "params": args.params.unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
        });

        let url = format!("{gateway_url}/rpc");
        let mut request = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&rpc_request);

        if let Some(token) = &token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }

        let response = request.send().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("gateway RPC request failed: {err}"))
        })?;

        let status = response.status();
        let body_bytes = response.bytes().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to read gateway response: {err}"))
        })?;
        let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

        if !status.is_success() {
            return Err(FunctionCallError::RespondToModel(format!(
                "gateway returned HTTP {status}: {body_text}"
            )));
        }

        // Try to extract the "result" field from the JSON-RPC response.
        if let Ok(rpc_response) = serde_json::from_str::<serde_json::Value>(&body_text) {
            if let Some(error) = rpc_response.get("error") {
                let message = error
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error");
                return Err(FunctionCallError::RespondToModel(format!(
                    "gateway method '{}' error: {message}",
                    args.method
                )));
            }
            if let Some(result) = rpc_response.get("result") {
                return Ok(ToolOutput::Function {
                    content: serde_json::to_string_pretty(result).unwrap_or(body_text),
                    content_items: None,
                    success: Some(true),
                });
            }
        }

        Ok(ToolOutput::Function {
            content: body_text,
            content_items: None,
            success: Some(true),
        })
    }
}
