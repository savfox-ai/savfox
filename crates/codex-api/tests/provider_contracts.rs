//! Provider contract tests: verify the request/response format produced by each
//! wire API (Chat Completions, Responses, Anthropic).
//!
//! These tests do **not** hit real endpoints. Instead they exercise the
//! request-builder and SSE-parser layers to ensure the JSON shapes and header
//! logic are correct.

use std::time::Duration;

use http::HeaderMap;
use savfox_api::provider::{Provider, RetryConfig, WireApi};
use savfox_protocol::models::{ContentItem, ResponseItem};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Helpers shared across modules
// ---------------------------------------------------------------------------

fn retry_config() -> RetryConfig {
    RetryConfig {
        max_attempts: 3,
        base_delay: Duration::from_millis(200),
        retry_429: false,
        retry_5xx: true,
        retry_transport: true,
    }
}

fn openai_provider() -> Provider {
    Provider {
        name: "openai".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        query_params: None,
        wire: WireApi::Responses,
        headers: HeaderMap::new(),
        retry: retry_config(),
        stream_idle_timeout: Duration::from_secs(300),
    }
}

fn chat_provider() -> Provider {
    Provider {
        name: "openai".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        query_params: None,
        wire: WireApi::Chat,
        headers: HeaderMap::new(),
        retry: retry_config(),
        stream_idle_timeout: Duration::from_secs(300),
    }
}

fn anthropic_provider() -> Provider {
    Provider {
        name: "anthropic".to_string(),
        base_url: "https://api.anthropic.com".to_string(),
        query_params: None,
        wire: WireApi::Anthropic,
        headers: HeaderMap::new(),
        retry: retry_config(),
        stream_idle_timeout: Duration::from_secs(300),
    }
}

fn simple_user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

fn simple_assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

fn sample_tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_file",
            "description": "Read a file from the filesystem",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }
        }
    })
}

// ===========================================================================
// 1. REQUEST FORMAT TESTS
// ===========================================================================

mod request_format {
    use pretty_assertions::assert_eq;
    use savfox_api::requests::anthropic::AnthropicRequestBuilder;
    use savfox_api::requests::chat::ChatRequestBuilder;
    use savfox_api::requests::responses::ResponsesRequestBuilder;
    use savfox_protocol::models::FunctionCallOutputPayload;
    use serde_json::json;

    use super::*;

    // -----------------------------------------------------------------------
    // 1a. Chat Completions
    // -----------------------------------------------------------------------

    #[test]
    fn chat_request_body_shape() {
        let input = vec![simple_user_message("Hello, world")];
        let tools = vec![sample_tool_definition()];

        let req = ChatRequestBuilder::new("gpt-4o", "You are helpful.", &input, &tools)
            .build(&chat_provider())
            .expect("build chat request");

        let body = &req.body;

        // Top-level fields
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["stream"], true);

        // Tools must be present
        let tools_arr = body["tools"].as_array().expect("tools array");
        assert_eq!(tools_arr.len(), 1);
        assert_eq!(tools_arr[0]["function"]["name"], "read_file");

