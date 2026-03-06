//! WebChat: embeddable chat widget for third-party websites.
//!
//! Routes:
//! - POST /webchat/session  - create a new webchat session (returns session_id + token)
//! - POST /webchat/send  - send a message in a webchat session
//! - GET  /webchat/poll/:session_id  - long-poll for new messages
//! - POST /webchat/media  - upload media attachment
//! - GET  /webchat/widget.js  - serve the embeddable widget script

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::auth::GatewayAuth;
use crate::channel::GatewayChannel;

// ─── Configuration ───────────────────────────────────────────────────────────

/// Configuration for the embedded webchat widget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WebChatConfig {
    /// Allowed origins for CORS (empty = allow all).
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Maximum messages per session before rotation.
    #[serde(default = "default_max_messages")]
    pub max_messages_per_session: usize,
    /// Rate limit: maximum messages per minute per session.
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: u32,
    /// Welcome message displayed when a new session starts.
    #[serde(default = "default_welcome_message")]
    pub welcome_message: String,
    /// Long-poll timeout in seconds.
    #[serde(default = "default_poll_timeout_secs")]
    pub poll_timeout_secs: u64,
}

fn default_max_messages() -> usize {
    200
}
fn default_rate_limit() -> u32 {
    20
}
fn default_welcome_message() -> String {
    "Hello! How can I help you today?".to_owned()
}
fn default_poll_timeout_secs() -> u64 {
    30
}

impl Default for WebChatConfig {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            max_messages_per_session: default_max_messages(),
            rate_limit_per_minute: default_rate_limit(),
            welcome_message: default_welcome_message(),
            poll_timeout_secs: default_poll_timeout_secs(),
        }
    }
}

// ─── Types ───────────────────────────────────────────────────────────────────

/// A single message in a webchat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WebChatMessage {
    /// Message role: "user", "assistant", or "system".
    pub role: String,
    /// The message text content.
    pub content: String,
    /// Unix timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// Optional unique message identifier.
    #[serde(default)]
    pub message_id: Option<String>,
}

/// An active webchat session.
#[derive(Debug, Clone)]
struct WebChatSession {
    /// Unique session identifier.
    session_id: String,
    /// Bearer token scoped to this session.
    token: String,
    /// Optional user display name.
    user_name: Option<String>,
    /// Caller-provided metadata.
    metadata: Option<serde_json::Value>,
    /// Conversation history.
    messages: Vec<WebChatMessage>,
    /// Messages not yet retrieved by the client via polling.
    pending_messages: Vec<WebChatMessage>,
    /// Creation timestamp (Unix ms).
    created_at_ms: u64,
    /// Last activity timestamp (Unix ms).
    last_active_ms: u64,
    /// Sliding-window user message timestamps for rate limiting.
    send_timestamps_ms: Vec<u64>,
}

// ─── Session Store (in-memory) ───────────────────────────────────────────────

fn webchat_store() -> &'static Mutex<HashMap<String, WebChatSession>> {
    static STORE: OnceLock<Mutex<HashMap<String, WebChatSession>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Lookup a session by its scoped token.
