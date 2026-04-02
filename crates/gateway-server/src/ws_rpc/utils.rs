use serde_json::Value;

use super::types::{JsonRpcError, JsonRpcErrorBody, JsonRpcResponse};

pub fn rpc_success(id: Value, mut result: Value) -> String {
    crate::redaction::redact_json_in_place(&mut result);
    serde_json::to_string(&JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result,
    })
    .unwrap_or_default()
}

pub fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> String {
    let safe_message = crate::redaction::redact_text(&message.into());
    serde_json::to_string(&JsonRpcError {
        jsonrpc: "2.0",
        id,
        error: JsonRpcErrorBody {
            code,
            message: safe_message,
            data: None,
        },
    })
    .unwrap_or_default()
}

// ─── Parameter extraction helpers ────────────────────────────────────────────

use super::types::INVALID_REQUEST;

/// Extract a required string parameter, returning an RPC error if missing or empty.
pub fn require_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, (i64, String)> {
    let val = params
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if val.is_empty() {
        return Err((INVALID_REQUEST, format!("missing '{key}' parameter")));
    }
    Ok(val)
}

/// Extract an optional string parameter with a default value.
pub fn opt_str<'a>(params: &'a Value, key: &str, default: &'a str) -> &'a str {
    params.get(key).and_then(|v| v.as_str()).unwrap_or(default)
}

/// Extract an optional boolean parameter with a default value.
pub fn opt_bool(params: &Value, key: &str, default: bool) -> bool {
    params.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// Extract an optional u64 parameter with a default value.
pub fn opt_u64(params: &Value, key: &str, default: u64) -> u64 {
    params.get(key).and_then(|v| v.as_u64()).unwrap_or(default)
}

/// Extract an array of strings from a parameter.
pub fn str_array(params: &Value, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn now_ms() -> u64 {
    crate::json_store::now_ms()
}
