use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, JsonObject, Tool};
use serde::Deserialize;
use serde_json::json;

pub(super) fn tool() -> Tool {
    #[expect(clippy::expect_used)]
    let schema: JsonObject = serde_json::from_value(json!({
        "type": "object",
        "properties": {
            "message": { "type": "string" },
            "env_var": { "type": "string" }
        },
        "required": ["message"],
        "additionalProperties": false
    }))
    .expect("echo tool schema should deserialize");

    Tool::new(
        Cow::Borrowed("echo"),
        Cow::Borrowed("Echo back the provided message and include environment data."),
        Arc::new(schema),
    )
}

#[derive(Deserialize)]
struct EchoArgs {
    message: String,
    #[allow(dead_code)]
    env_var: Option<String>,
}

pub(super) fn call(
    request: CallToolRequestParams,
    prefix: &str,
) -> Result<CallToolResponse, McpError> {
    let args: EchoArgs = match request.arguments {
        Some(arguments) => {
            serde_json::from_value(serde_json::Value::Object(arguments.into_iter().collect()))
                .map_err(|err| McpError::invalid_params(err.to_string(), None))?
        }
        None => {
            return Err(McpError::invalid_params(
                "missing arguments for echo tool",
                None,
            ));
        }
    };

    let env_snapshot: HashMap<String, String> = std::env::vars().collect();
    let structured_content = json!({
        "echo": format!("{prefix}{}", args.message),
        "env": env_snapshot.get("MCP_TEST_VALUE"),
    });

    Ok(CallToolResult::structured(structured_content).into())
}
