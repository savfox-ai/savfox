use http::HeaderMap;
use savfox_protocol::models::{ContentItem, FunctionCallOutputContentItem, ResponseItem};
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::provider::Provider;

/// Assembled request body plus headers for Anthropic Messages API streaming calls.
pub struct AnthropicRequest {
    pub body: Value,
    pub headers: HeaderMap,
}

pub struct AnthropicRequestBuilder<'a> {
    model: &'a str,
    instructions: &'a str,
    input: &'a [ResponseItem],
    tools: &'a [Value],
    max_tokens: i64,
}

impl<'a> AnthropicRequestBuilder<'a> {
    #[must_use] 
    pub fn new(
        model: &'a str,
        instructions: &'a str,
        input: &'a [ResponseItem],
        tools: &'a [Value],
        max_tokens: i64,
    ) -> Self {
        Self {
            model,
            instructions,
            input,
            tools,
            max_tokens,
        }
    }

    pub fn build(self, _provider: &Provider) -> Result<AnthropicRequest, ApiError> {
        let messages = build_anthropic_messages(self.input);

        let mut payload = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "stream": true,
            "messages": messages,
        });

        // System instructions go into a top-level "system" field, not into messages.
        if !self.instructions.is_empty() {
            payload["system"] = json!(self.instructions);
        }

        // Only include tools if non-empty.
        if !self.tools.is_empty() {
            payload["tools"] = json!(self.tools);
        }

        Ok(AnthropicRequest {
            body: payload,
            headers: HeaderMap::new(),
        })
    }
}

