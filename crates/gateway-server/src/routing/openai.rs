use std::sync::Arc;

use salvo::prelude::*;
use salvo::sse::{SseEvent, SseKeepAlive};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{error, warn};

use crate::auth::GatewayAuth;
use crate::bridge::GatewayChannel;
use crate::chat_session::{
    persist_chat_session_metadata, provider_from_model, validate_uuid_v7_session_id,
};
use crate::session::SessionStore;

// ─── Request / Response types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionRequest {
    /// Model or agent ID.
    pub model: String,
    /// Conversation message.
    pub message: ChatMessage,
    /// Optional persisted session id (UUID v7).
    #[serde(default)]
    pub session_id: Option<String>,
    /// Enable SSE streaming.
    #[serde(default)]
    pub stream: bool,
    /// Sampling temperature.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Maximum tokens to generate.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Stop sequences.
    #[serde(default)]
    pub stop: Option<StopCondition>,
    /// Top-p sampling.
    #[serde(default)]
    pub top_p: Option<f64>,
    /// Number of completions (only 1 supported).
    #[serde(default)]
    pub n: Option<u32>,
    /// Caller-provided metadata.
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct ChatMessage {
    pub role: String,
    pub content: MessageContent,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub(crate) enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "type")]
pub(crate) enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}
impl MessageContent {
    pub(crate) fn to_texts(&self) -> Vec<String> {
        match self {
            MessageContent::Text(t) => vec![t.clone()],
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct ImageUrl {
    pub url: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub(crate) enum StopCondition {
    Single(String),
    Multiple(Vec<String>),
}

// ─── Response types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<Choice>,
    usage: Usage,
}

#[derive(Debug, Serialize)]
struct Choice {
    index: u32,
    message: ChatMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug, Serialize)]
struct ChatCompletionChunk {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Serialize)]
struct StreamChoice {
    index: u32,
    delta: DeltaMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeltaMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmbeddedDeltaKind {
    Text,
    Reasoning,
}

#[derive(Debug, Clone)]
struct EmbeddedDelta {
    kind: EmbeddedDeltaKind,
    text: String,
}

fn generate_completion_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::now_v7())
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn parse_embedded_delta_object(value: &Value) -> Option<EmbeddedDelta> {
    let kind = value.get("type").and_then(Value::as_str)?;
    let text = value.get("text").and_then(Value::as_str)?.to_string();
    let kind = match kind {
        "text" => EmbeddedDeltaKind::Text,
        "reasoning_text" => EmbeddedDeltaKind::Reasoning,
        _ => return None,
    };
    Some(EmbeddedDelta { kind, text })
}

fn split_top_level_json_objects(input: &str) -> Option<Vec<String>> {
    let trimmed = input.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return None;
    }

    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut start: Option<usize> = None;

    for (idx, ch) in trimmed.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        out.push(trimmed[s..=idx].to_string());
                    } else {
                        return None;
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }

    if in_string || depth != 0 || out.is_empty() {
        return None;
    }
    Some(out)
}

fn decode_stream_delta(delta_text: &str) -> Vec<EmbeddedDelta> {
    let Some(parts) = split_top_level_json_objects(delta_text) else {
        return vec![EmbeddedDelta {
            kind: EmbeddedDeltaKind::Text,
            text: delta_text.to_string(),
        }];
    };

    let mut decoded = Vec::with_capacity(parts.len());
    for part in &parts {
        let Ok(value) = serde_json::from_str::<Value>(part) else {
            return vec![EmbeddedDelta {
                kind: EmbeddedDeltaKind::Text,
                text: delta_text.to_string(),
            }];
        };
        let Some(segment) = parse_embedded_delta_object(&value) else {
            return vec![EmbeddedDelta {
                kind: EmbeddedDeltaKind::Text,
                text: delta_text.to_string(),
            }];
        };
        decoded.push(segment);
    }
    decoded
}

/// Validate bearer token from the Authorization header.
fn extract_bearer_token(req: &Request) -> Option<String> {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_owned())
}

// ─── Models endpoint ────────────────────────────────────────────────────────

/// Model info in OpenAI format.
#[derive(Debug, Serialize)]
struct ModelObject {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: String,
}

/// Model list response.
#[derive(Debug, Serialize)]
struct ModelListResponse {
    object: &'static str,
    data: Vec<ModelObject>,
}