        // Messages array: system + user
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello, world");
    }

    #[test]
    fn chat_request_always_has_system_message() {
        let input = vec![simple_user_message("hi")];

        let req = ChatRequestBuilder::new("gpt-4o", "", &input, &[])
            .build(&chat_provider())
            .expect("build request");

        let messages = req.body["messages"].as_array().expect("messages");
        assert_eq!(messages[0]["role"], "system");
        // Instructions can be empty string
        assert_eq!(messages[0]["content"], "");
    }

    #[test]
    fn chat_request_function_call_uses_tool_calls_array() {
        let input = vec![
            simple_user_message("read file"),
            savfox_protocol::models::ResponseItem::FunctionCall {
                id: None,
                name: "read_file".to_string(),
                arguments: r#"{"path":"main.rs"}"#.to_string(),
                call_id: "call-1".to_string(),
            },
            savfox_protocol::models::ResponseItem::FunctionCallOutput {
                call_id: "call-1".to_string(),
                output: FunctionCallOutputPayload {
                    content: "fn main() {}".to_string(),
                    ..Default::default()
                },
            },
        ];

        let req = ChatRequestBuilder::new("gpt-4o", "inst", &input, &[])
            .build(&chat_provider())
            .expect("build");

        let messages = req.body["messages"].as_array().unwrap();
        // system + user + assistant(tool_calls) + tool
        assert_eq!(messages.len(), 4);

        let assistant_msg = &messages[2];
        assert_eq!(assistant_msg["role"], "assistant");
        assert!(assistant_msg["content"].is_null());
        let tc = assistant_msg["tool_calls"].as_array().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0]["id"], "call-1");
        assert_eq!(tc[0]["function"]["name"], "read_file");
        assert_eq!(tc[0]["function"]["arguments"], r#"{"path":"main.rs"}"#);

        let tool_msg = &messages[3];
        assert_eq!(tool_msg["role"], "tool");
        assert_eq!(tool_msg["tool_call_id"], "call-1");
        assert_eq!(tool_msg["content"], "fn main() {}");
    }

    // -----------------------------------------------------------------------
    // 1b. Responses API
    // -----------------------------------------------------------------------

    #[test]
    fn responses_request_body_shape() {
        let input = vec![simple_user_message("Hello")];
        let tools = vec![sample_tool_definition()];

        let req = ResponsesRequestBuilder::new("gpt-4o", "Be helpful.", &input)
            .tools(&tools)
            .build(&openai_provider())
            .expect("build responses request");

        let body = &req.body;

        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["instructions"], "Be helpful.");
        assert_eq!(body["stream"], true);
        assert_eq!(body["tool_choice"], "auto");

        // Input must be an array of ResponseItem
        let input_arr = body["input"].as_array().expect("input array");
        assert_eq!(input_arr.len(), 1);
        assert_eq!(input_arr[0]["role"], "user");

        // Tools
        let tools_arr = body["tools"].as_array().expect("tools array");
        assert_eq!(tools_arr.len(), 1);
    }

    #[test]
    fn responses_request_includes_store_field() {
        let input = vec![simple_user_message("Hello")];

        let req = ResponsesRequestBuilder::new("gpt-4o", "inst", &input)
            .build(&openai_provider())
            .expect("build");

        // Non-Azure should default to false
        assert_eq!(req.body["store"], false);
    }

    #[test]
    fn responses_request_azure_defaults_store_true() {
        let azure = Provider {
            name: "azure".to_string(),
            base_url: "https://example.openai.azure.com/v1".to_string(),
            query_params: None,
            wire: WireApi::Responses,
            headers: HeaderMap::new(),
            retry: retry_config(),
            stream_idle_timeout: Duration::from_secs(300),
        };

        let input = vec![simple_user_message("hi")];

        let req = ResponsesRequestBuilder::new("gpt-4o", "inst", &input)
            .build(&azure)
            .expect("build");

        assert_eq!(req.body["store"], true);
    }

    #[test]
    fn responses_request_store_override() {
        let input = vec![simple_user_message("hi")];

        let req = ResponsesRequestBuilder::new("gpt-4o", "inst", &input)
            .store_override(Some(true))
            .build(&openai_provider())
            .expect("build");

        assert_eq!(req.body["store"], true);
    }

    // -----------------------------------------------------------------------
    // 1c. Anthropic
    // -----------------------------------------------------------------------

    #[test]
    fn anthropic_request_body_shape() {
        let input = vec![simple_user_message("Hello, Claude")];
        let tools = vec![json!({
            "name": "read_file",
            "description": "Read a file",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }
        })];

        let req = AnthropicRequestBuilder::new(
            "claude-sonnet-4-20250514",
            "You are a helpful assistant.",
            &input,
            &tools,
            8192,
        )
        .build(&anthropic_provider())
        .expect("build anthropic request");

        let body = &req.body;

        assert_eq!(body["model"], "claude-sonnet-4-20250514");
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"], "You are a helpful assistant.");

        // Messages array (no system role -- it goes in top-level "system")
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert_eq!(messages[0]["content"][0]["text"], "Hello, Claude");

        // Tools present
        let tools_arr = body["tools"].as_array().expect("tools array");
        assert_eq!(tools_arr.len(), 1);
        assert_eq!(tools_arr[0]["name"], "read_file");
    }

    #[test]
    fn anthropic_empty_instructions_omits_system() {
        let input = vec![simple_user_message("hi")];

        let req = AnthropicRequestBuilder::new("claude-sonnet-4-20250514", "", &input, &[], 4096)
            .build(&anthropic_provider())
            .expect("build");

        assert!(req.body.get("system").is_none());
    }

    #[test]
    fn anthropic_empty_tools_omits_tools() {
        let input = vec![simple_user_message("hi")];

        let req =
            AnthropicRequestBuilder::new("claude-sonnet-4-20250514", "inst", &input, &[], 4096)
                .build(&anthropic_provider())
                .expect("build");

        assert!(req.body.get("tools").is_none());
    }

    #[test]
    fn anthropic_tool_use_round_trip() {
        let input = vec![
            simple_user_message("read a file"),
            savfox_protocol::models::ResponseItem::FunctionCall {
                id: None,
                name: "read_file".to_string(),
                arguments: r#"{"path":"a.txt"}"#.to_string(),
                call_id: "toolu_01".to_string(),
            },
            savfox_protocol::models::ResponseItem::FunctionCallOutput {
                call_id: "toolu_01".to_string(),
                output: FunctionCallOutputPayload {
                    content: "file contents".to_string(),
                    ..Default::default()
                },
            },
        ];

        let req =
            AnthropicRequestBuilder::new("claude-sonnet-4-20250514", "inst", &input, &[], 4096)
                .build(&anthropic_provider())
                .expect("build");

        let messages = req.body["messages"].as_array().unwrap();
        // user(text) + assistant(tool_use) + user(tool_result)
        assert_eq!(messages.len(), 3);

        assert_eq!(messages[0]["role"], "user");

        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[1]["content"][0]["id"], "toolu_01");
        assert_eq!(messages[1]["content"][0]["name"], "read_file");

        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_01");
    }
}

