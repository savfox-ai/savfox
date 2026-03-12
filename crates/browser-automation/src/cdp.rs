use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, RwLock, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::{debug, warn};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type PendingRequests = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

pub struct CdpClient {
    ws_tx: Mutex<futures_util::stream::SplitSink<WsStream, WsMessage>>,
    next_id: AtomicU64,
    pending: PendingRequests,
    event_handlers: Arc<RwLock<HashMap<String, Vec<Box<dyn Fn(Value) + Send + Sync>>>>>,
}

impl CdpClient {
    pub async fn connect(url: &str) -> Result<Self> {
        let (ws_stream, _) = connect_async(url).await?;
        let (ws_tx, mut ws_rx) = ws_stream.split();

        let pending: PendingRequests = Arc::new(Mutex::new(HashMap::new()));
        let event_handlers: Arc<RwLock<HashMap<String, Vec<Box<dyn Fn(Value) + Send + Sync>>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let pending_clone = pending.clone();
        let handlers_clone = event_handlers.clone();

        tokio::spawn(async move {
            while let Some(msg) = ws_rx.next().await {
                match msg {
                    Ok(WsMessage::Text(text)) => {
                        if let Ok(response) = serde_json::from_str::<CdpResponse>(&text) {
                            if let Some(id) = response.id {
                                let mut pending = pending_clone.lock().await;
                                if let Some(tx) = pending.remove(&id) {
                                    let result = if let Some(error) = response.error {
                                        Err(anyhow!("CDP error: {:?}", error))
                                    } else {
                                        Ok(response.result.unwrap_or(Value::Null))
                                    };
                                    let _ = tx.send(result);
                                }
                            } else if let Some(method) = response.method {
                                debug!(method = %method, "CDP event received");
                                let handlers = handlers_clone.read().await;
                                if let Some(callbacks) = handlers.get(&method) {
                                    let params = response.params.unwrap_or(Value::Null);
                                    for callback in callbacks {
                                        callback(params.clone());
                                    }
                                }
                            }
                        }
                    }
                    Ok(WsMessage::Close(_)) => break,
                    Err(e) => {
                        warn!("WebSocket error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });

        Ok(Self {
            ws_tx: Mutex::new(ws_tx),
            next_id: AtomicU64::new(1),
            pending,
            event_handlers,
        })
    }

    pub async fn send_request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let request = CdpRequest {
            id: Some(id),
            method: method.to_string(),
            params: Some(params),
            session_id: None,
        };

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let message = serde_json::to_string(&request)?;
        self.ws_tx
            .lock()
            .await
            .send(WsMessage::Text(message.into()))
            .await?;

        rx.await.map_err(|_| anyhow!("Channel closed"))?
    }

    pub async fn send_request_to_session(
        &self,
        method: &str,
        params: Value,
        session_id: &str,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let request = CdpRequest {
            id: Some(id),
            method: method.to_string(),
            params: Some(params),
            session_id: Some(session_id.to_string()),
        };

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let message = serde_json::to_string(&request)?;
        self.ws_tx
            .lock()
            .await
            .send(WsMessage::Text(message.into()))
            .await?;

        rx.await.map_err(|_| anyhow!("Channel closed"))?
    }

    pub async fn on_event<F>(&self, event_name: &str, handler: F)
    where
        F: Fn(Value) + Send + Sync + 'static,
    {
        let mut handlers = self.event_handlers.write().await;
        handlers
            .entry(event_name.to_string())
            .or_insert_with(Vec::new)
            .push(Box::new(handler));
    }
}

#[derive(Debug, Serialize)]
struct CdpRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CdpResponse {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    // #[serde(default)]
    // error: Option<CdpError>,
}

// #[derive(Debug, Deserialize)]
// struct CdpError {
//     #[serde(default)]
//     code: i64,
//     #[serde(default)]
//     message: String,
// }

pub async fn discover_websocket_url(port: u16) -> Result<String> {
    let http = HttpClient::new();
    let url = format!("http://127.0.0.1:{}/json/version", port);

    let response = http.get(&url).send().await?;
    let json: Value = response.json::<Value>().await?;

    json.get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("Failed to get WebSocket debugger URL"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    use super::CdpClient;

    #[tokio::test]
    async fn send_request_roundtrip_with_mock_cdp_server() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept socket");
            let mut ws = accept_async(stream).await.expect("accept websocket");
            let req = ws.next().await.expect("message").expect("websocket frame");
            let text = req.into_text().expect("text frame");
            let value: serde_json::Value =
                serde_json::from_str(&text).expect("decode json request");
            let id = value["id"].as_u64().expect("request id");
            ws.send(WsMessage::Text(
                json!({
                    "id": id,
                    "result": {"ok": true}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send response");
        });

        let client = CdpClient::connect(&format!("ws://{}", addr))
            .await
            .expect("connect client");
        let response = client
            .send_request("Runtime.enable", json!({}))
            .await
            .expect("request response");
        assert_eq!(response["ok"], json!(true));

        server.await.expect("server joined");
    }

    #[tokio::test]
    async fn event_handler_receives_mock_cdp_event() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept socket");
            let mut ws = accept_async(stream).await.expect("accept websocket");
            let req = ws.next().await.expect("message").expect("websocket frame");
            let text = req.into_text().expect("text frame");
            let value: serde_json::Value =
                serde_json::from_str(&text).expect("decode json request");
            let id = value["id"].as_u64().expect("request id");

            ws.send(WsMessage::Text(
                json!({
                    "id": id,
                    "result": {}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send ack");

            ws.send(WsMessage::Text(
                json!({
                    "method": "Network.responseReceived",
                    "params": {
                        "requestId": "abc",
                        "response": {
                            "url": "https://example.com",
                            "status": 200
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send event");
        });

        let client = CdpClient::connect(&format!("ws://{}", addr))
            .await
            .expect("connect client");

        let (tx, rx) = oneshot::channel();
        let tx_ref = Arc::new(StdMutex::new(Some(tx)));
        let tx_clone = Arc::clone(&tx_ref);
        client
            .on_event("Network.responseReceived", move |params| {
                if let Ok(mut lock) = tx_clone.lock()
                    && let Some(sender) = lock.take()
                {
                    let _ = sender.send(params);
                }
            })
            .await;

        let _ = client
            .send_request("Network.enable", json!({}))
            .await
            .expect("enable network");

        let event = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("event wait timeout")
            .expect("oneshot event");
        assert_eq!(event["response"]["status"], json!(200));

        server.await.expect("server joined");
    }
}
