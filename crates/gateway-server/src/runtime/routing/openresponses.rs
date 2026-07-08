use std::sync::Arc;

use salvo::prelude::*;
use salvo::sse::{SseEvent, SseKeepAlive};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{error, warn};

use crate::auth::GatewayAuth;
use crate::channel::GatewayChannel;

// ─── Request types ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesRequest {
    /// Model or agent ID.
    pub model: String,
    /// Input: either a string or structured messages.
    pub input: ResponsesInput,
    /// Client-defined tools.
    #[serde(default)]
    pub tools: Vec<ResponsesTool>,
    /// Tool choice strategy.
    #[serde(default)]
    pub tool_choice: Option<ToolChoice>,
    /// Enable SSE streaming.
    #[serde(default)]
    pub stream: bool,
    /// Sampling temperature.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Maximum tokens.
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// Caller metadata.
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ResponsesInput {
    Text(String),
    Messages(Vec<ResponsesMessage>),
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesMessage {
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
    pub role: String,
    #[serde(default)]
    pub content: ResponsesContent,
}
impl ResponsesContent {
    pub(crate) fn to_text(&self) -> Vec<String> {
        match self {
            Self::Text(t) => vec![t.clone()],
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ResponsesContentPart::InputText { text } => Some(text.clone()),
                    _ => None,
                })
                .collect(),
            Self::Empty => vec![],
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(untagged)]
pub(crate) enum ResponsesContent {
    Text(String),
    Parts(Vec<ResponsesContentPart>),
    #[default]
    Empty,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponsesContentPart {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "input_image")]
    InputImage {
        #[serde(default)]
        image_url: Option<String>,
        #[serde(default)]
        detail: Option<String>,
    },
    #[serde(rename = "input_file")]
    InputFile {
        #[serde(default)]
        file_data: Option<String>,
        #[serde(default)]
        filename: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ToolChoice {
    Mode(String),
    Specific { name: String },
}

// ─── Response types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ResponsesResponse {
    id: String,
    object: &'static str,
    created_at: u64,
    model: String,
    output: Vec<ResponsesOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<ResponsesUsage>,
    status: String,
}

#[derive(Debug, Serialize)]
struct ResponsesOutput {
    #[serde(rename = "type")]
    output_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Vec<ResponsesOutputContent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    // function_call fields
    #[serde(skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,
}

#[derive(Debug, Serialize)]
struct ResponsesOutputContent {
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

#[derive(Debug, Serialize)]
struct ResponsesUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn generate_response_id() -> String {
    format!("resp-{}", uuid::Uuid::now_v7())
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn extract_prompt(input: &ResponsesInput) -> String {
    match input {
        ResponsesInput::Text(t) => t.clone(),
        ResponsesInput::Messages(msgs) => {
            let mut parts = Vec::new();
            for msg in msgs {
                let text = match &msg.content {
                    ResponsesContent::Text(t) => t.clone(),
                    ResponsesContent::Parts(parts) => parts
                        .iter()
                        .filter_map(|p| match p {
                            ResponsesContentPart::InputText { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    ResponsesContent::Empty => continue,
                };
                parts.push(format!("[{}]: {text}", msg.role));
            }
            parts.join("\n\n")
        }
    }
}

fn extract_bearer_token(req: &Request) -> Option<String> {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_owned())
}

/// Build extra system prompt describing client-defined tools and tool_choice.
fn build_tools_system_prompt(
    tools: &[ResponsesTool],
    tool_choice: Option<&ToolChoice>,
) -> Option<String> {
    if tools.is_empty() {
        return None;
    }

    let mut prompt = String::from("The following client-defined tools are available:\n\n");
    for tool in tools {
        prompt.push_str(&format!("- **{}**", tool.name));
        if let Some(desc) = &tool.description {
            prompt.push_str(&format!(": {desc}"));
        }
        if let Some(params) = &tool.parameters {
            prompt.push_str(&format!(
                "\n  Parameters: {}",
                serde_json::to_string(params).unwrap_or_default()
            ));
        }
        prompt.push('\n');
    }

    // Apply tool_choice constraint.
    match tool_choice {
        Some(ToolChoice::Mode(mode)) => match mode.as_str() {
            "none" => {
                prompt.push_str("\nDo NOT call any tools. Respond with text only.");
            }
            "required" => {
                prompt.push_str(
                    "\nYou MUST call one of the available tools before responding with text.",
                );
            }
            _ => {} // "auto" is default
        },
        Some(ToolChoice::Specific { name }) => {
            prompt.push_str(&format!(
                "\nYou MUST call the `{name}` tool before responding with text."
            ));
        }
        None => {}
    }

    Some(prompt)
}

// ─── Handler ─────────────────────────────────────────────────────────────────

/// `POST /v1/responses`  - OpenResponses-compatible endpoint.
#[handler]
pub(crate) async fn responses_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    // Auth.
    let auth = if let Ok(a) = depot.get_typed::<Arc<GatewayAuth>>() {
        a.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let token = if let Some(t) = extract_bearer_token(req) {
        t
    } else {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(
            json!({"error": {"message": "missing Authorization header", "type": "invalid_request_error"}}).to_string(),
        ));
        return;
    };

    if auth.validate(&token).await.is_none() {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(
            json!({"error": {"message": "invalid API key", "type": "invalid_request_error"}})
                .to_string(),
        ));
        return;
    }

    // Parse body.
    let body: ResponsesRequest = match req.parse_json().await {
        Ok(b) => b,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": {"message": format!("invalid request: {err}"), "type": "invalid_request_error"}}).to_string(),
            ));
            return;
        }
    };

    let channel = if let Ok(b) = depot.get_typed::<Arc<GatewayChannel>>() {
        b.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let response_id = generate_response_id();
    let created_at = unix_timestamp();
    let model = body.model.clone();
    let tools_prompt = build_tools_system_prompt(&body.tools, body.tool_choice.as_ref());
    let has_tools = !body.tools.is_empty();

    // Build the full prompt, optionally prepending tool instructions.
    let user_prompt = extract_prompt(&body.input);
    let prompt = match tools_prompt {
        Some(tp) => format!("{tp}\n\n---\n\n{user_prompt}"),
        None => user_prompt,
    };

    if body.stream {
        handle_streaming(
            res,
            channel,
            response_id,
            created_at,
            model,
            prompt,
            has_tools,
        )
        .await;
    } else {
        handle_non_streaming(
            res,
            channel,
            response_id,
            created_at,
            model,
            prompt,
            has_tools,
        )
        .await;
    }
}

/// Try to detect a function call in the agent reply.
///
/// Looks for a JSON code block with `tool`/`name` and `arguments`/`params` fields,
/// which is the pattern agents use when they want to delegate to a client tool.
fn try_parse_function_call(reply: &str) -> Option<(String, String)> {
    // Look for JSON within code fences.
    let json_str = if let Some(start) = reply.find("```json") {
        let content = &reply[start + 7..];
        content.split("```").next().map(|s| s.trim())
    } else if let Some(start) = reply.find("```") {
        let content = &reply[start + 3..];
        content.split("```").next().map(|s| s.trim())
    } else {
        // Try parsing the whole reply as JSON.
        Some(reply.trim())
    };

    let json_str = json_str?;
    let parsed: Value = serde_json::from_str(json_str).ok()?;

    let name = parsed
        .get("tool")
        .or_else(|| parsed.get("name"))
        .or_else(|| parsed.get("function"))
        .and_then(|v| v.as_str())?
        .to_owned();

    let args = parsed
        .get("arguments")
        .or_else(|| parsed.get("params"))
        .or_else(|| parsed.get("input"))
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    let args_str = if args.is_string() {
        args.as_str().unwrap_or("{}").to_owned()
    } else {
        serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_owned())
    };

    Some((name, args_str))
}