/// `GET /v1/models`  - OpenAI-compatible model listing endpoint.
#[handler]
pub(crate) async fn models_list_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    // Auth
    let auth = match depot.obtain::<Arc<GatewayAuth>>() {
        Ok(a) => a.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let token = match extract_bearer_token(req) {
        Some(t) => t,
        None => {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Json(
                json!({"error": {"message": "missing Authorization header", "type": "invalid_request_error"}}).to_string(),
            ));
            return;
        }
    };

    if auth.validate(&token).await.is_none() {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(
            json!({"error": {"message": "invalid API key", "type": "invalid_request_error"}})
                .to_string(),
        ));
        return;
    }

    let bridge = match depot.obtain::<Arc<GatewayChannel>>() {
        Ok(b) => b.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let models = bridge.list_models().await;
    let created = unix_timestamp();

    let data: Vec<ModelObject> = models
        .into_iter()
        .map(|m| ModelObject {
            id: m.clone(),
            object: "model",
            created,
            owned_by: "savfox".to_string(),
        })
        .collect();

    let response = ModelListResponse {
        object: "list",
        data,
    };

    res.render(Text::Json(
        serde_json::to_string(&response).unwrap_or_default(),
    ));
}

// ─── Chat Handler ───────────────────────────────────────────────────────────

/// `POST /v1/chat/completions`  - OpenAI-compatible chat completions endpoint.
#[handler]
pub(crate) async fn chat_completions_handler(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) {
    // Auth
    let auth = match depot.obtain::<Arc<GatewayAuth>>() {
        Ok(a) => a.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let token = match extract_bearer_token(req) {
        Some(t) => t,
        None => {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Json(
                json!({"error": {"message": "missing Authorization header", "type": "invalid_request_error"}}).to_string(),
            ));
            return;
        }
    };

    if auth.validate(&token).await.is_none() {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(
            json!({"error": {"message": "invalid API key", "type": "invalid_request_error"}})
                .to_string(),
        ));
        return;
    }

    // Parse request body.
    let body: ChatCompletionRequest = match req.parse_json().await {
        Ok(b) => b,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": {"message": format!("invalid request: {err}"), "type": "invalid_request_error"}}).to_string(),
            ));
            return;
        }
    };

    let bridge = match depot.obtain::<Arc<GatewayChannel>>() {
        Ok(b) => b.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };
    let session_store = match depot.obtain::<Arc<SessionStore>>() {
        Ok(store) => store.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let completion_id = generate_completion_id();
    let created = unix_timestamp();
    let model = body.model.clone();
    let prompt = body.message.content.to_texts().join("\n\n");
    let session_id = match validate_uuid_v7_session_id(body.session_id.as_deref()) {
        Ok(value) => value,
        Err(message) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": {"message": message, "type": "invalid_request_error"}}).to_string(),
            ));
            return;
        }
    };

    if body.stream {
        println!("DEBUG: body.handle_streaming");
        handle_streaming(
            res,
            bridge,
            session_store,
            completion_id,
            created,
            model,
            prompt,
            session_id,
        )
        .await;
    } else {
        println!("DEBUG: body.handle_non_streaming");
        handle_non_streaming(
            res,
            bridge,
            session_store,
            completion_id,
            created,
            model,
            prompt,
            session_id,
        )
        .await;
    }
}

/// Non-streaming response: sends the full agent reply as a single JSON object.
async fn handle_non_streaming(
    res: &mut Response,
    bridge: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    completion_id: String,
    created: u64,
    model: String,
    prompt: String,
    session_id: Option<String>,
) {
    // Route through the bridge to the agent.
    let result = match bridge
        .invoke_agent_text_in_session_with_metadata(&prompt, &model, session_id.as_deref())
        .await
    {
        Ok(result) => result,
        Err(err) => {
            warn!("agent invocation failed: {err}");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({"error": {"message": format!("agent error: {err}"), "type": "server_error"}}).to_string(),
            ));
            return;
        }
    };
    let reply = result.reply.clone();

    let provider = provider_from_model(&model);
    persist_chat_session_metadata(
        session_store.as_ref(),
        &bridge.config().savfox_home,
        &result.session_id,
        &result.session_id,
        &model,
        &provider,
        result.rollout_path.as_deref(),
        result.last_token_usage.as_ref(),
    )
    .await;

    let prompt_tokens = (prompt.len() / 4) as u64;
    let completion_tokens = (reply.len() / 4) as u64;

    let response = ChatCompletionResponse {
        id: completion_id,
        object: "chat.completion",
        created,
        model,
        choices: vec![Choice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text(reply),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            finish_reason: Some("stop".to_string()),
        }],
        usage: Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
    };

    res.render(Text::Json(
        serde_json::to_string(&response).unwrap_or_default(),
    ));
}

