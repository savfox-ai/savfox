use std::time::Duration;

use serde::Deserialize;

use super::{gateway_endpoint, gateway_http_client, parse_arguments};
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct AgentStepArgs {
    /// Session key / agent ID to invoke.
    session_id: String,
    /// The prompt or message to send to the agent.
    prompt: String,
    /// Optional system prompt override for this step.
    #[serde(default)]
    system: Option<String>,
    /// Optional model override.
    #[serde(default)]
    model: Option<String>,
    /// Maximum seconds to wait for the agent step to complete.
    #[serde(default = "defaults::timeout")]
    timeout_secs: u64,
    /// Maximum number of history messages to scan for the reply.
    #[serde(default = "defaults::history_limit")]
    history_limit: usize,
    /// Agent lane to use (e.g., "nested" for sub-agent calls).
    #[serde(default)]
    lane: Option<String>,
}

mod defaults {
    pub fn timeout() -> u64 {
        60
    }

    pub fn history_limit() -> usize {
        50
    }
}

/// Runs a single agent processing step: sends a message/prompt to a session,
/// waits for the agent to complete, and returns the latest assistant reply.
///
/// This is the core building block for agent-to-agent orchestration.
pub struct AgentStepHandler;

#[async_trait::async_trait]
impl ToolHandler for AgentStepHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("AgentStepHandler received unsupported payload"),
        };
        let args: AgentStepArgs = parse_arguments(&arguments)?;
        if args.session_id.is_empty() {
            return model_err("session_id must not be empty");
        }
        if args.prompt.is_empty() {
            return model_err("prompt must not be empty");
        }

        let gateway_url = std::env::var("SAVFOX_GATEWAY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18881".to_owned());

        let client = gateway_http_client(Duration::from_secs(args.timeout_secs.max(5)))?;

        // Step 1: Send prompt to the agent via gateway.
        let idempotency_key = uuid::Uuid::new_v4().to_string();
        let mut agent_body = serde_json::json!({
            "session_id": args.session_id,
            "message": args.prompt,
            "idempotency_key": idempotency_key,
        });

        if let Some(system) = &args.system {
            agent_body["system"] = serde_json::Value::String(system.clone());
        }
        if let Some(model) = &args.model {
            agent_body["model"] = serde_json::Value::String(model.clone());
        }
        if let Some(lane) = &args.lane {
            agent_body["lane"] = serde_json::Value::String(lane.clone());
        }

        let agent_url = gateway_endpoint(&gateway_url, &["api", "agent"])?;
        let response = client
            .post(agent_url.as_str())
            .header("Content-Type", "application/json")
            .body(agent_body.to_string())
            .send()
            .await
            .or_else(|err| model_err(format!("agent invocation failed: {err}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            return model_err(format!(
                "agent invocation returned HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            ));
        }

        let run_result: serde_json::Value = response
            .json()
            .await
            .or_else(|err| model_err(format!("failed to parse agent response: {err}")))?;
        let run_id = run_result
            .get("run_id")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_owned();

        // Step 2: Wait for the agent run to complete.
        if !run_id.is_empty() {
            let wait_url = gateway_endpoint(&gateway_url, &["api", "agent", "wait"])?;
            let wait_body = serde_json::json!({
                "run_id": run_id,
                "timeout_secs": args.timeout_secs,
            });

            let _ = client
                .post(wait_url.as_str())
                .header("Content-Type", "application/json")
                .body(wait_body.to_string())
                .send()
                .await;
        }

        // Step 3: Read the latest assistant reply from session history.
        let reply = read_latest_assistant_reply(
            &client,
            &gateway_url,
            &args.session_id,
            args.history_limit,
        )
        .await;

        let result = serde_json::json!({
            "session_id": args.session_id,
            "run_id": run_id,
            "reply": reply,
            "status": if reply.is_empty() { "no_reply" } else { "completed" },
        });

        Ok(ToolOutput::ok(result.to_string()))
    }
}

/// Reads the latest assistant message from a session's chat history,
/// stripping tool-related messages.
async fn read_latest_assistant_reply(
    client: &reqwest::Client,
    gateway_url: &str,
    session_id: &str,
    limit: usize,
) -> String {
    let mut history_url =
        match gateway_endpoint(gateway_url, &["api", "sessions", session_id, "history"]) {
            Ok(url) => url,
            Err(_) => return String::new(),
        };
    history_url
        .query_pairs_mut()
        .append_pair("limit", &limit.to_string());

    let response = match client
        .get(history_url.as_str())
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp,
        _ => return String::new(),
    };

    let body: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(_) => return String::new(),
    };

    let messages = body.get("messages").and_then(|m| m.as_array());
    let Some(messages) = messages else {
        return String::new();
    };

    // Walk messages in reverse, find the last assistant message that isn't a tool call.
    for msg in messages.iter().rev() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "assistant" {
            continue;
        }

        // Skip messages that are tool calls.
        if msg.get("tool_calls").is_some() {
            continue;
        }

        // Extract text content.
        if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }

    String::new()
}
