use std::sync::Arc;

use salvo::prelude::*;
use salvo::websocket::{Message, WebSocket, WebSocketUpgrade};
use savfox_app_server_protocol::{JSONRPCRequest, RequestId};
use savfox_protocol::SessionId;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::auth::GatewayAuth;
use crate::channel::GatewayChannel;
use crate::cron_service::CronService;
use crate::protocol::GatewayMessage;
use crate::session::{ClientSession, GatewaySessionManager, SessionStore};

/// Channel buffer size for per-client outgoing message queues.
const CLIENT_CHANNEL_CAPACITY: usize = 256;

/// RAII guard that ensures `remove_session()` is called even if the
/// WebSocket handler panics or returns early without explicit cleanup.
struct SessionCleanupGuard {
    session_mgr: GatewaySessionManager,
    session_id: SessionId,
    active: bool,
}

impl SessionCleanupGuard {
    fn new(session_mgr: GatewaySessionManager, session_id: SessionId) -> Self {
        Self {
            session_mgr,
            session_id,
            active: true,
        }
    }

    /// Manually deactivate so Drop doesn't try to remove again.
    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for SessionCleanupGuard {
    fn drop(&mut self) {
        if self.active {
            let mgr = self.session_mgr.clone();
            let id = self.session_id;
            // Spawn cleanup — we're in a sync Drop so we can't await directly.
            tokio::spawn(async move {
                mgr.remove_session(&id).await;
                warn!(session_id = %id, "session cleaned up via drop guard (unexpected exit)");
            });
        }
    }
}

/// WebSocket upgrade handler for gateway connections at `GET /ws`.
#[handler]
pub(crate) async fn ws_handler(
    req: &mut Request,
    res: &mut Response,
    depot: &mut Depot,
) -> Result<(), StatusError> {
    let auth = depot
        .obtain::<Arc<GatewayAuth>>()
        .map_err(|_| StatusError::internal_server_error())?
        .clone();

    let session_mgr = depot
        .obtain::<Arc<GatewaySessionManager>>()
        .map_err(|_| StatusError::internal_server_error())?
        .clone();

    let channel = depot
        .obtain::<Arc<GatewayChannel>>()
        .map_err(|_| StatusError::internal_server_error())?
        .clone();

    let session_store = depot
        .obtain::<Arc<SessionStore>>()
        .map_err(|_| StatusError::internal_server_error())?
        .clone();

    let cron_service = depot
        .obtain::<Arc<CronService>>()
        .map_err(|_| StatusError::internal_server_error())?
        .clone();

    // Allow token via query parameter for CLI tools like websocat.
    //
    // NOTE: Query-parameter tokens appear in server access logs and browser
    // history. For production deployments, prefer the challenge-response
    // authentication flow (send a `Connect` message as the first WebSocket
    // frame) which keeps the token out of URLs entirely.
    let query_token = req.query::<String>("token");

    WebSocketUpgrade::new()
        .upgrade(req, res, move |ws| {
            handle_ws_connection(
                ws,
                auth,
                session_mgr,
                channel,
                session_store,
                cron_service,
                query_token,
            )
        })
        .await
}

/// Main WebSocket connection loop.
async fn handle_ws_connection(
    mut ws: WebSocket,
    auth: Arc<GatewayAuth>,
    session_mgr: Arc<GatewaySessionManager>,
    channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
    cron_service: Arc<CronService>,
    query_token: Option<String>,
) {
    // --- Authentication phase ---
    // The client must authenticate either:
    // 1. Via query parameter `?token=...` (already extracted)
    // 2. By sending a `Connect` message as the first frame

    let (token_info, _client_info) = if let Some(ref token) = query_token {
        match auth.validate(token).await {
            Some(info) => (info, None::<crate::protocol::ClientInfo>),
            None => {
                let err = GatewayMessage::Error {
                    id: None,
                    code: 401,
                    message: "invalid token".to_owned(),
                };
                let _ = send_message(&mut ws, &err).await;
                return;
            }
        }
    } else {
        // --- Challenge-Response Authentication ---
        // 1. Server sends a ConnectChallenge with a random nonce + timestamp
        // 2. Client replies with Connect { token }  - the token can be: a) Raw token
        //    (backwards-compatible, still accepted) b) HMAC-SHA256(nonce, token) signature for
        //    replay protection
        let nonce = uuid::Uuid::now_v7().to_string();
        let challenge_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let challenge = GatewayMessage::ConnectChallenge {
            nonce: nonce.clone(),
            ts: challenge_ts,
        };
        if send_message(&mut ws, &challenge).await.is_err() {
            return;
        }

        // Wait for a Connect message.
        match tokio::time::timeout(std::time::Duration::from_secs(30), ws.recv()).await {
            Ok(Some(Ok(msg))) => {
                let text = match msg.as_str() {
                    Ok(t) => t.to_owned(),
                    Err(_) => {
                        return;
                    }
                };
                match serde_json::from_str::<GatewayMessage>(&text) {
                    Ok(GatewayMessage::Connect {
                        token, client_info, ..
                    }) => {
                        // Try direct token validation first (backwards-compatible)
                        if let Some(info) = auth.validate(&token).await {
                            (info, client_info)
                        } else {
                            // Try as HMAC signature: client sent HMAC-SHA256(nonce, real_token)
                            // We need to check all known tokens to see if any matches
                            match auth.validate_challenge_response(&token, &nonce).await {
                                Some(info) => (info, client_info),
                                None => {
                                    let err = GatewayMessage::Error {
                                        id: None,
                                        code: 401,
                                        message: "invalid token or signature".to_owned(),
                                    };
                                    let _ = send_message(&mut ws, &err).await;
                                    return;
                                }
                            }
                        }
                    }
                    _ => {
                        let err = GatewayMessage::Error {
                            id: None,
                            code: 400,
                            message: "expected Connect message".to_owned(),
                        };
                        let _ = send_message(&mut ws, &err).await;
                        return;
                    }
                }
            }
            _ => {
                // Timeout or connection closed.
                return;
            }
        }
    };

    // Create session.
    let session_id = SessionId::new();
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<GatewayMessage>(CLIENT_CHANNEL_CAPACITY);

    // Keep a copy of token_info for per-method scope checks in the dispatch loop.
    let rpc_token_info = token_info.clone();
    let session = ClientSession::new(session_id, token_info, outgoing_tx);
    session_mgr.add_session(session).await;
    let mut _guard = SessionCleanupGuard::new((*session_mgr).clone(), session_id);

    // Send Connected acknowledgement.
    let connected = GatewayMessage::Connected {
        session_id: session_id.to_string(),
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: Some(crate::protocol::PROTOCOL_VERSION),
        policy: None,
    };
    if send_message(&mut ws, &connected).await.is_err() {
        // Guard will handle cleanup on drop.
        return;
    }

    info!(session_id = %session_id, "WebSocket client authenticated");

    // --- Message loop ---
    // We use a select loop to handle both reading from the WebSocket
    // and sending outgoing messages from the channel.
    loop {
        tokio::select! {
            // Outgoing messages from the session channel.
            outgoing = outgoing_rx.recv() => {
                let Some(msg) = outgoing else {
                    // Channel closed  - session was cleaned up.
                    break;
                };
                if send_message(&mut ws, &msg).await.is_err() {
                    info!(session_id = %session_id, "WebSocket write failed, closing");
                    break;
                }
            }

            // Incoming messages from the WebSocket.
            incoming = ws.recv() => {
                let Some(result) = incoming else {
                    info!(session_id = %session_id, "WebSocket closed by client");
                    break;
                };
                let msg: Message = match result {
                    Ok(m) => m,
                    Err(err) => {
                        warn!(session_id = %session_id, "WebSocket read error: {err}");
                        break;
                    }
                };

                if msg.is_close() {
                    info!(session_id = %session_id, "client sent close frame");
                    break;
                }

                let text = match msg.as_str() {
                    Ok(t) => t,
                    Err(_) => continue, // Skip non-text frames.
                };

                // Try JSON-RPC 2.0 dispatch first (Phase 5).
                // JSON-RPC messages have a "jsonrpc" field; GatewayMessages have "type".
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
                    if parsed.get("jsonrpc").is_some() {
                        let session_id_str = session_id.to_string();
                        let reply = crate::ws_rpc::dispatch_rpc(
                            text,
                            &session_id_str,
                            &auth,
                            &session_mgr,
                            &channel,
                            &session_store,
                            &cron_service,
                            &rpc_token_info,
                        ).await;
                        // Send the raw JSON-RPC response directly.
                        if ws.send(Message::text(reply)).await.is_err() {
                            break;
                        }
                        continue;
                    }
                }

                let gateway_msg = match serde_json::from_str::<GatewayMessage>(text) {
                    Ok(m) => m,
                    Err(err) => {
                        warn!(
                            session_id = %session_id,
                            "failed to parse WebSocket message: {err}"
                        );
                        continue;
                    }
                };

                let session_id_str = session_id.to_string();
                handle_client_message(
                    &session_id_str,
                    gateway_msg,
                    &session_mgr,
                    &channel,
                ).await;
            }
        }
    }