// ===========================================================================
// 2. SSE PARSING TESTS
// ===========================================================================

mod sse_parsing {
    use std::time::Duration;

    use assert_matches::assert_matches;
    use futures::TryStreamExt;
    use pretty_assertions::assert_eq;
    use savfox_api::common::ResponseEvent;
    use savfox_api::error::ApiError;
    use savfox_api::sse::responses::{ResponsesStreamEvent, process_responses_event};
    use savfox_protocol::models::{ContentItem, ResponseItem};
    use serde_json::json;
    use tokio::sync::mpsc;
    use tokio_util::io::ReaderStream;

    // -----------------------------------------------------------------------
    // Helpers for Chat SSE
    // -----------------------------------------------------------------------

    fn build_chat_body(events: &[serde_json::Value]) -> String {
        let mut body = String::new();
        for e in events {
            body.push_str(&format!("event: message\ndata: {e}\n\n"));
        }
        body
    }

    async fn collect_chat_events(body: &str) -> Vec<ResponseEvent> {
        let reader = ReaderStream::new(std::io::Cursor::new(body.to_string()))
            .map_err(|err| savfox_client::TransportError::Network(err.to_string()));
        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(64);
        tokio::spawn(savfox_api::sse::chat::process_chat_sse(
            reader,
            tx,
            Duration::from_millis(2000),
            None,
        ));

        let mut out = Vec::new();
        while let Some(ev) = rx.recv().await {
            out.push(ev.expect("stream error"));
        }
        out
    }

    // -----------------------------------------------------------------------
    // Helpers for Anthropic SSE
    // -----------------------------------------------------------------------

    fn build_anthropic_body(events: &[(&str, &str)]) -> String {
        let mut body = String::new();
        for (event_type, data) in events {
            body.push_str(&format!("event: {event_type}\ndata: {data}\n\n"));
        }
        body
    }

    async fn collect_anthropic_events(body: &str) -> Vec<ResponseEvent> {
        let reader = ReaderStream::new(std::io::Cursor::new(body.to_string()))
            .map_err(|err| savfox_client::TransportError::Network(err.to_string()));
        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(64);
        tokio::spawn(savfox_api::sse::anthropic::process_anthropic_sse(
            reader,
            tx,
            Duration::from_millis(2000),
            None,
        ));

        let mut out = Vec::new();
        while let Some(ev) = rx.recv().await {
            out.push(ev.expect("stream error"));
        }
        out
    }

