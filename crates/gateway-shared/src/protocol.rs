use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─── JSON-RPC error codes (spec + Savfox-specific) ──────────────────────────
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
pub const PERMISSION_DENIED: i64 = -32001;

/// JSON-RPC request id. Per spec, an id may be a string, number, or null.
pub type JsonRpcId = Value;

/// JSON-RPC notification (server-push, no `id`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonRpcNotification {
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonRpcResponse {
    pub id: JsonRpcId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StatusResponse {
    pub connected_clients: u32,
    pub session_ids: Vec<String>,
}