    // Cleanup — disarm guard and do it explicitly so we log properly.
    _guard.disarm();
    session_mgr.remove_session(&session_id).await;
    info!(session_id = %session_id, "WebSocket connection closed");
}

/// Handle a single parsed message from a connected client.
async fn handle_client_message(
    session_id: &str,
    msg: GatewayMessage,
    session_mgr: &GatewaySessionManager,
    channel: &GatewayChannel,
) {
    match msg {
        GatewayMessage::Request { id, method, params } => {
            let request = JSONRPCRequest {
                id: RequestId::String(id),
                method,
                params,
            };
            channel.process_request(session_id, request).await;
        }

        GatewayMessage::Response {
            id,
            ok: _,
            payload,
            error: _,
        } => {
            let result = payload.unwrap_or(Value::Null);
            channel
                .process_client_response(RequestId::String(id), result)
                .await;
        }

        GatewayMessage::ApprovalResponse {
            request_id,
            approved,
        } => {
            let result = serde_json::json!({ "approved": approved });
            channel
                .process_client_response(RequestId::String(request_id), result)
                .await;
        }

        GatewayMessage::Ping => {
            let _ = session_mgr
                .send_to_session(session_id, GatewayMessage::Pong)
                .await;
        }

        // Ignore messages that don't need handling from the server side.
        GatewayMessage::Connect { .. }
        | GatewayMessage::ConnectChallenge { .. }
        | GatewayMessage::Connected { .. }
        | GatewayMessage::Event { .. }
        | GatewayMessage::Error { .. }
        | GatewayMessage::Pong => {}
    }
}

/// Send a serialized `GatewayMessage` over the WebSocket.
async fn send_message(ws: &mut WebSocket, msg: &GatewayMessage) -> Result<(), ()> {
    let text = match serde_json::to_string(msg) {
        Ok(t) => t,
        Err(err) => {
            error!("failed to serialize gateway message: {err}");
            return Err(());
        }
    };

    ws.send(Message::text(text)).await.map_err(|err| {
        warn!("WebSocket send error: {err}");
    })
}