async fn session_by_token(token: &str) -> Option<String> {
    let store = webchat_store().lock().await;
    store
        .values()
        .find(|s| s.token == token)
        .map(|s| s.session_id.clone())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn generate_session_id() -> String {
    format!("wc-{}", uuid::Uuid::now_v7())
}

fn generate_session_token() -> String {
    let bytes: [u8; 24] = rand::random();
    bytes.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// Extract a bearer token from the Authorization header.
fn extract_bearer_token(req: &Request) -> Option<String> {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_owned())
}

// ─── Request / Response types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    #[serde(default)]
    user_name: Option<String>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct SendMessageRequest {
    session_id: String,
    message: String,
}

// ─── Handlers ────────────────────────────────────────────────────────────────

/// `POST /webchat/session`  - Create a new webchat session.
///
/// Accepts optional `user_name` and `metadata` in the JSON body.
/// Returns `{ session_id, token, welcome_message }`.
#[handler]
pub(crate) async fn create_session_handler(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) {
    // Validate gateway auth first.
    let auth = match depot.obtain::<Arc<GatewayAuth>>() {
        Ok(a) => a.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let token = extract_bearer_token(req);
    if let Some(ref t) = token {
        if auth.validate(t).await.is_none() {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Json(
                json!({"error": "invalid or missing authorization token"}).to_string(),
            ));
            return;
        }
    }
    // Allow session creation without auth for public-facing widgets  - the
    // returned session token is scoped to this single session.

    let body: CreateSessionRequest = match req.parse_json().await {
        Ok(b) => b,
        Err(_) => CreateSessionRequest {
            user_name: None,
            metadata: None,
        },
    };

    let session_id = generate_session_id();
    let session_token = generate_session_token();
    let now = now_ms();

    let welcome_msg = WebChatMessage {
        role: "assistant".to_owned(),
        content: default_welcome_message(),
        timestamp_ms: now,
        message_id: Some(format!("msg-{}", uuid::Uuid::now_v7())),
    };

    let session = WebChatSession {
        session_id: session_id.clone(),
        token: session_token.clone(),
        user_name: body.user_name.clone(),
        metadata: body.metadata.clone(),
        messages: vec![welcome_msg.clone()],
        pending_messages: Vec::new(),
        created_at_ms: now,
        last_active_ms: now,
        send_timestamps_ms: Vec::new(),
    };

    {
        let mut store = webchat_store().lock().await;

        // Evict oldest sessions if the store grows too large.
        if store.len() >= 4096 {
            let mut entries: Vec<_> = store
                .values()
                .map(|s| (s.session_id.clone(), s.last_active_ms))
                .collect();
            entries.sort_by_key(|e| e.1);
            let remove_count = entries.len() / 4;
            for (sid, _) in entries.into_iter().take(remove_count) {
                store.remove(&sid);
            }
        }

        store.insert(session_id.clone(), session);
    }

    info!(
        session_id = %session_id,
        user_name = ?body.user_name,
        "webchat session created"
    );

    res.render(Text::Json(
        json!({
            "session_id": session_id,
            "token": session_token,
            "welcome_message": welcome_msg.content,
            "created_at_ms": now,
        })
        .to_string(),
    ));
}

/// `POST /webchat/send`  - Send a user message in a webchat session.
///
/// Requires Bearer token auth (the session-scoped token returned from create).
/// Accepts `{ session_id, message }`. Routes through GatewayChannel to the agent
/// and returns the agent's reply.
#[handler]
pub(crate) async fn send_message_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let channel = match depot.obtain::<Arc<GatewayChannel>>() {
        Ok(b) => b.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    // Authenticate via session-scoped token.
    let token = match extract_bearer_token(req) {
        Some(t) => t,
        None => {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Json(
                json!({"error": "missing Authorization header"}).to_string(),
            ));
            return;
        }
    };

    let body: SendMessageRequest = match req.parse_json().await {
        Ok(b) => b,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": format!("invalid request body: {err}")}).to_string(),
            ));
            return;
        }
    };

    if body.message.trim().is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "'message' must not be empty"}).to_string(),
        ));
        return;
    }

    // Validate that the token belongs to the claimed session.
    {
        let store = webchat_store().lock().await;
        let session = match store.get(&body.session_id) {
            Some(s) => s,
            None => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Text::Json(
                    json!({"error": "session not found"}).to_string(),
                ));
                return;
            }
        };
        if session.token != token {
            res.status_code(StatusCode::UNAUTHORIZED);
            res.render(Text::Json(
                json!({"error": "token does not match session"}).to_string(),
            ));
            return;
        }
    }

    let now = now_ms();

    // Public-facing per-session rate limit.
    {
        let mut store = webchat_store().lock().await;
        if let Some(session) = store.get_mut(&body.session_id) {
            let cutoff = now.saturating_sub(60_000);
            session.send_timestamps_ms.retain(|ts| *ts >= cutoff);
            if session.send_timestamps_ms.len() as u32 >= default_rate_limit() {
                res.status_code(StatusCode::TOO_MANY_REQUESTS);
                res.render(Text::Json(
                    json!({
                        "error": "rate limit exceeded",
                        "limit_per_minute": default_rate_limit(),
                    })
                    .to_string(),
                ));
                return;
            }
            session.send_timestamps_ms.push(now);
        }
    }

    // Record the user message.
    let user_msg = WebChatMessage {
        role: "user".to_owned(),
        content: body.message.clone(),
        timestamp_ms: now,
        message_id: Some(format!("msg-{}", uuid::Uuid::now_v7())),
    };

    {
        let mut store = webchat_store().lock().await;
        if let Some(session) = store.get_mut(&body.session_id) {
            session.messages.push(user_msg.clone());
            session.last_active_ms = now;

            // Enforce max messages: trim oldest (keep first welcome message).
            if session.messages.len() > default_max_messages() {
                let keep_from = session.messages.len() - default_max_messages();
                session.messages = session.messages.split_off(keep_from);
            }
        }
    }

    // Route through the channel to the agent.
    let reply_text = match channel.invoke_agent_text(&body.message, "default").await {
        Ok(text) => text,
        Err(err) => {
            warn!(session_id = %body.session_id, "webchat agent invocation failed: {err}");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({"error": format!("agent error: {err}")}).to_string(),
            ));
            return;
        }
    };

    let reply_now = now_ms();
    let assistant_msg = WebChatMessage {
        role: "assistant".to_owned(),
        content: reply_text.clone(),
        timestamp_ms: reply_now,
        message_id: Some(format!("msg-{}", uuid::Uuid::now_v7())),
    };

    // Record the assistant reply and add to pending for poll clients.
    {
        let mut store = webchat_store().lock().await;
        if let Some(session) = store.get_mut(&body.session_id) {
            session.messages.push(assistant_msg.clone());
            session.pending_messages.push(assistant_msg.clone());
            session.last_active_ms = reply_now;
        }
    }

    info!(
        session_id = %body.session_id,
        reply_len = reply_text.len(),
        "webchat message processed"
    );

    res.render(Text::Json(
        json!({
            "session_id": body.session_id,
            "reply": {
                "role": assistant_msg.role,
                "content": assistant_msg.content,
                "timestamp_ms": assistant_msg.timestamp_ms,
                "message_id": assistant_msg.message_id,
            },
        })
        .to_string(),
    ));
}