/// Streaming response: sends SSE chunks as they become available.
async fn handle_streaming(
    res: &mut Response,
    bridge: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    completion_id: String,
    created: u64,
    model: String,
    prompt: String,
    session_id: Option<String>,
) {
    let (tx, rx) = mpsc::unbounded_channel::<Result<SseEvent, salvo::Error>>();

    // Disable all forms of buffering to ensure immediate SSE delivery
    res.headers_mut()
        .insert("X-Accel-Buffering", "no".parse().unwrap());
    res.headers_mut().insert(
        "Cache-Control",
        "no-cache, no-store, must-revalidate, no-transform"
            .parse()
            .unwrap(),
    );
    res.headers_mut()
        .insert("Pragma", "no-cache".parse().unwrap());
    res.headers_mut().insert("Expires", "0".parse().unwrap());
    res.headers_mut()
        .insert("Connection", "keep-alive".parse().unwrap());
    res.headers_mut()
        .insert("Content-Encoding", "identity".parse().unwrap());
    res.headers_mut().insert(
        "Content-Type",
        "text/event-stream; charset=utf-8".parse().unwrap(),
    );

    let cid = completion_id.clone();
    let m = model.clone();
    let sid = session_id.clone();
    let session_store_for_task = session_store.clone();
    let savfox_home = bridge.config().savfox_home.clone();
    tokio::spawn(async move {
        // First chunk: role announcement.
        let first_chunk = ChatCompletionChunk {
            id: cid.clone(),
            object: "chat.completion.chunk",
            created,
            model: m.clone(),
            choices: vec![StreamChoice {
                index: 0,
                delta: DeltaMessage {
                    role: Some("assistant".to_string()),
                    content: None,
                    reasoning_content: None,
                },
                finish_reason: None,
            }],
        };
        if let Ok(json) = serde_json::to_string(&first_chunk) {
            tracing::info!(
                "[SSE FIRST] Sending first chunk at {:?}",
                std::time::Instant::now()
            );
            let _ = tx.send(Ok(SseEvent::default().text(json)));
        }

        let cid_for_delta = cid.clone();
        let model_for_delta = m.clone();
        let tx_for_delta = tx.clone();
        let emit_delta = move |delta_text: &str| {
            println!("DEBUG: emit_delta called with delta_text: '{}'", delta_text);
            if delta_text.is_empty() {
                return;
            }
            for segment in decode_stream_delta(delta_text) {
                if segment.text.is_empty() {
                    continue;
                }
                tracing::info!(
                    "[SSE DELTA] Sending delta at {:?}: '{}'",
                    std::time::Instant::now(),
                    segment.text
                );
                let (content, reasoning_content) = match segment.kind {
                    EmbeddedDeltaKind::Text => (Some(segment.text), None),
                    EmbeddedDeltaKind::Reasoning => (None, Some(segment.text)),
                };
                let chunk = ChatCompletionChunk {
                    id: cid_for_delta.clone(),
                    object: "chat.completion.chunk",
                    created,
                    model: model_for_delta.clone(),
                    choices: vec![StreamChoice {
                        index: 0,
                        delta: DeltaMessage {
                            role: None,
                            content,
                            reasoning_content,
                        },
                        finish_reason: None,
                    }],
                };
                if let Ok(json) = serde_json::to_string(&chunk) {
                    println!("DEBUG: tx_for_delta send json: '{}'", json);
                    let _ = tx_for_delta.send(Ok(SseEvent::default().text(json)));
                }
            }
        };

        // Stream reply deltas from the agent as they arrive.
        match bridge
            .invoke_agent_text_in_session_stream(&prompt, &m, sid.as_deref(), emit_delta)
            .await
        {
            Ok(result) => {
                let provider = provider_from_model(&m);
                persist_chat_session_metadata(
                    session_store_for_task.as_ref(),
                    &savfox_home,
                    &result.session_id,
                    &result.session_id,
                    &m,
                    &provider,
                    result.rollout_path.as_deref(),
                    result.last_token_usage.as_ref(),
                )
                .await;

                // Final chunk: finish_reason.
                let done_chunk = ChatCompletionChunk {
                    id: cid,
                    object: "chat.completion.chunk",
                    created,
                    model: m,
                    choices: vec![StreamChoice {
                        index: 0,
                        delta: DeltaMessage {
                            role: None,
                            content: None,
                            reasoning_content: None,
                        },
                        finish_reason: Some("stop".to_string()),
                    }],
                };
                if let Ok(json) = serde_json::to_string(&done_chunk) {
                    let _ = tx.send(Ok(SseEvent::default().text(json)));
                }

                // [DONE] marker.
                let _ = tx.send(Ok(SseEvent::default().text("[DONE]")));
            }
            Err(err) => {
                error!("streaming agent error: {err}");
                let _ = tx.send(Ok(SseEvent::default().text("[DONE]")));
            }
        }
    });

    tracing::info!(
        "[SSE STREAM] Starting SSE stream at {:?}",
        std::time::Instant::now()
    );
    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);

    // Use SseKeepAlive to ensure proper SSE formatting and streaming
    SseKeepAlive::new(stream).stream(res);
}