    // -----------------------------------------------------------------------
    // 2a. Chat Completions SSE
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn chat_sse_parses_content_delta_stream() {
        let body = build_chat_body(&[
            json!({
                "choices": [{
                    "delta": { "content": "Hello" }
                }]
            }),
            json!({
                "choices": [{
                    "delta": { "content": " world" }
                }]
            }),
            json!({
                "choices": [{
                    "finish_reason": "stop"
                }]
            }),
        ]);

        let events = collect_chat_events(&body).await;

        // OutputItemAdded (assistant) + delta "Hello" + delta " world" + OutputItemDone + Completed
        assert!(events.len() >= 4);
        assert_matches!(&events[0], ResponseEvent::OutputItemAdded(ResponseItem::Message { role, .. }) if role == "assistant");
        assert_matches!(&events[1], ResponseEvent::OutputTextDelta(d) if d == "Hello");
        assert_matches!(&events[2], ResponseEvent::OutputTextDelta(d) if d == " world");
        assert_matches!(
            &events[3],
            ResponseEvent::OutputItemDone(ResponseItem::Message { .. })
        );
        assert_matches!(&events[4], ResponseEvent::Completed { .. });
    }

    #[tokio::test]
    async fn chat_sse_parses_tool_calls_stream() {
        let body = build_chat_body(&[
            json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "id": "call_abc",
                            "index": 0,
                            "function": { "name": "read_file", "arguments": "" }
                        }]
                    }
                }]
            }),
            json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": { "arguments": "{\"path\":" }
                        }]
                    }
                }]
            }),
            json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": { "arguments": "\"test.rs\"}" }
                        }]
                    }
                }]
            }),
            json!({
                "choices": [{
                    "finish_reason": "tool_calls"
                }]
            }),
        ]);

        let events = collect_chat_events(&body).await;

        assert_matches!(
            &events[0],
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { call_id, name, arguments, .. })
            if call_id == "call_abc" && name == "read_file" && arguments == "{\"path\":\"test.rs\"}"
        );
        assert_matches!(&events[1], ResponseEvent::Completed { .. });
    }

    #[tokio::test]
    async fn chat_sse_handles_done_sentinel() {
        let body = "event: message\ndata: [DONE]\n\n";
        let events = collect_chat_events(body).await;
        assert_matches!(&events[0], ResponseEvent::Completed { .. });
    }

    #[tokio::test]
    async fn chat_sse_context_length_returns_error() {
        let body = build_chat_body(&[json!({
            "choices": [{
                "finish_reason": "length"
            }]
        })]);

        let reader = ReaderStream::new(std::io::Cursor::new(body))
            .map_err(|err| savfox_client::TransportError::Network(err.to_string()));
        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(16);
        tokio::spawn(savfox_api::sse::chat::process_chat_sse(
            reader,
            tx,
            Duration::from_millis(2000),
            None,
        ));

        let result = rx.recv().await.unwrap();
        assert_matches!(result, Err(ApiError::ContextWindowExceeded));
    }

    // -----------------------------------------------------------------------
    // 2b. Responses API SSE
    // -----------------------------------------------------------------------

    #[test]
    fn responses_sse_parses_output_item_done() {
        let event: ResponsesStreamEvent = serde_json::from_value(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Hello from Responses"}]
            }
        }))
        .expect("deserialize event");

        let result = process_responses_event(event);
        match result {
            Ok(Some(ResponseEvent::OutputItemDone(ResponseItem::Message {
                role,
                content,
                ..
            }))) => {
                assert_eq!(role, "assistant");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentItem::OutputText { text } => assert_eq!(text, "Hello from Responses"),
                    other => panic!("unexpected content item: {other:?}"),
                }
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn responses_sse_parses_text_delta() {
        let event: ResponsesStreamEvent = serde_json::from_value(json!({
            "type": "response.output_text.delta",
            "delta": "Hello"
        }))
        .expect("deserialize event");

        let result = process_responses_event(event);
        assert_matches!(result, Ok(Some(ResponseEvent::OutputTextDelta(d))) if d == "Hello");
    }

    #[test]
    fn responses_sse_parses_completed() {
        let event: ResponsesStreamEvent = serde_json::from_value(json!({
            "type": "response.completed",
            "response": {
                "id": "resp-123",
                "usage": {
                    "input_tokens": 100,
                    "input_tokens_details": null,
                    "output_tokens": 50,
                    "output_tokens_details": null,
                    "total_tokens": 150
                }
            }
        }))
        .expect("deserialize event");

        let result = process_responses_event(event);
        match result {
            Ok(Some(ResponseEvent::Completed {
                response_id,
                token_usage,
            })) => {
                assert_eq!(response_id, "resp-123");
                let usage = token_usage.unwrap();
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 50);
                assert_eq!(usage.total_tokens, 150);
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn responses_sse_parses_function_call_done() {
        let event: ResponsesStreamEvent = serde_json::from_value(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "name": "read_file",
                "arguments": "{\"path\":\"main.rs\"}",
                "call_id": "call_xyz"
            }
        }))
        .expect("deserialize event");

        let result = process_responses_event(event);
        match result {
            Ok(Some(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            }))) => {
                assert_eq!(name, "read_file");
                assert_eq!(arguments, "{\"path\":\"main.rs\"}");
                assert_eq!(call_id, "call_xyz");
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn responses_sse_parses_created() {
        let event: ResponsesStreamEvent = serde_json::from_value(json!({
            "type": "response.created",
            "response": {}
        }))
        .expect("deserialize event");

        let result = process_responses_event(event);
        assert_matches!(result, Ok(Some(ResponseEvent::Created)));
    }

    #[test]
    fn responses_sse_parses_reasoning_summary_delta() {
        let event: ResponsesStreamEvent = serde_json::from_value(json!({
            "type": "response.reasoning_summary_text.delta",
            "delta": "thinking step",
            "summary_index": 0
        }))
        .expect("deserialize event");

        let result = process_responses_event(event);
        assert_matches!(
            result,
            Ok(Some(ResponseEvent::ReasoningSummaryDelta { delta, summary_index }))
            if delta == "thinking step" && summary_index == 0
        );
    }

    #[test]
    fn responses_sse_unknown_event_returns_none() {
        let event: ResponsesStreamEvent = serde_json::from_value(json!({
            "type": "response.some_future_event"
        }))
        .expect("deserialize event");

        let result = process_responses_event(event);
        assert_matches!(result, Ok(None));
    }

    // -----------------------------------------------------------------------
    // 2c. Anthropic SSE
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn anthropic_sse_parses_text_response() {
        let body = build_anthropic_body(&[
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","usage":{"input_tokens":15,"output_tokens":0}}}"#,
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
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" from Claude"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":8}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);

        let events = collect_anthropic_events(&body).await;

        assert_matches!(&events[0], ResponseEvent::Created);
        assert_matches!(&events[1], ResponseEvent::OutputItemAdded(ResponseItem::Message { role, .. }) if role == "assistant");
        assert_matches!(&events[2], ResponseEvent::OutputTextDelta(d) if d == "Hello");
        assert_matches!(&events[3], ResponseEvent::OutputTextDelta(d) if d == " from Claude");
        assert_matches!(
            &events[4],
            ResponseEvent::OutputItemDone(ResponseItem::Message { .. })
        );
        assert_matches!(&events[5], ResponseEvent::Completed { token_usage, .. } => {
            let usage = token_usage.as_ref().unwrap();
            assert_eq!(usage.input_tokens, 15);
            assert_eq!(usage.output_tokens, 8);
        });
    }

    #[tokio::test]
    async fn anthropic_sse_parses_tool_use_response() {
        let body = build_anthropic_body(&[
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_02","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","usage":{"input_tokens":20,"output_tokens":0}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_abc","name":"read_file"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"test.rs\"}"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);

        let events = collect_anthropic_events(&body).await;

        assert_matches!(&events[0], ResponseEvent::Created);
        assert_matches!(
            &events[1],
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { call_id, name, arguments, .. })
            if call_id == "toolu_abc" && name == "read_file" && arguments == "{\"path\":\"test.rs\"}"
        );
        assert_matches!(&events[2], ResponseEvent::Completed { .. });
    }

    #[tokio::test]
    async fn anthropic_sse_handles_error_event() {
        let body = build_anthropic_body(&[(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"API is overloaded"}}"#,
        )]);

        let reader = ReaderStream::new(std::io::Cursor::new(body))
            .map_err(|err| savfox_client::TransportError::Network(err.to_string()));
        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(16);
        tokio::spawn(savfox_api::sse::anthropic::process_anthropic_sse(
            reader,
            tx,
            Duration::from_millis(2000),
            None,
        ));

        let result = rx.recv().await.unwrap();
        assert_matches!(result, Err(ApiError::Stream(msg)) if msg.contains("overloaded"));
    }

    #[tokio::test]
    async fn anthropic_sse_parses_thinking_response() {
        let body = build_anthropic_body(&[
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_03","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","usage":{"input_tokens":10,"output_tokens":0}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think..."}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"The answer is 42."}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":1}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);

        let events = collect_anthropic_events(&body).await;

        assert_matches!(&events[0], ResponseEvent::Created);
        // Thinking events
        assert_matches!(
            &events[1],
            ResponseEvent::OutputItemAdded(ResponseItem::Reasoning { .. })
        );
        assert_matches!(&events[2], ResponseEvent::ReasoningContentDelta { delta, .. } if delta == "Let me think...");
        assert_matches!(
            &events[3],
            ResponseEvent::OutputItemDone(ResponseItem::Reasoning { .. })
        );
        // Text events
        assert_matches!(&events[4], ResponseEvent::OutputItemAdded(ResponseItem::Message { role, .. }) if role == "assistant");
        assert_matches!(&events[5], ResponseEvent::OutputTextDelta(d) if d == "The answer is 42.");
        assert_matches!(
            &events[6],
            ResponseEvent::OutputItemDone(ResponseItem::Message { .. })
        );
        assert_matches!(&events[7], ResponseEvent::Completed { .. });
    }
}

