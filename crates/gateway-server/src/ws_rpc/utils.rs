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

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
