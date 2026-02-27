use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Current protocol version.
pub const PROTOCOL_VERSION: u32 = 1;

/// Gateway WebSocket frame envelope types.
///
/// These wrap the existing JSON-RPC messages from `savfox-app-server-protocol` in a thin
/// envelope that adds gateway-specific concerns (auth handshake, event sequencing).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum GatewayMessage {
    /// Server challenge sent immediately after WebSocket upgrade.
    /// Client must sign this with their token to complete authentication.
    ConnectChallenge {
        /// Random nonce for replay protection.
        nonce: String,
        /// Server timestamp (ms since epoch).
        ts: u64,
    },

    /// Client authentication handshake (first message after WS connect).
    Connect {
        token: String,
        #[serde(default)]
        client_info: Option<ClientInfo>,
        /// Minimum protocol version the client supports.
        #[serde(default)]
        min_protocol: Option<u32>,
        /// Maximum protocol version the client supports.
        #[serde(default)]
        max_protocol: Option<u32>,
        /// Client role: "operator" or "node".
        #[serde(default)]
        role: Option<ClientRole>,
        /// Requested scopes.
        #[serde(default)]
        scopes: Vec<String>,
    },

    /// Server acknowledgement of successful authentication.
    Connected {
        session_id: String,
        server_version: String,
        /// Negotiated protocol version.
        #[serde(default)]
        protocol: Option<u32>,
        /// Server policy (e.g., tick interval).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy: Option<ConnectionPolicy>,
    },

    /// Wrapped JSON-RPC request (reuses `ClientRequest` from app-server-protocol).
    Request {
        id: String,
        method: String,
        #[serde(default)]
        params: Option<Value>,
    },

    /// Wrapped JSON-RPC response.
    Response {
        id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<Value>,
    },

    /// Server-push event (wraps `ServerNotification`).
    Event {
        event: String,
        payload: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },

    /// Approval response from a client.
    ApprovalResponse { request_id: String, approved: bool },

    /// Error envelope.
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        code: i64,
        message: String,
    },

    /// Ping for keepalive.
    Ping,

    /// Pong reply.
    Pong,
}

/// Information about the connecting client.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    /// Client name (e.g. "discord-bridge", "web-ui").
    #[serde(default)]
    pub name: Option<String>,

    /// Client version string.
    #[serde(default)]
    pub version: Option<String>,

    /// Platform identifier (e.g. "macos", "linux", "windows", "ios", "android").
    #[serde(default)]
    pub platform: Option<String>,
}

/// Client role determines available methods and events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientRole {
    /// CLI/UI/automation client with full operator capabilities.
    Operator,
    /// Capability host (camera, screen, canvas, voice)  - typically a mobile device.
    Node,
    /// Web-based chat client with limited scope.
    WebChat,
}

impl Default for ClientRole {
    fn default() -> Self {
        Self::Operator
    }
}

/// Server-side connection policy sent with the Connected message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPolicy {
    /// Suggested tick/heartbeat interval in milliseconds.
    #[serde(default)]
    pub tick_interval_ms: Option<u64>,
}

/// Actions that can result from processing an incoming bridge webhook.
#[derive(Debug, Clone)]
pub enum BridgeAction {
    /// Start a new agent thread with the given prompt.
    StartThread { channel: String, prompt: String },
    /// Send a message to an existing thread.
    SendToThread { thread_id: String, message: String },
    /// Respond to an approval request.
    Approve { thread_id: String, decision: bool },
    /// No action needed (e.g., unrelated webhook event).
    Ignore,
}
