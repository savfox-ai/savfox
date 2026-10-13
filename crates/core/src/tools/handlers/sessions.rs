use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::handlers::{gateway_endpoint, gateway_http_client, parse_arguments};
use crate::tools::registry::{ToolHandler, ToolKind};

/// Handles multiple session-related tool names:
/// - `sessions_list` — List active sessions
/// - `sessions_history` — Get session history
/// - `session_status` — Get session metadata
pub struct SessionsHandler;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HISTORY_LIMIT: usize = 500;

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
                return model_err("sessions handler received unsupported payload");
            }
        };

        let gateway_url = std::env::var("SAVFOX_GATEWAY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18881".to_owned());

        match tool_name.as_str() {
            "sessions_list" => self.handle_list(&arguments, &gateway_url).await,
            "sessions_history" => self.handle_history(&arguments, &gateway_url).await,
            "session_status" => self.handle_status(&arguments, &gateway_url).await,
            _ => model_err(format!("unknown sessions tool: {tool_name}")),
        }
    }
}

impl SessionsHandler {
    async fn handle_list(
        &self,
        arguments: &str,
        gateway_url: &str,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SessionsListArgs = parse_arguments(arguments)?;

        let client = gateway_http_client(REQUEST_TIMEOUT)?;
        let url = gateway_endpoint(gateway_url, &["api", "sessions"])?;
        let mut payload = get_json(&client, url, "list sessions").await?;

        if let Some(filter) = args
            .filter
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let needle = filter.to_ascii_lowercase();
            if let Some(sessions) = payload.get_mut("sessions").and_then(|v| v.as_array_mut()) {
                sessions.retain(|session| {
                    session
                        .as_str()
                        .is_some_and(|id| id.to_ascii_lowercase().contains(&needle))
                });
                payload["count"] = serde_json::json!(sessions.len());
            }
        }

        Ok(ToolOutput::ok(payload.to_string()))
    }

    async fn handle_history(
        &self,
        arguments: &str,
        gateway_url: &str,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SessionsHistoryArgs = parse_arguments(arguments)?;
        let session_id = args.session_id.trim();
        if session_id.is_empty() {
            return model_err("session_id must not be empty");
        }

        let client = gateway_http_client(REQUEST_TIMEOUT)?;
        let mut url = gateway_endpoint(gateway_url, &["api", "sessions", session_id, "history"])?;
        url.query_pairs_mut()
            .append_pair("limit", &args.limit.clamp(1, MAX_HISTORY_LIMIT).to_string());
        let payload = get_json(&client, url, "get session history").await?;
        Ok(ToolOutput::ok(payload.to_string()))
    }

    async fn handle_status(
        &self,
        arguments: &str,
        gateway_url: &str,
    ) -> Result<ToolOutput, FunctionCallError> {
        let args: SessionStatusArgs = parse_arguments(arguments)?;
        let session_id = args.session_id.trim();
        if session_id.is_empty() {
            return model_err("session_id must not be empty");
        }

        let client = gateway_http_client(REQUEST_TIMEOUT)?;
        let url = gateway_endpoint(gateway_url, &["api", "sessions"])?;
        let payload = get_json(&client, url, "get session status").await?;
        let active = payload
            .get("sessions")
            .and_then(|value| value.as_array())
            .is_some_and(|sessions| {
                sessions
                    .iter()
                    .any(|value| value.as_str() == Some(session_id))
            });
        Ok(ToolOutput::ok(
            serde_json::json!({
                "session_id": session_id,
                "status": if active { "active" } else { "not_found" },
            })
            .to_string(),
        ))
    }
}

async fn get_json(
    client: &reqwest::Client,
    url: reqwest::Url,
    operation: &str,
) -> Result<serde_json::Value, FunctionCallError> {
    let response = client.get(url.as_str()).send().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to {operation}: {err}"))
    })?;
    let status = response.status();
    let body = response.bytes().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read gateway response: {err}"))
    })?;
    if !status.is_success() {
        return model_err(format!(
            "gateway returned HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        ));
    }
    serde_json::from_slice(&body).map_err(|err| {
        FunctionCallError::RespondToModel(format!("gateway returned invalid JSON: {err}"))
    })
}
