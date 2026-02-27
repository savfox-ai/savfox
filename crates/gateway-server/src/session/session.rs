use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{RwLock, mpsc};
use tracing::{info, warn};

use crate::auth::{TokenInfo, TokenScope};
use crate::protocol::GatewayMessage;
use savfox_protocol::SessionId;

/// Per-connection state for a WebSocket client.
#[derive(Debug)]
pub struct ClientSession {
    /// Unique session identifier.
    pub id: SessionId,
    /// Authentication info for this session.
    pub token_info: TokenInfo,
    /// Optional log level filter.
    pub log_level_filter: Option<String>,
    /// Channel to send outbound messages to this client's WebSocket writer.
    pub sender: mpsc::Sender<GatewayMessage>,
    /// Monotonic sequence counter for events sent to this client.
    event_seq: AtomicU64,
    /// Whether this session is subscribed to logs.
    pub subscribed_to_logs: bool,
    /// Thread subscriptions for this session.
    pub subscriptions: HashSet<String>,
}

impl ClientSession {
    /// Create a new client session.
    #[must_use]
    pub fn new(id: SessionId, token_info: TokenInfo, sender: mpsc::Sender<GatewayMessage>) -> Self {
        Self {
            id,
            token_info,
            subscriptions: HashSet::new(),
            subscribed_to_logs: false,
            log_level_filter: None,
            sender,
            event_seq: AtomicU64::new(0),
        }
    }

    /// Return the next event sequence number for this client.
    pub fn next_seq(&self) -> u64 {
        self.event_seq.fetch_add(1, Ordering::Relaxed)
    }
}

/// Manages all active gateway WebSocket sessions.
#[derive(Debug, Clone)]
pub struct GatewaySessionManager {
    sessions: Arc<RwLock<HashMap<SessionId, Arc<RwLock<ClientSession>>>>>,
}

