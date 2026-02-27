//! `savfox acp` — stdio ACP bridge backed by gateway WS-RPC.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, RwLock, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::ws_rpc_client;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;
type SessionMap = Arc<RwLock<HashMap<String, String>>>;

const RPC_TIMEOUT: Duration = Duration::from_secs(180);
static NEXT_RPC_ID: AtomicU64 = AtomicU64::new(1);

/// Run ACP bridge over stdio JSON lines.
#[derive(Debug, Parser)]
pub struct AcpCommand {
    /// Gateway URL.
    #[clap(long, default_value = "http://127.0.0.1:18881")]
    pub gateway_url: String,
    /// Gateway auth token.
    #[clap(long, env = "SAVFOX_TOKEN")]
    pub token: Option<String>,
}

pub async fn run(cmd: AcpCommand) -> anyhow::Result<()> {
    let token = cmd.token.unwrap_or_default();
    let ws_base = ws_rpc_client::gateway_ws_url(&cmd.gateway_url);
    let ws_url = if token.is_empty() {
        ws_base
    } else if ws_base.contains('?') {
        format!("{ws_base}&token={token}")
    } else {
        format!("{ws_base}?token={token}")
    };

    let (ws, _resp) = connect_async(&ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("failed to connect to gateway websocket {ws_url}: {e}"))?;
    let (sink, mut stream) = ws.split();
    let sink = Arc::new(Mutex::new(sink));
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

    // ACP session_id -> gateway session_id.
    let acp_to_gateway: SessionMap = Arc::new(RwLock::new(HashMap::new()));
    // gateway session_id -> ACP session_id.
    let gateway_to_acp: SessionMap = Arc::new(RwLock::new(HashMap::new()));
    // in-flight rpc request id -> gateway session_id.
    let request_to_acp: SessionMap = Arc::new(RwLock::new(HashMap::new()));

    let stdout = Arc::new(Mutex::new(tokio::io::stdout()));

    let pending_reader = Arc::clone(&pending);
    let stdout_reader = Arc::clone(&stdout);
    let gateway_to_acp_reader = Arc::clone(&gateway_to_acp);
    let request_to_acp_reader = Arc::clone(&request_to_acp);
    let acp_to_gateway_reader = Arc::clone(&acp_to_gateway);
    let reader_task = tokio::spawn(async move {
        while let Some(frame) = stream.next().await {
            let Ok(frame) = frame else {
                break;
            };
            let Message::Text(text) = frame else {
                if matches!(frame, Message::Close(_)) {
                    break;
                }
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };

            if let Some(response_id) = extract_response_id(&value) {
                if let Some(tx) = pending_reader.lock().await.remove(&response_id) {
                    let _ = tx.send(value);
                }
                continue;
            }

            if value.get("type").and_then(|v| v.as_str()) == Some("event") {
                let _ = forward_gateway_event(
                    &stdout_reader,
                    &value,
                    &gateway_to_acp_reader,
                    &request_to_acp_reader,
                    &acp_to_gateway_reader,
                )
                .await;
            }
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(err) => {
                let _ = write_json_line(
                    &stdout,
                    &json!({
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("invalid JSON: {err}") }
                    }),
                )
                .await;
                continue;
            }
        };

        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = req
            .get("params")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

        match method {
            "prompt" => {
                let prompt = params
                    .get("prompt")
                    .or_else(|| params.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if prompt.is_empty() {
                    let _ = write_json_line(
                        &stdout,
                        &json!({
                            "id": id,
                            "error": { "code": -32602, "message": "missing prompt/message" }
                        }),
                    )
                    .await;
                    continue;
                }

                let acp_session_id = params
                    .get("session_id")
                    .or_else(|| params.get("sessionId"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
                let agent = params
                    .get("agent")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default")
                    .to_string();

                let sink_task = Arc::clone(&sink);
                let pending_task = Arc::clone(&pending);
                let stdout_task = Arc::clone(&stdout);
                let acp_to_gateway_task = Arc::clone(&acp_to_gateway);
                let gateway_to_acp_task = Arc::clone(&gateway_to_acp);
                let request_to_acp_task = Arc::clone(&request_to_acp);

                tokio::spawn(async move {
                    let requested_gateway_session_id = if let Some(mapped) = acp_to_gateway_task
                        .read()
                        .await
                        .get(&acp_session_id)
                        .cloned()
                    {
                        mapped
                    } else if is_uuid_v7(&acp_session_id) {
                        acp_session_id.clone()
                    } else {
                        let generated = uuid::Uuid::now_v7().to_string();
                        acp_to_gateway_task
                            .write()
                            .await
                            .insert(acp_session_id.clone(), generated.clone());
                        gateway_to_acp_task
                            .write()
                            .await
                            .insert(generated.clone(), acp_session_id.clone());
                        generated
                    };

                    let rpc_id = next_rpc_id();
                    request_to_acp_task
                        .write()
                        .await
                        .insert(rpc_id.clone(), requested_gateway_session_id.clone());

                    let rpc_params = json!({
                        "session_id": requested_gateway_session_id,
                        "message": prompt,
                        "agent": agent,
                        "request_id": rpc_id,
                    });

                    let response = rpc_call_with_id(
                        &sink_task,
                        &pending_task,
                        rpc_id.clone(),
                        "chat.send",
                        rpc_params,
                    )
                    .await;

                    request_to_acp_task.write().await.remove(&rpc_id);

                    match response {
                        Ok(resp) => {
                            if let Some(err) = resp.get("error") {
                                let message = err
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("gateway RPC error");
                                let _ = write_json_line(
                                    &stdout_task,
                                    &json!({
                                        "id": id,
                                        "error": { "code": -32000, "message": message }
                                    }),
                                )
                                .await;
                                return;
                            }

                            let result = resp.get("result").cloned().unwrap_or(Value::Null);
                            let resolved_session_id = result
                                .get("session_id")
                                .or_else(|| result.get("session_id"))
                                .and_then(|v| v.as_str())
                                .map(ToOwned::to_owned);
                            let canonical_session_id = resolved_session_id
                                .unwrap_or_else(|| requested_gateway_session_id.clone());

                            acp_to_gateway_task
                                .write()
                                .await
                                .insert(acp_session_id.clone(), canonical_session_id.clone());
                            gateway_to_acp_task
                                .write()
                                .await
                                .insert(canonical_session_id.clone(), acp_session_id.clone());

                            let _ = write_json_line(
                                &stdout_task,
                                &json!({
                                    "id": id,
                                    "result": {
                                        "session_id": canonical_session_id,
                                        "payload": result
                                    }
                                }),
                            )
                            .await;
                        }
                        Err(err) => {
                            let _ = write_json_line(
                                &stdout_task,
                                &json!({
                                    "id": id,
                                    "error": { "code": -32001, "message": err }
                                }),
                            )
                            .await;
                        }
                    }
                });
            }
            "cancel" => {
                let session_id = params
                    .get("session_id")
                    .or_else(|| params.get("sessionId"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .unwrap_or("")
                    .to_string();

                let mapped_session_id = acp_to_gateway
                    .read()
                    .await
                    .get(&session_id)
                    .cloned()
                    .or_else(|| is_uuid_v7(&session_id).then(|| session_id.clone()));
                let rpc_params = mapped_session_id
                    .clone()
                    .map(|session_id| json!({ "session_id": session_id }))
                    .unwrap_or_else(|| json!({}));

                match rpc_call(&sink, &pending, "chat.abort", rpc_params).await {
                    Ok(resp) => {
                        if let Some(err) = resp.get("error") {
                            let message = err
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("gateway RPC error");
                            let _ = write_json_line(
                                &stdout,
                                &json!({
                                    "id": id,
                                    "error": { "code": -32000, "message": message }
                                }),
                            )
                            .await;
                        } else {
                            let result = resp.get("result").cloned().unwrap_or(Value::Null);
                            let _ = write_json_line(
                                &stdout,
                                &json!({
                                    "id": id,
                                    "result": {
                                        "session_id": mapped_session_id,
                                        "payload": result
                                    }
                                }),
                            )
                            .await;
                        }
                    }
                    Err(err) => {
                        let _ = write_json_line(
                            &stdout,
                            &json!({
                                "id": id,
                                "error": { "code": -32001, "message": err }
                            }),
                        )
                        .await;
                    }
                }
            }
            "ping" => {
                let _ =
                    write_json_line(&stdout, &json!({ "id": id, "result": { "ok": true } })).await;
            }
            _ => {
                let _ = write_json_line(
                    &stdout,
                    &json!({
                        "id": id,
                        "error": { "code": -32601, "message": format!("unsupported method: {method}") }
                    }),
                )
                .await;
            }
        }
    }

    {
        let mut sink_guard = sink.lock().await;
        let _ = sink_guard.send(Message::Close(None)).await;
    }
    let _ = reader_task.await;
    Ok(())
}

fn next_rpc_id() -> String {
    let id = NEXT_RPC_ID.fetch_add(1, Ordering::Relaxed);
    format!("acp-rpc-{id}")
}

fn extract_response_id(value: &Value) -> Option<String> {
    if value.get("result").is_none() && value.get("error").is_none() {
        return None;
    }
    let id = value.get("id")?;
    if let Some(s) = id.as_str() {
        Some(s.to_string())
    } else if let Some(n) = id.as_u64() {
        Some(n.to_string())
    } else {
        id.as_i64().map(|n| n.to_string())
    }
}

async fn rpc_call(
    sink: &Arc<Mutex<WsSink>>,
    pending: &PendingMap,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    rpc_call_with_id(sink, pending, next_rpc_id(), method, params).await
}

async fn rpc_call_with_id(
    sink: &Arc<Mutex<WsSink>>,
    pending: &PendingMap,
    rpc_id: String,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let request_id = rpc_id.clone();
    let (tx, rx) = oneshot::channel();
    pending.lock().await.insert(rpc_id.clone(), tx);

    let req = json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "method": method,
        "params": params
    });

    let send_result = sink
        .lock()
        .await
        .send(Message::Text(req.to_string().into()))
        .await;
    if let Err(err) = send_result {
        pending.lock().await.remove(&request_id);
        return Err(format!("failed to send RPC request: {err}"));
    }

    match tokio::time::timeout(RPC_TIMEOUT, rx).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err("RPC response channel closed".to_string()),
        Err(_) => Err(format!(
            "timed out waiting for RPC response ({}s)",
            RPC_TIMEOUT.as_secs()
        )),
    }
}

async fn forward_gateway_event(
    stdout: &Arc<Mutex<tokio::io::Stdout>>,
    frame: &Value,
    gateway_to_acp: &SessionMap,
    request_to_acp: &SessionMap,
    acp_to_gateway: &SessionMap,
) -> Result<(), String> {
    let event_name = frame
        .get("event")
        .and_then(|v| v.as_str())
        .unwrap_or("event")
        .to_string();
    let payload = frame.get("payload").cloned().unwrap_or(Value::Null);

    let request_id = payload
        .get("request_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned);
    let gateway_session_id = payload
        .get("session_id")
        .or_else(|| payload.get("session_id"))
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned);

    let mut session_id = None;
    if let Some(req_id) = request_id.as_deref() {
        session_id = request_to_acp.read().await.get(req_id).cloned();
    }
    if session_id.is_none() {
        if let Some(sid) = gateway_session_id.as_deref() {
            session_id = gateway_to_acp.read().await.get(sid).cloned();
        }
    }
    if let (Some(sid), Some(gateway_sid)) = (&session_id, &gateway_session_id) {
        acp_to_gateway
            .write()
            .await
            .insert(sid.clone(), gateway_sid.clone());
        gateway_to_acp
            .write()
            .await
            .insert(gateway_sid.clone(), sid.clone());
    }

    let mut merged = payload;
    if let Value::Object(ref mut map) = merged {
        if let Some(sid) = gateway_session_id.or(session_id) {
            map.insert("session_id".to_string(), json!(sid));
        }
        map.insert("event".to_string(), json!(event_name.clone()));
    }

    let method = match event_name.as_str() {
        "agent.stream" => "stream",
        "agent.complete" => "complete",
        "agent.error" => "error",
        "tool.call" => "tool_call",
        "tool.result" => "tool_result",
        _ => "event",
    };

    write_json_line(stdout, &json!({ "method": method, "params": merged })).await
}

fn is_uuid_v7(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .map(|u| u.get_version_num() == 7)
        .unwrap_or(false)
}

async fn write_json_line(
    stdout: &Arc<Mutex<tokio::io::Stdout>>,
    value: &Value,
) -> Result<(), String> {
    let line = serde_json::to_string(value).map_err(|e| format!("json serialize error: {e}"))?;
    let mut out = stdout.lock().await;
    out.write_all(line.as_bytes())
        .await
        .map_err(|e| format!("stdout write error: {e}"))?;
    out.write_all(b"\n")
        .await
        .map_err(|e| format!("stdout write error: {e}"))?;
    out.flush()
        .await
        .map_err(|e| format!("stdout flush error: {e}"))?;
    Ok(())
}
