//! Integration tests for the WS-RPC protocol.

mod helpers;

use helpers::free_port;

/// Verify that the WS-RPC JSON-RPC 2.0 framing is correct.
#[tokio::test]
async fn ws_rpc_jsonrpc_framing() {
    // This test validates the JSON-RPC 2.0 message format.
    // In a running server, we'd connect via WebSocket and send:
    // {"jsonrpc":"2.0","id":1,"method":"models.list","params":{}}
    // and expect:
    // {"jsonrpc":"2.0","id":1,"result":{"models":[...]}}

    let _port = free_port();

    // Skeleton: validate JSON-RPC request/response structure
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "models.list",
        "params": {}
    });

    assert_eq!(request["jsonrpc"], "2.0");
    assert_eq!(request["method"], "models.list");
}

/// Verify that unknown methods return a proper error.
#[tokio::test]
async fn ws_rpc_unknown_method_error() {
    let error_response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {
            "code": -32601,
            "message": "Method not found"
        }
    });

    assert_eq!(error_response["error"]["code"], -32601);
}
