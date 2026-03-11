use serde_json::{Value, json};

use crate::client_common::tools::ToolSpec;

/// Returns JSON values that are compatible with Function Calling in the
/// Responses API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=responses
pub fn create_tools_json_for_responses_api(tools: &[ToolSpec]) -> crate::error::Result<Vec<Value>> {
    let mut tools_json = Vec::with_capacity(tools.len());

    for tool in tools {
        tools_json.push(serde_json::to_value(tool)?);
    }

    Ok(tools_json)
}

/// Returns JSON values that are compatible with Function Calling in the
/// Chat Completions API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=chat
pub(crate) fn create_tools_json_for_chat_completions_api(
    tools: &[ToolSpec],
) -> crate::error::Result<Vec<Value>> {
    // Start from the Responses API format and rewrite it into the chat
    // completions function wrapper shape.
    let responses_api_tools_json = create_tools_json_for_responses_api(tools)?;
    let tools_json = responses_api_tools_json
        .into_iter()
        .filter_map(|mut tool| {
            if tool.get("type") != Some(&Value::String("function".to_string())) {
                return None;
            }

            if let Some(map) = tool.as_object_mut() {
                let name = map
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                map.remove("type");
                Some(json!({
                    "type": "function",
                    "name": name,
                    "function": map,
                }))
            } else {
                None
            }
        })
        .collect();
    Ok(tools_json)
}

/// Returns JSON values that are compatible with the Anthropic Messages API
/// tool format: `{"name": "...", "description": "...", "input_schema": {...}}`
pub(crate) fn create_tools_json_for_anthropic_api(
    tools: &[ToolSpec],
) -> crate::error::Result<Vec<Value>> {
    let responses_api_tools_json = create_tools_json_for_responses_api(tools)?;
    let tools_json = responses_api_tools_json
        .into_iter()
        .filter_map(|tool| {
            if tool.get("type") != Some(&Value::String("function".to_string())) {
                return None;
            }

            let name = tool
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let description = tool
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let input_schema = tool
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));

            Some(json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            }))
        })
        .collect();
    Ok(tools_json)
}
