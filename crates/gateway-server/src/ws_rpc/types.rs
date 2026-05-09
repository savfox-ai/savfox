use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use savfox_gateway_shared::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JsonRpcError as JsonRpcErrorBody, JsonRpcId,
    METHOD_NOT_FOUND, PARSE_ERROR, PERMISSION_DENIED,
};

pub const PLUGIN_ROUTE_RATE_LIMIT_PER_MINUTE: u32 = 60;

#[derive(Debug, Deserialize)]
pub(crate) struct JsonRpcRequest {
    pub jsonrpc: Option<String>,
    pub id: JsonRpcId,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: JsonRpcId,
    pub result: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcError {
    pub jsonrpc: &'static str,
    pub id: JsonRpcId,
    pub error: JsonRpcErrorBody,
}

pub type RpcResult = Result<Value, (i64, String)>;
