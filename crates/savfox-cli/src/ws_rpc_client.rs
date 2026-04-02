//! Lightweight WS-RPC client for talking to a running gateway.
//!
//! Connects to the gateway WebSocket endpoint, sends a single JSON-RPC 2.0
//! request, waits for the matching response, and returns the `result` field
//! (or an error).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

/// Global request ID counter (monotonically increasing across calls).
static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Default timeout for waiting on a WS-RPC response.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// Resolve the gateway WebSocket URL from env or fall back to the default.
pub fn gateway_ws_url(gateway_url: &str) -> String {
    // If the caller already provided a ws:// or wss:// URL, use it directly.
    if gateway_url.starts_with("ws://") || gateway_url.starts_with("wss://") {
        return format!("{gateway_url}/ws");
    }
    // Otherwise convert http(s):// to ws(s)://.
    let ws_url = gateway_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    format!("{ws_url}/ws")
}

/// Open a WebSocket connection to the gateway, optionally authenticating
/// with a bearer token.
async fn connect(
    ws_url: &str,
    token: &str,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, String> {
    let url = if token.is_empty() {
        ws_url.to_owned()
    } else {
        // Append token as a query parameter so the gateway can authenticate.
        if ws_url.contains('?') {
            format!("{ws_url}&token={token}")
        } else {
            format!("{ws_url}?token={token}")
        }
    };

    let (ws_stream, _response) = connect_async(&url)
        .await
        .map_err(|e| format!("failed to connect to {ws_url}: {e}"))?;

    Ok(ws_stream)
}

/// Send a JSON-RPC 2.0 request and return the `result` value from the
/// matching response.
pub async fn rpc_call(
    gateway_url: &str,
    token: &str,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let ws_url = gateway_ws_url(gateway_url);
    let mut ws = connect(&ws_url, token).await?;

    let id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);

    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });

    ws.send(Message::Text(request.to_string().into()))
        .await
        .map_err(|e| format!("failed to send request: {e}"))?;

    // Read messages until we get the matching response (or timeout).
    let result = tokio::time::timeout(DEFAULT_TIMEOUT, async {
        while let Some(msg) = ws.next().await {
            let msg = msg.map_err(|e| format!("websocket read error: {e}"))?;
            match msg {
                Message::Text(text) => {
                    let resp: Value = serde_json::from_str(&text)
                        .map_err(|e| format!("invalid JSON response: {e}"))?;

                    // Check if this is our response (match by id).
                    if resp.get("id").and_then(|v| v.as_u64()) == Some(id) {
                        // Check for error.
                        if let Some(err) = resp.get("error") {
                            let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
                            let message = err
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown error");
                            return Err(format!("RPC error {code}: {message}"));
                        }
                        // Return result.
                        return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
                    }
                    // Not our response — skip (could be a notification).
                }
                Message::Close(_) => {
                    return Err("connection closed by server".to_owned());
                }
                _ => {
                    // Ignore ping/pong/binary.
                }
            }
        }
        Err("connection ended without response".to_owned())
    })
    .await;

    // Close cleanly.
    let _ = ws.close(None).await;

    match result {
        Ok(inner) => inner,
        Err(_elapsed) => Err(format!(
            "timed out waiting for response ({}s)",
            DEFAULT_TIMEOUT.as_secs()
        )),
    }
}

/// Pretty-print a JSON value to stdout.
pub fn print_json(value: &Value) {
    if let Ok(pretty) = serde_json::to_string_pretty(value) {
        println!("{pretty}");
    }
}