/// Convert the internal ResponseItem history into Anthropic Messages API messages.
///
/// Anthropic requires strict alternation of user/assistant roles. This function
/// groups consecutive items by their effective role and merges content blocks.
fn build_anthropic_messages(input: &[ResponseItem]) -> Vec<Value> {
    // Collect (role, content_block) pairs from input items.
    let mut role_blocks: Vec<(&str, Value)> = Vec::new();

    for item in input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let anthropic_role = if role == "assistant" {
                    "assistant"
                } else {
                    "user"
                };
                let blocks: Vec<Value> = content
                    .iter()
                    .filter_map(|c| match c {
                        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                            Some(json!({"type": "text", "text": text}))
                        }
                        ContentItem::InputImage { image_url } => {
                            // Anthropic supports base64 images; pass URL-based images as text fallback.
                            if image_url.starts_with("data:") {
                                // data:image/png;base64,<data>
                                let parts: Vec<&str> = image_url.splitn(2, ',').collect();
                                if parts.len() == 2 {
                                    let media_type = parts[0]
                                        .strip_prefix("data:")
                                        .unwrap_or("image/png")
                                        .strip_suffix(";base64")
                                        .unwrap_or("image/png");
                                    Some(json!({
                                        "type": "image",
                                        "source": {
                                            "type": "base64",
                                            "media_type": media_type,
                                            "data": parts[1]
                                        }
                                    }))
                                } else {
                                    Some(json!({"type": "text", "text": format!("[image: {image_url}]")}))
                                }
                            } else {
                                Some(json!({
                                    "type": "image",
                                    "source": {
                                        "type": "url",
                                        "url": image_url
                                    }
                                }))
                            }
                        }
                    })
                    .collect();

                for block in blocks {
                    role_blocks.push((anthropic_role, block));
                }
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                // FunctionCall maps to an assistant message with a tool_use content block.
                let input_value: Value =
                    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
                role_blocks.push((
                    "assistant",
                    json!({
                        "type": "tool_use",
                        "id": call_id,
                        "name": name,
                        "input": input_value
                    }),
                ));
            }
            ResponseItem::FunctionCallOutput { call_id, output } => {
                // FunctionCallOutput maps to a user message with a tool_result content block.
                let content_value = if let Some(items) = &output.content_items {
                    let mapped: Vec<Value> = items
                        .iter()
                        .map(|it| match it {
                            FunctionCallOutputContentItem::InputText { text } => {
                                json!({"type": "text", "text": text})
                            }
                            FunctionCallOutputContentItem::InputImage { image_url } => {
                                json!({"type": "text", "text": format!("[image: {image_url}]")})
                            }
                        })
                        .collect();
                    json!(mapped)
                } else {
                    json!(output.content)
                };

                role_blocks.push((
                    "user",
                    json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": content_value
                    }),
                ));
            }
            // Skip items that have no Anthropic equivalent.
            ResponseItem::Reasoning { .. }
            | ResponseItem::Other
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::CustomToolCallOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::GhostSnapshot { .. }
            | ResponseItem::Compaction { .. } => continue,
        }
    }

    // Now merge consecutive blocks with the same role into single messages.
    let mut messages: Vec<Value> = Vec::new();
    let mut current_role: Option<&str> = None;
    let mut current_content: Vec<Value> = Vec::new();

    for (role, block) in &role_blocks {
        if current_role == Some(role) {
            current_content.push(block.clone());
        } else {
            if let Some(prev_role) = current_role {
                messages.push(json!({
                    "role": prev_role,
                    "content": current_content
                }));
                current_content = Vec::new();
            }
            current_role = Some(role);
            current_content.push(block.clone());
        }
    }

    if let Some(role) = current_role {
        messages.push(json!({
            "role": role,
            "content": current_content
        }));
    }

    messages
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pretty_assertions::assert_eq;
    use savfox_protocol::models::FunctionCallOutputPayload;

    use super::*;
    use crate::provider::{Provider, RetryConfig, WireApi};

    fn provider() -> Provider {
        Provider {
            id: "anthropic".to_string(),
            name: "anthropic".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            query_params: None,
            wire: WireApi::Anthropic,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(10),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn builds_basic_anthropic_request() {
        let input = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Hello".to_string(),
            }],
            end_turn: None,
            phase: None,
        }];

        let req = AnthropicRequestBuilder::new(
            "claude-sonnet-4-20250514",
            "Be helpful",
            &input,
            &[],
            8192,
        )
        .build(&provider())
        .expect("request");

        assert_eq!(req.body["model"], "claude-sonnet-4-20250514");
        assert_eq!(req.body["max_tokens"], 8192);
        assert_eq!(req.body["stream"], true);
        assert_eq!(req.body["system"], "Be helpful");

        let messages = req.body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert_eq!(messages[0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn merges_consecutive_tool_results_into_single_user_message() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "read files".to_string(),
                }],
                end_turn: None,
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "read_file".to_string(),
                arguments: r#"{"path":"a.txt"}"#.to_string(),
                call_id: "call-a".to_string(),
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "read_file".to_string(),
                arguments: r#"{"path":"b.txt"}"#.to_string(),
                call_id: "call-b".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "call-a".to_string(),
                output: FunctionCallOutputPayload {
                    content: "content A".to_string(),
                    ..Default::default()
                },
            },
            ResponseItem::FunctionCallOutput {
                call_id: "call-b".to_string(),
                output: FunctionCallOutputPayload {
                    content: "content B".to_string(),
                    ..Default::default()
                },
            },
        ];

        let req = AnthropicRequestBuilder::new("claude-sonnet-4-20250514", "", &input, &[], 4096)
            .build(&provider())
            .expect("request");

        let messages = req.body["messages"].as_array().expect("messages");
        // user(text) + assistant(tool_use x2) + user(tool_result x2)
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        // Two tool_use blocks merged into one assistant message
        assert_eq!(messages[1]["content"].as_array().unwrap().len(), 2);
        assert_eq!(messages[2]["role"], "user");
        // Two tool_result blocks merged into one user message
        assert_eq!(messages[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "call-a");
        assert_eq!(messages[2]["content"][1]["type"], "tool_result");
        assert_eq!(messages[2]["content"][1]["tool_use_id"], "call-b");
    }
}