// ===========================================================================
// 3. PROVIDER CONFIGURATION TESTS
// ===========================================================================

mod provider_config {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn provider_url_construction() {
        let p = openai_provider();
        assert_eq!(
            p.url_for_path("/responses"),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            p.url_for_path("/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn provider_url_with_query_params() {
        let mut p = openai_provider();
        let mut params = std::collections::HashMap::new();
        params.insert("api-version".to_string(), "2025-04-01".to_string());
        p.query_params = Some(params);

        let url = p.url_for_path("/responses");
        assert!(
            url.contains("api-version=2025-04-01"),
            "expected query param in URL: {url}"
        );
    }

    #[test]
    fn anthropic_provider_wire_api() {
        let p = anthropic_provider();
        assert_eq!(p.wire, WireApi::Anthropic);
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn chat_provider_wire_api() {
        let p = chat_provider();
        assert_eq!(p.wire, WireApi::Chat);
    }

    #[test]
    fn responses_provider_wire_api() {
        let p = openai_provider();
        assert_eq!(p.wire, WireApi::Responses);
    }

    #[test]
    fn retry_config_to_policy_conversion() {
        let config = RetryConfig {
            max_attempts: 5,
            base_delay: Duration::from_millis(200),
            retry_429: false,
            retry_5xx: true,
            retry_transport: true,
        };

        let policy = config.to_policy();
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.base_delay, Duration::from_millis(200));
        assert!(!policy.retry_on.retry_429);
        assert!(policy.retry_on.retry_5xx);
        assert!(policy.retry_on.retry_transport);
    }

    #[test]
    fn retry_config_defaults_sensible() {
        let config = retry_config();
        assert_eq!(config.max_attempts, 3);
        assert_eq!(config.base_delay, Duration::from_millis(200));
        assert!(config.retry_5xx);
        assert!(config.retry_transport);
    }

    #[test]
    fn azure_detection_by_name() {
        let p = Provider {
            name: "azure".to_string(),
            base_url: "https://example.com".to_string(),
            query_params: None,
            wire: WireApi::Responses,
            headers: HeaderMap::new(),
            retry: retry_config(),
            stream_idle_timeout: Duration::from_secs(300),
        };
        assert!(p.is_azure_responses_endpoint());
    }

    #[test]
    fn azure_detection_by_url() {
        let p = Provider {
            name: "custom".to_string(),
            base_url: "https://foo.openai.azure.com/openai".to_string(),
            query_params: None,
            wire: WireApi::Responses,
            headers: HeaderMap::new(),
            retry: retry_config(),
            stream_idle_timeout: Duration::from_secs(300),
        };
        assert!(p.is_azure_responses_endpoint());
    }

    #[test]
    fn non_azure_provider_detection() {
        assert!(!openai_provider().is_azure_responses_endpoint());
        assert!(!anthropic_provider().is_azure_responses_endpoint());
    }

    #[test]
    fn websocket_url_construction() {
        let p = openai_provider();
        let ws_url = p.websocket_url_for_path("/realtime").expect("parse ws url");
        assert_eq!(ws_url.scheme(), "wss");
        assert_eq!(ws_url.host_str(), Some("api.openai.com"));
        assert!(ws_url.path().contains("realtime"));
    }

    #[test]
    fn websocket_url_http_becomes_ws() {
        let p = Provider {
            name: "local".to_string(),
            base_url: "http://localhost:8080/v1".to_string(),
            query_params: None,
            wire: WireApi::Responses,
            headers: HeaderMap::new(),
            retry: retry_config(),
            stream_idle_timeout: Duration::from_secs(5),
        };
        let ws_url = p.websocket_url_for_path("/realtime").expect("parse ws url");
        assert_eq!(ws_url.scheme(), "ws");
    }
}

// ===========================================================================
// 4. ROUND-TRIP TESTS
// ===========================================================================

mod round_trip {
    use pretty_assertions::assert_eq;
    use savfox_api::common::{Reasoning, ResponsesApiRequest};
    use savfox_api::requests::anthropic::AnthropicRequestBuilder;
    use savfox_api::requests::chat::ChatRequestBuilder;
    use savfox_api::requests::responses::ResponsesRequestBuilder;
    use savfox_protocol::models::ContentItem;