/// Non-streaming: return full response as JSON.
async fn handle_non_streaming(
    res: &mut Response,
    channel: Arc<GatewayChannel>,
    response_id: String,
    created_at: u64,
    model: String,
    prompt: String,
    has_tools: bool,
) {
    let reply = match channel.invoke_agent_text(&prompt, &model).await {
        Ok(text) => text,
        Err(err) => {
            warn!("agent invocation failed: {err}");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({"error": {"message": format!("agent error: {err}"), "type": "server_error"}}).to_string(),
            ));
            return;
        }
    };

    let input_tokens = (prompt.len() / 4) as u64;
    let output_tokens = (reply.len() / 4) as u64;

    // Check if the agent's reply looks like a function call (when client tools are present).
    let output = if has_tools {
        if let Some((fn_name, fn_args)) = try_parse_function_call(&reply) {
            // Return as function_call output with status "incomplete" (client must execute).
            vec![ResponsesOutput {
                output_type: "function_call".to_owned(),
                id: Some(format!("call-{}", uuid::Uuid::now_v7())),
                call_id: Some(format!("call_{}", uuid::Uuid::now_v7())),
                name: Some(fn_name),
                arguments: Some(fn_args),
                role: None,
                content: None,
                status: None,
            }]
        } else {
            vec![ResponsesOutput {
                output_type: "message".to_owned(),
                id: Some(format!("msg-{}", uuid::Uuid::now_v7())),
                role: Some("assistant".to_owned()),
                content: Some(vec![ResponsesOutputContent {
                    content_type: "output_text".to_owned(),
                    text: reply,
                }]),
                status: Some("completed".to_owned()),
                call_id: None,
                name: None,
                arguments: None,
            }]
        }
    } else {
        vec![ResponsesOutput {
            output_type: "message".to_owned(),
            id: Some(format!("msg-{}", uuid::Uuid::now_v7())),
            role: Some("assistant".to_owned()),
            content: Some(vec![ResponsesOutputContent {
                content_type: "output_text".to_owned(),
                text: reply,
            }]),
            status: Some("completed".to_owned()),
            call_id: None,
            name: None,
            arguments: None,
        }]
    };

    let is_function_call = output
        .first()
        .is_some_and(|o| o.output_type == "function_call");
    let status = if is_function_call {
        "incomplete"
    } else {
        "completed"
    };

    let response = ResponsesResponse {
        id: response_id,
        object: "response",
        created_at,
        model,
        output,
        usage: Some(ResponsesUsage {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
        }),
        status: status.to_owned(),
    };

    res.render(Text::Json(
        serde_json::to_string(&response).unwrap_or_default(),
    ));
}