/// `GET /webchat/poll/<session_id>`  - Long-poll for new messages.
///
/// Returns any pending messages for the session. If none are available,
/// waits up to 30 seconds before returning an empty array.
#[handler]
pub(crate) async fn poll_handler(req: &mut Request, res: &mut Response) {
    let session_id = req.param::<String>("session_id").unwrap_or_default();

    if session_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "missing session_id in path"}).to_string(),
        ));
        return;
    }

    // Authenticate via query param or header token.
    let token = extract_bearer_token(req).or_else(|| req.query::<String>("token"));

    if let Some(ref t) = token {
        let store = webchat_store().lock().await;
        if let Some(session) = store.get(&session_id) {
            if session.token != *t {
                drop(store);
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Json(
                    json!({"error": "token does not match session"}).to_string(),
                ));
                return;
            }
        } else {
            drop(store);
            res.status_code(StatusCode::NOT_FOUND);
            res.render(Text::Json(
                json!({"error": "session not found"}).to_string(),
            ));
            return;
        }
    } else {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(json!({"error": "missing token"}).to_string()));
        return;
    }

    let poll_timeout = std::time::Duration::from_secs(default_poll_timeout_secs());
    let deadline = tokio::time::Instant::now() + poll_timeout;
    let poll_interval = std::time::Duration::from_millis(500);

    loop {
        // Check for pending messages.
        {
            let mut store = webchat_store().lock().await;
            if let Some(session) = store.get_mut(&session_id) {
                if !session.pending_messages.is_empty() {
                    let messages: Vec<_> = session.pending_messages.drain(..).collect();
                    session.last_active_ms = now_ms();

                    drop(store);
                    res.render(Text::Json(
                        json!({
                            "session_id": session_id,
                            "messages": messages,
                        })
                        .to_string(),
                    ));
                    return;
                }
            } else {
                drop(store);
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Text::Json(
                    json!({"error": "session not found"}).to_string(),
                ));
                return;
            }
        }

        // Check timeout.
        if tokio::time::Instant::now() >= deadline {
            res.render(Text::Json(
                json!({
                    "session_id": session_id,
                    "messages": [],
                })
                .to_string(),
            ));
            return;
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// `GET /webchat/widget.js`  - Serve the embeddable widget JavaScript snippet.
///
/// This returns a minimal JS file that third-party sites can include via
/// `<script src="https://gateway-host/webchat/widget.js"></script>`.
/// The script creates an iframe-based chat widget in the host page.
#[handler]
pub(crate) async fn widget_js_handler(req: &mut Request, res: &mut Response) {
    // Derive the gateway origin from the incoming request so the widget
    // knows where to point API calls.
    let scheme = if req.uri().scheme_str() == Some("https") {
        "https"
    } else {
        // Default to the scheme implied by the request; fall back to http.
        "http"
    };

    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:18881");

    let origin = format!("{scheme}://{host}");

    let js = format!(
        r##"(function(){{
  "use strict";
  if(window.__savfoxWebChat) return;
  window.__savfoxWebChat = true;

  var ORIGIN = "{origin}";
  var CFG = window.SAVFOX_WEBCHAT_CONFIG || {{}};
  var BRAND_NAME = CFG.name || "Savfox Chat";
  var BRAND_AVATAR = CFG.avatar || "\u{{1F4AC}}";
  var PRIMARY_COLOR = CFG.primaryColor || "#2563eb";
  var SECONDARY_BG = CFG.secondaryBg || "#f3f4f6";

  // Create container
  var container = document.createElement("div");
  container.id = "savfox-gateway-dioxuschat-container";
  container.style.cssText = "position:fixed;bottom:20px;right:20px;z-index:999999;font-family:system-ui,-apple-system,sans-serif;";

  // Toggle button
  var btn = document.createElement("button");
  btn.id = "savfox-gateway-dioxuschat-toggle";
  btn.textContent = BRAND_AVATAR;
  btn.style.cssText = "width:56px;height:56px;border-radius:50%;border:none;background:" + PRIMARY_COLOR + ";color:#fff;font-size:24px;cursor:pointer;box-shadow:0 4px 12px rgba(0,0,0,0.15);";
  container.appendChild(btn);

  // Chat panel
  var panel = document.createElement("div");
  panel.id = "savfox-gateway-dioxuschat-panel";
  panel.style.cssText = "display:none;width:380px;height:520px;border-radius:12px;overflow:hidden;box-shadow:0 8px 30px rgba(0,0,0,0.12);background:#fff;flex-direction:column;position:absolute;bottom:66px;right:0;";

  // Header
  var header = document.createElement("div");
  header.style.cssText = "background:" + PRIMARY_COLOR + ";color:#fff;padding:14px 16px;font-weight:600;font-size:15px;";
  header.textContent = BRAND_NAME;
  panel.appendChild(header);

  // Messages area
  var msgs = document.createElement("div");
  msgs.id = "savfox-gateway-dioxuschat-messages";
  msgs.style.cssText = "flex:1;overflow-y:auto;padding:12px;display:flex;flex-direction:column;gap:8px;height:380px;";
  panel.appendChild(msgs);

  // Input area
  var inputRow = document.createElement("div");
  inputRow.style.cssText = "display:flex;border-top:1px solid #e5e7eb;padding:8px;gap:8px;";
  var input = document.createElement("input");
  input.type = "text";
  input.placeholder = "Type a message...";
  input.style.cssText = "flex:1;border:1px solid #d1d5db;border-radius:8px;padding:8px 12px;outline:none;font-size:14px;";
  var sendBtn = document.createElement("button");
  sendBtn.textContent = "Send";
  sendBtn.style.cssText = "background:" + PRIMARY_COLOR + ";color:#fff;border:none;border-radius:8px;padding:8px 16px;cursor:pointer;font-size:14px;font-weight:500;";
  inputRow.appendChild(input);
  inputRow.appendChild(sendBtn);
  panel.appendChild(inputRow);
  container.appendChild(panel);
  document.body.appendChild(container);

  var sessionId = null;
  var sessionToken = null;
  var isOpen = false;

  function addMessage(role, content) {{
    var bubble = document.createElement("div");
    var isUser = role === "user";
    bubble.style.cssText = "max-width:80%;padding:10px 14px;border-radius:12px;font-size:14px;line-height:1.4;word-wrap:break-word;" +
      (isUser ? "background:" + PRIMARY_COLOR + ";color:#fff;align-self:flex-end;margin-left:auto;" : "background:" + SECONDARY_BG + ";color:#111;align-self:flex-start;");
    bubble.textContent = content;
    msgs.appendChild(bubble);
    msgs.scrollTop = msgs.scrollHeight;
  }}

  function initSession() {{
    return fetch(ORIGIN + "/webchat/session", {{
      method: "POST",
      headers: {{ "Content-Type": "application/json" }},
      body: JSON.stringify({{}})
    }})
    .then(function(r) {{ return r.json(); }})
    .then(function(data) {{
      sessionId = data.session_id;
      sessionToken = data.token;
      if(data.welcome_message) addMessage("assistant", data.welcome_message);
    }});
  }}

  function sendMessage(text) {{
    if(!text.trim() || !sessionId) return;
    addMessage("user", text);
    input.value = "";
    input.disabled = true;
    sendBtn.disabled = true;

    fetch(ORIGIN + "/webchat/send", {{
      method: "POST",
      headers: {{
        "Content-Type": "application/json",
        "Authorization": "Bearer " + sessionToken
      }},
      body: JSON.stringify({{ session_id: sessionId, message: text }})
    }})
    .then(function(r) {{ return r.json(); }})
    .then(function(data) {{
      if(data.reply) addMessage("assistant", data.reply.content);
      else if(data.error) addMessage("assistant", "Error: " + data.error);
    }})
    .catch(function() {{ addMessage("assistant", "Sorry, something went wrong."); }})
    .finally(function() {{ input.disabled = false; sendBtn.disabled = false; input.focus(); }});
  }}

  btn.addEventListener("click", function() {{
    isOpen = !isOpen;
    panel.style.display = isOpen ? "flex" : "none";
    btn.textContent = isOpen ? "\u2715" : BRAND_AVATAR;
    if(isOpen && !sessionId) initSession();
    if(isOpen) input.focus();
  }});

  sendBtn.addEventListener("click", function() {{ sendMessage(input.value); }});
  input.addEventListener("keydown", function(e) {{
    if(e.key === "Enter") sendMessage(input.value);
  }});
}})();
"##
    );

    res.headers_mut().insert(
        "content-type",
        "application/javascript; charset=utf-8".parse().unwrap(),
    );
    res.headers_mut()
        .insert("cache-control", "public, max-age=300".parse().unwrap());

    res.render(Text::Plain(js));
}

// ─── Router ──────────────────────────────────────────────────────────────────

/// Build the webchat sub-router with all webchat endpoints.
pub(crate) fn webchat_router() -> Router {
    Router::with_path("webchat")
        .push(Router::with_path("session").post(create_session_handler))
        .push(Router::with_path("send").post(send_message_handler))
        .push(Router::with_path("poll/<session_id>").get(poll_handler))
        .push(Router::with_path("widget.js").get(widget_js_handler))
}
