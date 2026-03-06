use std::sync::Arc;

use salvo::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::warn;

use crate::auth::GatewayAuth;
use crate::bridge::GatewayChannel;

/// Request body for `POST /tools/invoke`.
#[derive(Debug, Deserialize)]
struct ToolInvokeRequest {
    /// Tool name (e.g., "web_search", "read_file").
    tool: String,
    /// Optional action within the tool (e.g., "status", "navigate" for browser).
    #[serde(default)]
    action: Option<String>,
    /// Tool arguments as a JSON object.
    #[serde(default)]
    arguments: Value,
    /// Optional session key for context.
    #[serde(default)]
    session_id: Option<String>,
}

/// Build a prompt that instructs the agent to invoke a specific tool.
fn build_tool_prompt(tool: &str, action: Option<&str>, arguments: &Value) -> String {
    let mut args = arguments.clone();
    // If an action is specified, merge it into the arguments object.
    if let Some(act) = action {
        if let Value::Object(ref mut map) = args {
            map.entry("action")
                .or_insert_with(|| Value::String(act.to_owned()));
        }
    }

    let args_str = serde_json::to_string_pretty(&args).unwrap_or_else(|_| "{}".to_string());
    format!(
        "Use the `{tool}` tool with exactly these arguments:\n\
         ```json\n{args_str}\n```\n\
         Return only the raw tool output. Do not add commentary."
    )
}

/// `POST /tools/invoke`  - Directly invoke a tool by name.
///
/// Routes the request through the agent engine, which has access to all
/// registered tool handlers. The agent is instructed to call the specific
/// tool and return its output.
#[handler]
pub(crate) async fn tools_invoke_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    // Authenticate.
    let auth = match depot.obtain::<Arc<GatewayAuth>>() {
        Ok(a) => a.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_owned());

    let Some(token) = token else {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(
            json!({"error": "missing Authorization header"}).to_string(),
        ));
        return;
    };

    if auth.validate(&token).await.is_none() {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(json!({"error": "invalid token"}).to_string()));
        return;
    }

    // Parse body.
    let body: ToolInvokeRequest = match req.parse_json().await {
        Ok(b) => b,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": format!("invalid request: {err}")}).to_string(),
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

    // Build a prompt that forces the agent to invoke the requested tool.
    let prompt = build_tool_prompt(&body.tool, body.action.as_deref(), &body.arguments);

    match bridge.invoke_agent_text(&prompt, "default").await {
        Ok(output) => {
            // Try to parse the output as JSON for a cleaner response.
            let parsed = serde_json::from_str::<Value>(&output).unwrap_or_else(|_| {
                Value::String(savfox_core::external_content::wrap_external_content(
                    &format!("tool:{}", body.tool),
                    &output,
                ))
            });
            let result = crate::redaction::redact_json(parsed);
            let response = json!({
                "ok": true,
                "tool": body.tool,
                "result": result,
            });
            res.render(Text::Json(response.to_string()));
        }
        Err(err) => {
            warn!(tool = %body.tool, "tool invocation failed: {err}");
            let safe_err = crate::redaction::redact_text(&format!("{err}"));
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({
                    "ok": false,
                    "tool": body.tool,
                    "error": { "type": "invocation_error", "message": safe_err },
                })
                .to_string(),
            ));
        }
    }
}