/// Streaming: send SSE events.
async fn handle_streaming(
    res: &mut Response,
    channel: Arc<GatewayChannel>,
    response_id: String,
    _created_at: u64,
    model: String,
    prompt: String,
    has_tools: bool,
) {
    let (tx, rx) = mpsc::channel::<Result<SseEvent, salvo::Error>>(64);

    let rid = response_id;
    let m = model;
    tokio::spawn(async move {
        // Initial event: response.created
        let created_json = json!({
            "type": "response.created",
            "response": {
                "id": rid,
                "object": "response",
                "status": "in_progress",
                "model": m,
            }
        });
        let _ = tx
            .send(Ok(SseEvent::default().name("response.created").text(
                serde_json::to_string(&created_json).unwrap_or_default(),
            )))
            .await;

        match channel.invoke_agent_text(&prompt, &m).await {
            Ok(text) => {
                // Check if the reply looks like a function call.
                let fn_call = if has_tools {
                    try_parse_function_call(&text)
                } else {
                    None
                };

                if let Some((fn_name, fn_args)) = fn_call {
                    // Emit function_call events.
                    let call_id = format!("call_{}", uuid::Uuid::now_v7());
                    let fn_call_json = json!({
                        "type": "response.function_call_arguments.delta",
                        "call_id": call_id,
                        "name": fn_name,
                        "delta": fn_args,
                    });
                    let _ =
                        tx.send(Ok(SseEvent::default()
                            .name("response.function_call_arguments.delta")
                            .text(serde_json::to_string(&fn_call_json).unwrap_or_default())))
                            .await;

                    let fn_done_json = json!({
                        "type": "response.function_call_arguments.done",
                        "call_id": call_id,
                        "name": fn_name,
                        "arguments": fn_args,
                    });
                    let _ =
                        tx.send(Ok(SseEvent::default()
                            .name("response.function_call_arguments.done")
                            .text(serde_json::to_string(&fn_done_json).unwrap_or_default())))
                            .await;

                    // Incomplete  - client must execute the function.
                    let completed_json = json!({
                        "type": "response.completed",
                        "response": {
                            "id": rid,
                            "status": "incomplete",
                        }
                    });
                    let _ = tx
                        .send(Ok(SseEvent::default().name("response.completed").text(
                            serde_json::to_string(&completed_json).unwrap_or_default(),
                        )))
                        .await;
                } else {
                    // Content delta.
                    let delta_json = json!({
                        "type": "response.output_text.delta",
                        "delta": text,
                    });
                    let _ = tx
                        .send(Ok(SseEvent::default()
                            .name("response.output_text.delta")
                            .text(serde_json::to_string(&delta_json).unwrap_or_default())))
                        .await;

                    // Content done.
                    let done_json = json!({
                        "type": "response.output_text.done",
                        "text": text,
                    });
                    let _ = tx
                        .send(Ok(SseEvent::default()
                            .name("response.output_text.done")
                            .text(serde_json::to_string(&done_json).unwrap_or_default())))
                        .await;

                    // Response completed.
                    let completed_json = json!({
                        "type": "response.completed",
                        "response": {
                            "id": rid,
                            "status": "completed",
                        }
                    });
                    let _ = tx
                        .send(Ok(SseEvent::default().name("response.completed").text(
                            serde_json::to_string(&completed_json).unwrap_or_default(),
                        )))
                        .await;
                }
            }
            Err(err) => {
                error!("streaming agent error: {err}");
                let failed_json = json!({
                    "type": "response.failed",
                    "error": { "message": format!("{err}") },
                });
                let _ = tx
                    .send(Ok(SseEvent::default().name("response.failed").text(
                        serde_json::to_string(&failed_json).unwrap_or_default(),
                    )))
                    .await;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    SseKeepAlive::new(stream).stream(res);
}