    use super::*;

    #[test]
    fn chat_request_serialize_deserialize_round_trip() {
        let input = vec![
            simple_user_message("What is 2+2?"),
            simple_assistant_message("4"),
            simple_user_message("And 3+3?"),
        ];
        let tools = vec![sample_tool_definition()];

        let req = ChatRequestBuilder::new("gpt-4o", "You are a math tutor.", &input, &tools)
            .build(&chat_provider())
            .expect("build");

        // Serialize to string and back
        let json_string = serde_json::to_string(&req.body).expect("serialize");
        let deserialized: Value = serde_json::from_str(&json_string).expect("deserialize");

        // Key fields preserved
        assert_eq!(deserialized["model"], "gpt-4o");
        assert_eq!(deserialized["stream"], true);
        assert_eq!(
            deserialized["messages"].as_array().expect("messages").len(),
            4 // system + user + assistant + user
        );
        assert_eq!(deserialized["tools"].as_array().expect("tools").len(), 1);
    }

    #[test]
    fn responses_request_serialize_deserialize_round_trip() {
        let input = vec![simple_user_message("Hello")];
        let tools = vec![sample_tool_definition()];

        let req = ResponsesRequestBuilder::new("gpt-4o", "Be concise.", &input)
            .tools(&tools)
            .build(&openai_provider())
            .expect("build");

        let json_string = serde_json::to_string(&req.body).expect("serialize");
        let deserialized: Value = serde_json::from_str(&json_string).expect("deserialize");

        assert_eq!(deserialized["model"], "gpt-4o");
        assert_eq!(deserialized["instructions"], "Be concise.");
        assert_eq!(deserialized["stream"], true);
        assert_eq!(deserialized["tool_choice"], "auto");
        assert!(deserialized["input"].is_array());
        assert!(deserialized["tools"].is_array());
    }

