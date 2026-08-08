#![allow(clippy::manual_let_else)]

//! End-to-end integration tests for the Savfox Gateway Server.
//!
//! These tests require a running gateway instance.  They are marked `#[ignore]`
//! so that `cargo test` does not attempt to run them during normal development.
//!
//! To run them manually:
//!
//! ```bash
//! export E2E_GATEWAY_URL="http://localhost:18881"
//! export E2E_GATEWAY_TOKEN="<your-token>"
//! cargo test -p savfox-gateway-server --test e2e_gateway -- --ignored
//! ```
//!
//! Or use the helper script `scripts/test-e2e.sh` which starts a gateway,
//! waits for it to be healthy, and runs these tests automatically.

mod helpers;

use helpers::http_client_with_timeout;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read the base URL for a running gateway from `E2E_GATEWAY_URL`.
fn gateway_url() -> Option<String> {
    std::env::var("E2E_GATEWAY_URL").ok()
}

/// Read the bearer token from `E2E_GATEWAY_TOKEN`.
fn gateway_token() -> Option<String> {
    std::env::var("E2E_GATEWAY_TOKEN").ok()
}

/// Bail out of a test early (with a message to stderr) when the required
/// environment variables are not set.
macro_rules! require_gateway {
    () => {{
        let Some(url) = gateway_url() else {
            eprintln!("Skipping: E2E_GATEWAY_URL not set");
            return;
        };
        let Some(token) = gateway_token() else {
            eprintln!("Skipping: E2E_GATEWAY_TOKEN not set");
            return;
        };
        (url, token)
    }};
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn to_ws_base(url: &str) -> String {
    url.replace("http://", "ws://")
        .replace("https://", "wss://")
}

async fn ws_connect_with_query_token(url: &str, token: &str) -> WsStream {
    use futures_util::StreamExt;

    let ws_url = format!("{}/ws?token={token}", to_ws_base(url));
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("failed to connect websocket");

    // Query-token auth should immediately return "connected".
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
        .await
        .expect("timed out waiting for connected")
        .expect("ws stream ended")
        .expect("ws read error");
    let first_text = first.into_text().expect("expected text frame");
    let first_json: Value = serde_json::from_str(&first_text).expect("invalid JSON frame");
    assert_eq!(
        first_json.get("type").and_then(|v| v.as_str()),
        Some("connected"),
        "expected immediate connected frame, got: {first_text}"
    );

    ws
}

async fn ws_connect_with_challenge(url: &str, token: &str) -> WsStream {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let ws_url = format!("{}/ws", to_ws_base(url));
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("failed to connect websocket");

    // First frame should be connect_challenge.
    let challenge = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
        .await
        .expect("timed out waiting for challenge")
        .expect("ws stream ended")
        .expect("ws read error");
    let challenge_text = challenge.into_text().expect("expected text frame");
    let challenge_json: Value =
        serde_json::from_str(&challenge_text).expect("invalid challenge JSON");
    let challenge_type = challenge_json.get("type").and_then(|v| v.as_str());
    assert!(
        matches!(
            challenge_type,
            Some("connect_challenge" | "connectChallenge")
        ),
        "expected connect_challenge/connectChallenge frame, got: {challenge_text}"
    );

    let connect_msg = json!({
        "type": "connect",
        "token": token,
    });
    ws.send(Message::Text(connect_msg.to_string().into()))
        .await
        .expect("failed to send connect frame");

    let connected = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
        .await
        .expect("timed out waiting for connected")
        .expect("ws stream ended")
        .expect("ws read error");
    let connected_text = connected.into_text().expect("expected text frame");
    let connected_json: Value =
        serde_json::from_str(&connected_text).expect("invalid connected JSON");
    assert_eq!(
        connected_json.get("type").and_then(|v| v.as_str()),
        Some("connected"),
        "expected connected frame, got: {connected_text}"
    );

    ws
}

async fn ws_read_json(ws: &mut WsStream) -> Value {
    use futures_util::StreamExt;

    let frame = tokio::time::timeout(std::time::Duration::from_secs(20), ws.next())
        .await
        .expect("timed out waiting for ws frame")
        .expect("ws stream ended")
        .expect("ws read error");
    let text = frame.into_text().expect("expected text frame");
    serde_json::from_str(&text).expect("invalid JSON frame")
}

async fn ws_rpc_call(ws: &mut WsStream, id: u64, method: &str, params: Value) -> Value {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let req = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    ws.send(Message::Text(req.to_string().into()))
        .await
        .expect("failed to send rpc request");

    loop {
        let frame = ws_read_json(ws).await;
        if frame.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
            // Skip out-of-band gateway frames.
            continue;
        }
        if frame.get("id").and_then(|v| v.as_u64()) != Some(id) {
            continue;
        }
        if let Some(err) = frame.get("error") {
            panic!("rpc {method} failed: {err}");
        }
        return frame
            .get("result")
            .cloned()
            .expect("rpc response missing result");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `GET /health` should return 200 with `{"status":"ok"}`.
#[tokio::test]
#[ignore]
async fn health_check() {
    let (url, _token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .get(format!("{url}/health"))
        .send()
        .await
        .expect("health request failed");

    assert_eq!(resp.status().as_u16(), 200, "expected 200 from /health");

    let body: Value = resp.json().await.expect("invalid JSON from /health");
    assert_eq!(
        body.get("status").and_then(|v| v.as_str()),
        Some("ok"),
        "health status should be 'ok'"
    );
}

/// `POST /v1/chat/completions` without an `Authorization` header should
/// return 401.
#[tokio::test]
#[ignore]
async fn auth_missing_token() {
    let (url, _token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .post(format!("{url}/v1/chat/completions"))
        .json(&json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status().as_u16(),
        401,
        "missing auth should produce 401"
    );
}

/// `POST /v1/chat/completions` with an invalid bearer token should
/// return 401.
#[tokio::test]
#[ignore]
async fn auth_invalid_token() {
    let (url, _token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .post(format!("{url}/v1/chat/completions"))
        .header("Authorization", "Bearer totally-wrong-token")
        .json(&json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status().as_u16(),
        401,
        "invalid token should produce 401"
    );
}

/// `GET /v1/models` with a valid token should return a JSON body
/// containing an `object` field equal to `"list"` and a `data` array.
#[tokio::test]
#[ignore]
async fn models_list() {
    let (url, token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .get(format!("{url}/v1/models"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .expect("models request failed");

    assert_eq!(resp.status().as_u16(), 200, "expected 200 from /v1/models");

    let body: Value = resp.json().await.expect("invalid JSON from /v1/models");
    assert_eq!(
        body.get("object").and_then(|v| v.as_str()),
        Some("list"),
        "object should be 'list'"
    );
    assert!(
        body.get("data").and_then(|v| v.as_array()).is_some(),
        "data should be an array"
    );
}

/// `GET /v1/models` without a token should return 401.
#[tokio::test]
#[ignore]
async fn models_list_no_auth() {
    let (url, _token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .get(format!("{url}/v1/models"))
        .send()
        .await
        .expect("models request failed");

    assert_eq!(
        resp.status().as_u16(),
        401,
        "missing auth should produce 401 on /v1/models"
    );
}

/// `POST /v1/chat/completions` with a valid token but a malformed body
/// (missing required `model` field) should return 400.
#[tokio::test]
#[ignore]
async fn chat_completions_bad_request() {
    let (url, token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    // Send a body that is missing the required `model` field.
    let resp = client
        .post(format!("{url}/v1/chat/completions"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status().as_u16(),
        400,
        "missing model field should produce 400"
    );

    let body: Value = resp.json().await.expect("invalid JSON from error response");
    assert!(
        body.get("error").is_some(),
        "error response should contain an 'error' field"
    );
}

/// `GET /api/status` should return 200 with valid JSON that includes
/// `connected_clients` and `session_ids`.
#[tokio::test]
#[ignore]
async fn api_status() {
    let (url, _token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .get(format!("{url}/api/status"))
        .send()
        .await
        .expect("status request failed");

    assert_eq!(resp.status().as_u16(), 200, "expected 200 from /api/status");

    let body: Value = resp.json().await.expect("invalid JSON from /api/status");
    assert!(
        body.get("connected_clients").is_some(),
        "status should contain connected_clients"
    );
    assert!(
        body.get("session_ids").is_some(),
        "status should contain session_ids"
    );
}

/// `POST /api/token/validate` with a valid token should return
/// `{"valid": true}`.
#[tokio::test]
#[ignore]
async fn token_validate_valid() {
    let (url, token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .post(format!("{url}/api/token/validate"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .expect("token validate request failed");

    assert_eq!(resp.status().as_u16(), 200, "valid token should return 200");

    let body: Value = resp
        .json()
        .await
        .expect("invalid JSON from /api/token/validate");
    assert_eq!(
        body.get("valid").and_then(|v| v.as_bool()),
        Some(true),
        "valid field should be true"
    );
}

/// `POST /api/token/validate` with an invalid token should return 401
/// and `{"valid": false}`.
#[tokio::test]
#[ignore]
async fn token_validate_invalid() {
    let (url, _token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .post(format!("{url}/api/token/validate"))
        .header("Authorization", "Bearer bad-token-value")
        .send()
        .await
        .expect("token validate request failed");

    assert_eq!(
        resp.status().as_u16(),
        401,
        "invalid token should return 401"
    );

    let body: Value = resp
        .json()
        .await
        .expect("invalid JSON from /api/token/validate");
    assert_eq!(
        body.get("valid").and_then(|v| v.as_bool()),
        Some(false),
        "valid field should be false"
    );
}

/// `POST /api/token/validate` with no token at all should return 400.
#[tokio::test]
#[ignore]
async fn token_validate_missing() {
    let (url, _token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .post(format!("{url}/api/token/validate"))
        .send()
        .await
        .expect("token validate request failed");

    assert_eq!(
        resp.status().as_u16(),
        400,
        "no token provided should return 400"
    );
}

/// `GET /api/config` should return 200 with JSON containing version and
/// endpoints information.
#[tokio::test]
#[ignore]
async fn api_config() {
    let (url, _token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .get(format!("{url}/api/config"))
        .send()
        .await
        .expect("config request failed");

    assert_eq!(resp.status().as_u16(), 200, "expected 200 from /api/config");

    let body: Value = resp.json().await.expect("invalid JSON from /api/config");
    assert!(
        body.get("version").is_some(),
        "config should contain version"
    );
    assert!(
        body.get("endpoints").is_some(),
        "config should contain endpoints"
    );
}

/// `GET /api/sessions` should return 200 with a sessions list.
#[tokio::test]
#[ignore]
async fn api_sessions() {
    let (url, _token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .get(format!("{url}/api/sessions"))
        .send()
        .await
        .expect("sessions request failed");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "expected 200 from /api/sessions"
    );

    let body: Value = resp.json().await.expect("invalid JSON from /api/sessions");
    assert!(
        body.get("sessions").is_some(),
        "response should contain sessions"
    );
    assert!(body.get("count").is_some(), "response should contain count");
}

/// `GET /api/channels` should return 200 with a channels list.
#[tokio::test]
#[ignore]
async fn api_channels() {
    let (url, _token) = require_gateway!();
    let client = http_client_with_timeout(std::time::Duration::from_secs(30));

    let resp = client
        .get(format!("{url}/api/channels"))
        .send()
        .await
        .expect("channels request failed");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "expected 200 from /api/channels"
    );

    let body: Value = resp.json().await.expect("invalid JSON from /api/channels");
    assert!(
        body.get("channels").and_then(|v| v.as_array()).is_some(),
        "response should contain channels array"
    );
}

/// WebSocket connection at `/ws` using the Connect handshake protocol.
///
/// 1. Open a WebSocket to `ws://<host>/ws`
/// 2. Send a `Connect` message with the valid token
/// 3. Expect a `Connected` response with a `session_id`
#[tokio::test]
#[ignore]
async fn websocket_connect_handshake() {
    let (url, token) = require_gateway!();
    let mut ws_stream = ws_connect_with_challenge(&url, &token).await;
    ws_stream.close(None).await.ok();
}

/// WebSocket connection at `/ws` with an invalid token should receive
/// an error frame and then the server should close the connection.
#[tokio::test]
#[ignore]
async fn websocket_connect_invalid_token() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let (url, _token) = require_gateway!();

    let ws_url = format!("{}/ws", to_ws_base(&url));
    let (mut ws_stream, _response) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("WebSocket connection failed");

    // Consume challenge first.
    let _challenge = tokio::time::timeout(std::time::Duration::from_secs(10), ws_stream.next())
        .await
        .expect("timed out waiting for challenge")
        .expect("WebSocket stream ended unexpectedly")
        .expect("WebSocket read error");

    // Send Connect with a wrong token
    let connect_msg = json!({
        "type": "connect",
        "token": "completely-invalid-token",
    });
    ws_stream
        .send(Message::Text(connect_msg.to_string().into()))
        .await
        .expect("failed to send Connect message");

    // Expect an error message
    let timeout_dur = std::time::Duration::from_secs(10);
    let msg = tokio::time::timeout(timeout_dur, ws_stream.next())
        .await
        .expect("timed out waiting for error response")
        .expect("WebSocket stream ended unexpectedly")
        .expect("WebSocket read error");

    let text = msg.into_text().expect("expected a text WebSocket frame");

    let parsed: Value = serde_json::from_str(&text).expect("error response is not valid JSON");

    assert_eq!(
        parsed.get("type").and_then(|v| v.as_str()),
        Some("error"),
        "expected 'error' message type, got: {text}"
    );
    assert_eq!(
        parsed.get("code").and_then(|v| v.as_i64()),
        Some(401),
        "error code should be 401"
    );
}

/// WebSocket connection via query-parameter authentication:
/// `GET /ws?token=<valid-token>` should receive a `Connected` message
/// without needing to send a `Connect` frame first.
#[tokio::test]
#[ignore]
async fn websocket_query_token_auth() {
    let (url, token) = require_gateway!();
    let mut ws_stream = ws_connect_with_query_token(&url, &token).await;
    ws_stream.close(None).await.ok();
}

/// WebSocket Ping/Pong: after connecting, send a `ping` message and
/// expect a `pong` reply.
#[tokio::test]
#[ignore]
async fn websocket_ping_pong() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let (url, token) = require_gateway!();
    let mut ws_stream = ws_connect_with_query_token(&url, &token).await;

    // Send a Ping message
    let ping_msg = json!({"type": "ping"});
    ws_stream
        .send(Message::Text(ping_msg.to_string().into()))
        .await
        .expect("failed to send Ping");

    // Expect a Pong
    let parsed = ws_read_json(&mut ws_stream).await;

    assert_eq!(
        parsed.get("type").and_then(|v| v.as_str()),
        Some("pong"),
        "expected 'pong' message type"
    );

    ws_stream.close(None).await.ok();
}

/// WebSocket end-to-end flow:
/// connect -> auth -> JSON-RPC call -> disconnect.
#[tokio::test]
#[ignore]
async fn websocket_auth_rpc_disconnect_flow() {
    let (url, token) = require_gateway!();
    let mut ws = ws_connect_with_challenge(&url, &token).await;

    let result = ws_rpc_call(&mut ws, 1001, "status", json!({})).await;
    assert!(
        result.get("connected_clients").is_some(),
        "status result should include connected_clients"
    );
    assert!(
        result.get("session_ids").is_some(),
        "status result should include session_ids"
    );

    ws.close(None).await.ok();
}

/// Session lifecycle e2e: create -> reset.
#[tokio::test]
#[ignore]
async fn session_lifecycle_create_and_reset() {
    let (url, token) = require_gateway!();
    let mut ws = ws_connect_with_query_token(&url, &token).await;
    let session_id = uuid::Uuid::now_v7().to_string();

    let patched = ws_rpc_call(
        &mut ws,
        1101,
        "sessions.patch",
        json!({
            "session_id": session_id,
            "patch": {
                "model": "mock/echo",
                "label": "e2e-session"
            }
        }),
    )
    .await;
    assert_eq!(
        patched.get("status").and_then(|v| v.as_str()),
        Some("patched")
    );

    let reset = ws_rpc_call(
        &mut ws,
        1102,
        "sessions.reset",
        json!({
            "session_id": session_id
        }),
    )
    .await;
    assert_eq!(reset.get("status").and_then(|v| v.as_str()), Some("reset"));

    ws.close(None).await.ok();
}

/// Chat send -> agent response -> channel delivery e2e.
///
/// Uses `mock_response` to avoid external model dependencies in CI.
#[tokio::test]
#[ignore]
async fn chat_send_agent_response_message_delivery_e2e() {
    let (url, token) = require_gateway!();
    let mut ws = ws_connect_with_query_token(&url, &token).await;

    let send_result = ws_rpc_call(
        &mut ws,
        1201,
        "chat.send",
        json!({
            "agent": "default",
            "message": "hello from e2e",
            "mock_response": "hello from mock agent"
        }),
    )
    .await;

    let response = send_result
        .get("response")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    assert!(
        response.contains("hello from mock agent"),
        "chat.send should return mocked agent response"
    );

    let delivery = ws_rpc_call(
        &mut ws,
        1202,
        "send",
        json!({
            "channel": "webhook:e2e-channel",
            "text": response,
        }),
    )
    .await;
    assert_eq!(
        delivery.get("status").and_then(|v| v.as_str()),
        Some("sent")
    );

    ws.close(None).await.ok();
}

/// Cron scheduling e2e:
/// add -> list -> remove.
#[tokio::test]
#[ignore]
async fn cron_job_scheduling_e2e() {
    let (url, token) = require_gateway!();
    let mut ws = ws_connect_with_query_token(&url, &token).await;
    let name = format!("e2e-cron-{}", uuid::Uuid::now_v7());

    let add = ws_rpc_call(
        &mut ws,
        1301,
        "cron.add",
        json!({
            "name": name,
            "schedule": { "kind": "every", "interval_secs": 3600 },
            "payload": { "type": "system_event", "text": "e2e cron tick" },
        }),
    )
    .await;
    let id = add
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    assert!(!id.is_empty(), "cron.add should return id");

    let list = ws_rpc_call(&mut ws, 1302, "cron.list", json!({})).await;
    let jobs = list
        .get("jobs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        jobs.iter()
            .any(|job| job.get("id").and_then(|v| v.as_str()) == Some(id.as_str())),
        "cron.list should include newly added job"
    );

    let removed = ws_rpc_call(&mut ws, 1303, "cron.remove", json!({ "id": id })).await;
    assert_eq!(
        removed.get("status").and_then(|v| v.as_str()),
        Some("removed")
    );

    ws.close(None).await.ok();
}

/// Config reload e2e:
/// patch -> get -> remove patch.
#[tokio::test]
#[ignore]
async fn config_reload_e2e() {
    let (url, token) = require_gateway!();
    let mut ws = ws_connect_with_query_token(&url, &token).await;
    let marker = format!("e2e-{}", uuid::Uuid::now_v7());

    let patched = ws_rpc_call(
        &mut ws,
        1401,
        "config.patch",
        json!({
            "patch": {
                "e2e_marker": marker
            }
        }),
    )
    .await;
    assert_eq!(
        patched.get("status").and_then(|v| v.as_str()),
        Some("patched")
    );

    let got = ws_rpc_call(&mut ws, 1402, "config.get", json!({})).await;
    assert!(
        got.get("config").and_then(|v| v.as_object()).is_some(),
        "config.get should include config object"
    );

    let _ = ws_rpc_call(
        &mut ws,
        1403,
        "config.patch",
        json!({
            "patch": {
                "e2e_marker": Value::Null
            }
        }),
    )
    .await;

    ws.close(None).await.ok();
}

/// Multi-client concurrent WebSocket connection test.
#[tokio::test]
#[ignore]
async fn multi_client_concurrent_connection_test() {
    let (url, token) = require_gateway!();
    let mut tasks = Vec::new();
    for i in 0..8_u64 {
        let url = url.clone();
        let token = token.clone();
        tasks.push(tokio::spawn(async move {
            let mut ws = ws_connect_with_query_token(&url, &token).await;
            let result = ws_rpc_call(&mut ws, 2000 + i, "status", json!({})).await;
            ws.close(None).await.ok();
            result
        }));
    }

    let mut completed = 0_u32;
    for task in tasks {
        let result = task.await.expect("concurrent task panicked");
        if result.get("connected_clients").is_some() {
            completed += 1;
        }
    }
    assert_eq!(completed, 8, "all concurrent clients should complete");
}
