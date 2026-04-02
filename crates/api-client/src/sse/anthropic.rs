use std::sync::{Arc, OnceLock};
use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use savfox_http_client::StreamResponse;
use savfox_protocol::models::{ContentItem, ReasoningItemContent, ResponseItem};
use savfox_protocol::protocol::TokenUsage;
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout};
use tracing::{debug, trace};

use crate::common::{ResponseEvent, ResponseStream};
use crate::error::ApiError;
use crate::telemetry::SseTelemetry;

pub(crate) fn spawn_anthropic_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    _turn_state: Option<Arc<OnceLock<String>>>,
) -> ResponseStream {
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(async move {
        process_anthropic_sse(stream_response.bytes, tx_event, idle_timeout, telemetry).await;
    });
    ResponseStream { rx_event }
}

/// State for accumulating a tool_use content block across deltas.
#[derive(Debug, Default)]
struct ToolAccumulator {
    id: String,
    name: String,
    arguments: String,
}

/// Processes Server-Sent Events from the Anthropic Messages streaming API.
///
/// Anthropic SSE events use a different protocol than OpenAI:
/// - Events are typed via the `event:` field (message_start, content_block_start, etc.)
/// - Each event carries a JSON `data:` payload
/// - The stream ends with `message_stop`
pub async fn process_anthropic_sse<S>(
    stream: S,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) where
    S: Stream<Item = Result<bytes::Bytes, savfox_http_client::TransportError>> + Unpin,
{
    let mut stream = stream.eventsource();

    let mut response_id = String::new();
    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;

    // Current content block tracking
    let mut current_block_type: Option<String> = None;
    let mut tool_accumulator: Option<ToolAccumulator> = None;
    let mut assistant_item: Option<ResponseItem> = None;
    let mut reasoning_item: Option<ResponseItem> = None;

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                let _ = tx_event.send(Err(ApiError::Stream(e.to_string()))).await;
                return;
            }
            Ok(None) => {
                // Stream ended without message_stop — emit Completed anyway.
                emit_completed(
                    &tx_event,
                    &response_id,
                    input_tokens,
                    output_tokens,
                    &mut reasoning_item,
                    &mut assistant_item,
                )
                .await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        let event_type = sse.event.as_str();
        let data = sse.data.trim();

        trace!("Anthropic SSE event: type={event_type}, data={data}");

        if data.is_empty() {
            continue;
        }

        let value: serde_json::Value = match serde_json::from_str(data) {
            Ok(val) => val,
            Err(err) => {
                debug!("Failed to parse Anthropic SSE event: {err}, data: {data}");
                continue;
            }
        };

        match event_type {
            "message_start" => {
                // Extract response ID and input tokens from message metadata.
                if let Some(message) = value.get("message") {
                    if let Some(id) = message.get("id").and_then(|v| v.as_str()) {
                        response_id = id.to_owned();
                    }
                    if let Some(usage) = message.get("usage")
                        && let Some(tokens) = usage.get("input_tokens").and_then(|v| v.as_i64())
                    {
                        input_tokens = tokens;
                    }
                }
                let _ = tx_event.send(Ok(ResponseEvent::Created)).await;
            }

            "content_block_start" => {
                let block_type = value
                    .get("content_block")
                    .and_then(|cb| cb.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");

                current_block_type = Some(block_type.to_owned());

                match block_type {
                    "text" => {
                        let item = ResponseItem::Message {
                            id: None,
                            role: "assistant".to_owned(),
                            content: vec![],
                            end_turn: None,
                            phase: None,
                        };
                        assistant_item = Some(item.clone());
                        let _ = tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                            .await;
                    }
                    "tool_use" => {
                        let cb = value.get("content_block");
                        let id = cb
                            .and_then(|c| c.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_owned();
                        let name = cb
                            .and_then(|c| c.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_owned();
                        tool_accumulator = Some(ToolAccumulator {
                            id,
                            name,
                            arguments: String::new(),
                        });
                    }
                    "thinking" => {
                        let item = ResponseItem::Reasoning {
                            id: String::new(),
                            summary: Vec::new(),
                            content: Some(vec![]),
                            encrypted_content: None,
                        };
                        reasoning_item = Some(item.clone());
                        let _ = tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                            .await;
                    }
                    _ => {}
                }
            }

            "content_block_delta" => {
                let delta_type = value
                    .get("delta")
                    .and_then(|d| d.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");

                match delta_type {
                    "text_delta" => {
                        if let Some(text) = value
                            .get("delta")
                            .and_then(|d| d.get("text"))
                            .and_then(|t| t.as_str())
                        {
                            if let Some(ResponseItem::Message { content, .. }) = &mut assistant_item
                            {
                                if let Some(ContentItem::OutputText { text: existing }) =
                                    content.last_mut()
                                {
                                    existing.push_str(text);
                                } else {
                                    content.push(ContentItem::OutputText {
                                        text: text.to_owned(),
                                    });
                                }
                            }
                            let _ = tx_event
                                .send(Ok(ResponseEvent::OutputTextDelta(text.to_owned())))
                                .await;
                        }
                    }
                    "input_json_delta" => {
                        if let Some(partial) = value
                            .get("delta")
                            .and_then(|d| d.get("partial_json"))
                            .and_then(|t| t.as_str())
                            && let Some(acc) = &mut tool_accumulator
                        {
                            acc.arguments.push_str(partial);
                        }
                    }
                    "thinking_delta" => {
                        if let Some(text) = value
                            .get("delta")
                            .and_then(|d| d.get("thinking"))
                            .and_then(|t| t.as_str())
                            && let Some(ResponseItem::Reasoning {
                                content: Some(content),
                                ..
                            }) = &mut reasoning_item
                        {
                            let content_index = if let Some(last) = content.last_mut() {
                                match last {
                                    ReasoningItemContent::ReasoningText { text: existing }
                                    | ReasoningItemContent::Text { text: existing } => {
                                        existing.push_str(text);
                                        (content.len().saturating_sub(1)) as i64
                                    }
                                }
                            } else {
                                content.push(ReasoningItemContent::ReasoningText {
                                    text: text.to_owned(),
                                });
                                0
                            };
                            let _ = tx_event
                                .send(Ok(ResponseEvent::ReasoningContentDelta {
                                    delta: text.to_owned(),
                                    content_index,
                                }))
                                .await;
                        }
                    }
                    _ => {}
                }
            }

            "content_block_stop" => {
                let block_type = current_block_type.take();

                match block_type.as_deref() {
                    Some("tool_use") => {
                        if let Some(acc) = tool_accumulator.take() {
                            let item = ResponseItem::FunctionCall {
                                id: None,
                                name: acc.name,
                                arguments: acc.arguments,
                                call_id: acc.id,
                            };
                            let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
                        }
                    }
                    Some("text") => {
                        if let Some(item) = assistant_item.take() {
                            let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
                        }
                    }
                    Some("thinking") => {
                        if let Some(item) = reasoning_item.take() {
                            let _ = tx_event.send(Ok(ResponseEvent::OutputItemDone(item))).await;
                        }
                    }
                    _ => {}
                }
            }

            "message_delta" => {
                // Extract output tokens and stop_reason from message_delta.
                if let Some(usage) = value.get("usage")
                    && let Some(tokens) = usage.get("output_tokens").and_then(|v| v.as_i64())
                {
                    output_tokens = tokens;
                }
            }

            "message_stop" => {
                emit_completed(
                    &tx_event,
                    &response_id,
                    input_tokens,
                    output_tokens,
                    &mut reasoning_item,
                    &mut assistant_item,
                )
                .await;
                return;
            }

            "error" => {
                let error_msg = value
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown Anthropic API error");
                let _ = tx_event
                    .send(Err(ApiError::Stream(error_msg.to_owned())))
                    .await;
                return;
            }

            // Ignore ping, etc.
            _ => {}
        }
    }
}

async fn emit_completed(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    response_id: &str,
    input_tokens: i64,
    output_tokens: i64,
    reasoning_item: &mut Option<ResponseItem>,
    assistant_item: &mut Option<ResponseItem>,
) {
    // Flush any outstanding items.
    if let Some(reasoning) = reasoning_item.take() {
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputItemDone(reasoning)))
            .await;
    }
    if let Some(assistant) = assistant_item.take() {
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputItemDone(assistant)))
            .await;
    }

    let token_usage = if input_tokens > 0 || output_tokens > 0 {
        Some(TokenUsage {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
            cached_input_tokens: 0,
            reasoning_output_tokens: 0,
        })
    } else {
        None
    };

    let _ = tx_event
        .send(Ok(ResponseEvent::Completed {
            response_id: response_id.to_owned(),
            token_usage,
        }))
        .await;
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use futures::TryStreamExt;
    use savfox_protocol::models::{ContentItem, ReasoningItemContent, ResponseItem};
    use tokio::sync::mpsc;
    use tokio_util::io::ReaderStream;

    use super::*;

    fn build_sse_body(events: &[(&str, &str)]) -> String {
        let mut body = String::new();
        for (event_type, data) in events {
            body.push_str(&format!("event: {event_type}\ndata: {data}\n\n"));
        }
        body
    }

    async fn collect_events(body: &str) -> Vec<ResponseEvent> {
        let reader = ReaderStream::new(std::io::Cursor::new(body.to_string()))
            .map_err(|err| savfox_http_client::TransportError::Network(err.to_string()));
        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(16);
        tokio::spawn(process_anthropic_sse(
            reader,
            tx,
            Duration::from_millis(1000),
            None,
        ));

        let mut out = Vec::new();
        while let Some(ev) = rx.recv().await {
            out.push(ev.expect("stream error"));
        }
        out
    }

    #[tokio::test]
    async fn parses_text_response() {
        let body = build_sse_body(&[
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);

        let events = collect_events(&body).await;
        assert_matches!(&events[0], ResponseEvent::Created);
        assert_matches!(&events[1], ResponseEvent::OutputItemAdded(ResponseItem::Message { role, .. }) if role == "assistant");
        assert_matches!(&events[2], ResponseEvent::OutputTextDelta(d) if d == "Hello");
        assert_matches!(&events[3], ResponseEvent::OutputTextDelta(d) if d == " world");
        assert_matches!(&events[4], ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
            assert_eq!(content.len(), 1);
            match content.first() {
                Some(ContentItem::OutputText { text }) => assert_eq!(text, "Hello world"),
                other => panic!("expected single OutputText, got {other:?}"),
            }
        });
        assert_matches!(&events[5], ResponseEvent::Completed { token_usage, .. } => {
            let usage = token_usage.as_ref().unwrap();
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 5);
        });
    }

    #[tokio::test]
    async fn coalesces_thinking_deltas_into_single_reasoning_entry() {
        let body = build_sse_body(&[
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_03","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"first "}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"second"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);

        let events = collect_events(&body).await;
        assert_matches!(&events[0], ResponseEvent::Created);
        assert_matches!(
            &events[1],
            ResponseEvent::OutputItemAdded(ResponseItem::Reasoning { .. })
        );
        assert_matches!(
            &events[4],
            ResponseEvent::OutputItemDone(ResponseItem::Reasoning { content: Some(content), .. }) => {
                assert_eq!(content.len(), 1);
                match content.first() {
                    Some(ReasoningItemContent::ReasoningText { text }) => {
                        assert_eq!(text, "first second");
                    }
                    other => panic!("expected single ReasoningText, got {other:?}"),
                }
            }
        );
    }

    #[tokio::test]
    async fn parses_tool_use_response() {
        let body = build_sse_body(&[
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_02","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_01","name":"read_file"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"a.txt\"}"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);

        let events = collect_events(&body).await;
        assert_matches!(&events[0], ResponseEvent::Created);
        assert_matches!(
            &events[1],
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { call_id, name, arguments, .. })
            if call_id == "toolu_01" && name == "read_file" && arguments == "{\"path\":\"a.txt\"}"
        );
        assert_matches!(&events[2], ResponseEvent::Completed { .. });
    }

    #[tokio::test]
    async fn handles_error_event() {
        let body = build_sse_body(&[(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )]);

        let reader = ReaderStream::new(std::io::Cursor::new(body))
            .map_err(|err| savfox_http_client::TransportError::Network(err.to_string()));
        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(16);
        tokio::spawn(process_anthropic_sse(
            reader,
            tx,
            Duration::from_millis(1000),
            None,
        ));

        let result = rx.recv().await.unwrap();
        assert_matches!(result, Err(ApiError::Stream(msg)) if msg.contains("Overloaded"));
    }
}