    #[test]
    fn anthropic_request_serialize_deserialize_round_trip() {
        let input = vec![
            simple_user_message("Hello"),
            simple_assistant_message("Hi there!"),
            simple_user_message("How are you?"),
        ];

        let req = AnthropicRequestBuilder::new(
            "claude-sonnet-4-20250514",
            "Be friendly.",
            &input,
            &[],
            4096,
        )
        .build(&anthropic_provider())
        .expect("build");

        let json_string = serde_json::to_string(&req.body).expect("serialize");
        let deserialized: Value = serde_json::from_str(&json_string).expect("deserialize");

        assert_eq!(deserialized["model"], "claude-sonnet-4-20250514");
        assert_eq!(deserialized["max_tokens"], 4096);
        assert_eq!(deserialized["stream"], true);
        assert_eq!(deserialized["system"], "Be friendly.");

        let messages = deserialized["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[2]["role"], "user");
    }

    #[test]
    fn anthropic_role_alternation_is_preserved() {
        // Anthropic requires strict user/assistant alternation
        let input = vec![
            simple_user_message("Question 1"),
            simple_assistant_message("Answer 1"),
            simple_user_message("Question 2"),
            simple_assistant_message("Answer 2"),
        ];

        let req =
            AnthropicRequestBuilder::new("claude-sonnet-4-20250514", "inst", &input, &[], 4096)
                .build(&anthropic_provider())
                .expect("build");

        let messages = req.body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);

        let expected_roles = ["user", "assistant", "user", "assistant"];
        for (i, expected_role) in expected_roles.iter().enumerate() {
            assert_eq!(
                messages[i]["role"], *expected_role,
                "message {i} role mismatch"
            );
        }
    }

