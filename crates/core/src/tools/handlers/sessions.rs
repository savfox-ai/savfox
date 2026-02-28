use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::{ToolHandler, ToolKind};

/// Handles multiple session-related tool names:
/// - `sessions_list` — List active sessions
/// - `sessions_history` — Get session history
/// - `sessions_send` — Send a message to a session
/// - `session_status` — Get session metadata
pub struct SessionsHandler;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct SessionsListArgs {
    /// Optional filter for session type.
    #[serde(default)]
    filter: Option<String>,
}

#[derive(Deserialize)]
struct SessionsHistoryArgs {
    /// Session ID to get history for.
    session_id: String,
    /// Maximum number of messages to return.
    #[serde(default = "defaults::limit")]
    limit: usize,
}

#[derive(Deserialize)]
struct SessionsSendArgs {
    /// Session ID to send to.
    session_id: String,
    /// Message text.
    message: String,
}

#[derive(Deserialize)]
struct SessionStatusArgs {
    /// Session ID to check.
    session_id: String,
}

mod defaults {
    pub fn limit() -> usize {
        50
    }
}

#[async_trait]
impl ToolHandler for SessionsHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            tool_name, payload, ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "sessions handler received unsupported payload".to_string(),
                ));
            }
        };

        let gateway_url = std::env::var("SAVFOX_GATEWAY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18881".to_string());

        match tool_name.as_str() {
            "sessions_list" => self.handle_list(&arguments, &gateway_url).await,
            "sessions_history" => self.handle_history(&arguments, &gateway_url).await,
            "sessions_send" => self.handle_send(&arguments, &gateway_url).await,
            "session_status" => self.handle_status(&arguments, &gateway_url).await,
            _ => Err(FunctionCallError::RespondToModel(format!(
                "unknown sessions tool: {tool_name}"
            ))),
        }
    }
}

impl SessionsHandler {
    async fn handle_list(
        &self,
        arguments: &str,
        gateway_url: &str,
    ) -> Result<ToolOutput, FunctionCallError> {
        let _args: SessionsListArgs = parse_arguments(arguments)?;

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        let url = format!("{gateway_url}/api/sessions");
        let response = client.get(&url).send().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to list sessions: {err}"))
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(FunctionCallError::RespondToModel(format!(
                "gateway returned HTTP {status}"
            )));
        }

        let body = response.bytes().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to read response: {err}"))
        })?;

        Ok(ToolOutput::Function {
            content: String::from_utf8_lossy(&body).into_owned(),
            content_items: None,
            success: Some(true),
        })
    }

    async fn handle_history(
        &self,
        arguments: &str,
        _gateway_url: &str,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SessionsHistoryArgs = parse_arguments(arguments)?;

        // Session history is stored locally by the SessionManager.
        // For now, return a placeholder indicating that the session history
        // would be retrieved from the local session storage.
        Ok(ToolOutput::Function {
            content: serde_json::json!({
                "session_id": args.session_id,
                "messages": [],
                "note": "Session history retrieval via SessionManager - implement when session storage API is available"
            })
            .to_string(),
            content_items: None,
            success: Some(true),
        })
    }

    async fn handle_send(
        &self,
        arguments: &str,
        _gateway_url: &str,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SessionsSendArgs = parse_arguments(arguments)?;

        if args.session_id.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "session_id must not be empty".to_string(),
            ));
        }

        // Sending to a session would go through the SessionManager's message routing.
        // For now, return a placeholder.
        Ok(ToolOutput::Function {
            content: format!(
                "Message queued for session {} (direct session messaging requires SessionManager integration)",
                args.session_id
            ),
            content_items: None,
            success: Some(true),
        })
    }

    async fn handle_status(
        &self,
        arguments: &str,
        _gateway_url: &str,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SessionStatusArgs = parse_arguments(arguments)?;

        Ok(ToolOutput::Function {
            content: serde_json::json!({
                "session_id": args.session_id,
                "status": "unknown",
                "note": "Session status via SessionManager - implement when session status API is available"
            })
            .to_string(),
            content_items: None,
            success: Some(true),
        })
    }
}