impl GatewaySessionManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new session.
    pub async fn add_session(&self, session: ClientSession) {
        let id = session.id.clone();
        let session = Arc::new(RwLock::new(session));
        let mut sessions = self.sessions.write().await;
        sessions.insert(id.clone(), session);
        info!(session_id = %id, "client session registered");
    }

    /// Remove a session by ID.
    pub async fn remove_session(&self, session_id: &SessionId) {
        let mut sessions = self.sessions.write().await;
        if sessions.remove(session_id).is_some() {
            info!(session_id = %session_id, "client session removed");
        }
    }

    /// Get a session by ID.
    pub async fn get_session(&self, session_id: &SessionId) -> Option<Arc<RwLock<ClientSession>>> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned()
    }

    /// Broadcast a log entry to all sessions subscribed to logs.
    pub async fn broadcast_log(&self, level: &str, source: &str, message: &str) {
        let sessions = self.sessions.read().await;
        for session_arc in sessions.values() {
            let session = session_arc.read().await;
            if session.subscribed_to_logs {
                if let Some(ref filter) = session.log_level_filter {
                    if !Self::log_level_matches(level, filter) {
                        continue;
                    }
                }
                let seq = session.next_seq();
                let msg = GatewayMessage::Event {
                    event: "log".to_owned(),
                    payload: serde_json::json!({
                        "level": level,
                        "source": source,
                        "message": message,
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                    }),
                    seq: Some(seq),
                };
                if let Err(err) = session.sender.try_send(msg) {
                    warn!(
                        session_id = %session.id,
                        "failed to broadcast log to client: {err}"
                    );
                }
            }
        }
    }

    fn log_level_matches(level: &str, filter: &str) -> bool {
        let level_rank = |l: &str| -> u8 {
            match l.to_lowercase().as_str() {
                "error" => 4,
                "warn" => 3,
                "info" => 2,
                "debug" => 1,
                "trace" => 0,
                _ => 2,
            }
        };
        level_rank(level) >= level_rank(filter)
    }

    /// Broadcast an event to all sessions subscribed to a given session.
    pub async fn broadcast_to_session(
        &self,
        session_id: &str,
        event: &str,
        payload: serde_json::Value,
    ) {
        let sessions = self.sessions.read().await;
        for session_arc in sessions.values() {
            let session = session_arc.read().await;
            if session.subscriptions.contains(session_id) {
                let seq = session.next_seq();
                let msg = GatewayMessage::Event {
                    event: event.to_owned(),
                    payload: payload.clone(),
                    seq: Some(seq),
                };
                if let Err(err) = session.sender.try_send(msg) {
                    warn!(
                        session_id = %session.id,
                        "failed to broadcast event to client: {err}"
                    );
                }
            }
        }
    }

    /// Broadcast an event to ALL connected sessions (used for approval requests).
    pub async fn broadcast_to_all(&self, event: &str, payload: serde_json::Value) {
        let sessions = self.sessions.read().await;
        for session_arc in sessions.values() {
            let session = session_arc.read().await;
            let seq = session.next_seq();
            let msg = GatewayMessage::Event {
                event: event.to_owned(),
                payload: payload.clone(),
                seq: Some(seq),
            };
            if let Err(err) = session.sender.try_send(msg) {
                warn!(
                    session_id = %session.id,
                    "failed to broadcast event to client: {err}"
                );
            }
        }
    }

    /// Broadcast an event only to sessions that have the required `TokenScope`.
    ///
    /// Use this instead of `broadcast_to_all` for events that should be
    /// filtered by the client's auth scopes (e.g., approval events only
    /// go to clients with the `OperatorApprovals` scope).
    pub async fn broadcast_to_scoped(
        &self,
        event: &str,
        payload: serde_json::Value,
        required_scope: TokenScope,
    ) {
        let sessions = self.sessions.read().await;
        for session_arc in sessions.values() {
            let session = session_arc.read().await;
            if !session.token_info.has_scope(required_scope) {
                continue;
            }
            let seq = session.next_seq();
            let msg = GatewayMessage::Event {
                event: event.to_owned(),
                payload: payload.clone(),
                seq: Some(seq),
            };
            if let Err(err) = session.sender.try_send(msg) {
                warn!(
                    session_id = %session.id,
                    "failed to broadcast scoped event to client: {err}"
                );
            }
        }
    }

    /// Determine the required scope for an event name and broadcast accordingly.
    ///
    /// Events are categorized:
    /// - `exec.approval.*` → `OperatorApprovals`
    /// - `chat.*`, `message.*` → `Chat` scope (implies Chat token)
    /// - `agent.*`, `session.*`, `cron.*`, `channel.*` → `OperatorWrite`
    /// - `log` → `OperatorRead`
    /// - Everything else → `OperatorRead`
    pub async fn broadcast_event_filtered(&self, event: &str, payload: serde_json::Value) {
        let scope = event_required_scope(event);
        self.broadcast_to_scoped(event, payload, scope).await;
    }

    /// Send a message to a specific session.
    pub async fn send_to_session(
        &self,
        session_id: &str,
        msg: GatewayMessage,
    ) -> Result<(), String> {
        let session_id =
            SessionId::from_string(session_id).map_err(|e| format!("invalid session ID: {e}"))?;
        let sessions = self.sessions.read().await;
        if let Some(session_arc) = sessions.get(&session_id) {
            let session = session_arc.read().await;
            session
                .sender
                .try_send(msg)
                .map_err(|e| format!("send failed: {e}"))
        } else {
            Err("session not found".to_owned())
        }
    }

    /// Return the number of active sessions.
    pub async fn session_count(&self) -> usize {
        let sessions = self.sessions.read().await;
        sessions.len()
    }

    /// Return all active session IDs.
    pub async fn session_ids(&self) -> Vec<SessionId> {
        let sessions = self.sessions.read().await;
        sessions.keys().cloned().collect()
    }
}

impl Default for GatewaySessionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Map event names to the minimum `TokenScope` needed to receive them.
fn event_required_scope(event: &str) -> TokenScope {
    if event.starts_with("exec.approval") {
        return TokenScope::OperatorApprovals;
    }
    if event.starts_with("chat.") || event.starts_with("message.") || event == "chat" {
        return TokenScope::Chat;
    }
    if event.starts_with("agent.")
        || event.starts_with("session.")
        || event.starts_with("cron.")
        || event.starts_with("channel.")
    {
        return TokenScope::OperatorWrite;
    }
    if event.starts_with("node.pair") || event.starts_with("device.") {
        return TokenScope::OperatorPairing;
    }
    // Default: read-level events (log, health, status, etc.)
    TokenScope::OperatorRead
}