    #[test]
    fn chat_request_with_image_content() {
        let input = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![
                ContentItem::InputText {
                    text: "What is in this image?".to_string(),
                },
                ContentItem::InputImage {
                    image_url: "https://example.com/cat.jpg".to_string(),
                },
            ],
            end_turn: None,
            phase: None,
        }];

        let req = ChatRequestBuilder::new("gpt-4o", "inst", &input, &[])
            .build(&chat_provider())
            .expect("build");

        let messages = req.body["messages"].as_array().unwrap();
        // system + user
        assert_eq!(messages.len(), 2);

        let user_msg = &messages[1];
        assert_eq!(user_msg["role"], "user");
        // Image messages produce array content
        let content = user_msg["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(
            content[1]["image_url"]["url"],
            "https://example.com/cat.jpg"
        );
    }

    #[test]
    fn anthropic_request_with_image_content_url() {
        let input = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputImage {
                image_url: "https://example.com/cat.jpg".to_string(),
            }],
            end_turn: None,
            phase: None,
        }];

        let req =
            AnthropicRequestBuilder::new("claude-sonnet-4-20250514", "inst", &input, &[], 4096)
                .build(&anthropic_provider())
                .expect("build");

        let messages = req.body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);

        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "url");
        assert_eq!(content[0]["source"]["url"], "https://example.com/cat.jpg");
    }

    #[test]
    fn anthropic_request_with_base64_image() {
        let input = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputImage {
                image_url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
            }],
            end_turn: None,
            phase: None,
        }];

        let req =
            AnthropicRequestBuilder::new("claude-sonnet-4-20250514", "inst", &input, &[], 4096)
                .build(&anthropic_provider())
                .expect("build");

        let messages = req.body["messages"].as_array().unwrap();
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[0]["source"]["data"], "iVBORw0KGgo=");
    }

    #[test]
    fn responses_api_request_serde_matches_spec() {
        // Verify that ResponsesApiRequest serializes all expected fields
        let input = vec![simple_user_message("hi")];
        let tools: Vec<Value> = vec![sample_tool_definition()];

        let req = ResponsesApiRequest {
            model: "gpt-4o",
            instructions: "Be helpful",
            input: &input,
            tools: &tools,
            tool_choice: "auto",
            parallel_tool_calls: true,
            reasoning: Some(Reasoning {
                effort: None,
                summary: None,
            }),
            store: false,
            stream: true,
            include: vec!["reasoning.encrypted_content".to_string()],
            prompt_cache_key: Some("cache-1".to_string()),
            text: None,
        };

        let value = serde_json::to_value(&req).expect("serialize");

        assert_eq!(value["model"], "gpt-4o");
        assert_eq!(value["instructions"], "Be helpful");
        assert_eq!(value["tool_choice"], "auto");
        assert_eq!(value["parallel_tool_calls"], true);
        assert_eq!(value["store"], false);
        assert_eq!(value["stream"], true);
        assert_eq!(value["prompt_cache_key"], "cache-1");
        assert!(value["reasoning"].is_object());
        assert!(value["include"].as_array().unwrap().len() == 1);
    }

    #[test]
    fn responses_api_request_omits_optional_nulls() {
        let input = vec![simple_user_message("hi")];
        let tools: Vec<Value> = vec![];

        let req = ResponsesApiRequest {
            model: "gpt-4o",
            instructions: "inst",
            input: &input,
            tools: &tools,
            tool_choice: "auto",
            parallel_tool_calls: false,
            reasoning: None,
            store: false,
            stream: true,
            include: vec![],
            prompt_cache_key: None,
            text: None,
        };

        let value = serde_json::to_value(&req).expect("serialize");

        // prompt_cache_key and text should be absent (skip_serializing_if)
        assert!(value.get("prompt_cache_key").is_none());
        assert!(value.get("text").is_none());
    }
}
