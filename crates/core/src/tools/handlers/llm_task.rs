use std::time::Duration;

use serde::Deserialize;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Deserialize)]
struct LlmTaskArgs {
    /// The task prompt to send to the LLM.
    prompt: String,
    /// Optional model to use (defaults to the agent's model).
    #[serde(default)]
    model: Option<String>,
    /// Optional system prompt for the task.
    #[serde(default)]
    system: Option<String>,
    /// Optional JSON schema the output must conform to.
    #[serde(default)]
    output_schema: Option<serde_json::Value>,
    /// Temperature for sampling.
    #[serde(default)]
    temperature: Option<f64>,
    /// Maximum tokens for the response.
    #[serde(default)]
    max_tokens: Option<u64>,
    /// Timeout in seconds.
    #[serde(default)]
    timeout_secs: Option<u64>,
}

/// LLM Task tool — run a standalone LLM inference task with optional
/// JSON schema validation on the output.
///
/// This mirrors OpenClaw's `llm-task-tool.ts` extension. It allows an agent
/// to delegate a sub-task to an LLM with a specific prompt, model, and
/// optional JSON schema constraint, receiving structured output back.
pub struct LlmTaskHandler;

#[async_trait::async_trait]
impl ToolHandler for LlmTaskHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &_invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("LlmTaskHandler received unsupported payload"),
        };
        let args: LlmTaskArgs = parse_arguments(&arguments)?;

        // Route through the gateway's OpenAI-compatible endpoint.
        let gateway_url = std::env::var("SAVFOX_GATEWAY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18881".to_owned());
        let token = std::env::var("SAVFOX_GATEWAY_TOKEN").ok();

        let timeout_secs = args.timeout_secs.unwrap_or(REQUEST_TIMEOUT.as_secs());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        // Build the OpenAI-format request.
        let mut messages = Vec::new();

        if let Some(system) = &args.system {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }

        messages.push(serde_json::json!({
            "role": "user",
            "content": args.prompt,
        }));

        let mut body = serde_json::json!({
            "model": args.model.as_deref().unwrap_or("default"),
            "messages": messages,
            "stream": false,
        });

        if let Some(temperature) = args.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }
        if let Some(max_tokens) = args.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        let url = format!("{gateway_url}/v1/chat/completions");
        let mut request = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body);

        if let Some(token) = &token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }

        let response = request.send().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("LLM task request failed: {err}"))
        })?;

        let status = response.status();
        let body_bytes = response.bytes().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to read LLM task response: {err}"))
        })?;
        let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

        if !status.is_success() {
            return model_err(format!("LLM task returned HTTP {status}: {body_text}"));
        }

        // Extract the assistant message content from the OpenAI response.
        let mut result_text = body_text.clone();
        if let Ok(response_json) = serde_json::from_str::<serde_json::Value>(&body_text)
            && let Some(content) = response_json
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
        {
            result_text = content.to_owned();
        }

        // If an output_schema was provided, validate the result is valid JSON.
        if args.output_schema.is_some() {
            match serde_json::from_str::<serde_json::Value>(&result_text) {
                Ok(json_value) => {
                    result_text = serde_json::to_string_pretty(&json_value).unwrap_or(result_text);
                }
                Err(_) => {
                    return model_err(format!(
                        "LLM task output is not valid JSON (schema validation requested): {result_text}"
                    ));
                }
            }
        }

        Ok(ToolOutput::ok(result_text))
    }
}
