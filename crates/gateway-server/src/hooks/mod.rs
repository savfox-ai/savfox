use std::sync::Arc;

use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::auth::GatewayAuth;
use crate::bridge::GatewayBridge;

pub(crate) mod event_bus;
pub(crate) mod llm_hook;
pub(crate) mod transformer;
pub(crate) mod validator;

pub(crate) use event_bus::{EventBus, HookEvent};

/// Maximum body size for hook payloads (256 KB).
const MAX_BODY_SIZE: usize = 256 * 1024;

// ─── Hook mapping types ──────────────────────────────────────────────────────

/// A hook mapping defines how an inbound webhook request is routed
/// to an internal agent or channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HookMapping {
    /// Unique mapping identifier.
    pub id: String,
    /// Target agent ID.
    pub agent: String,
    /// Optional target channel.
    #[serde(default)]
    pub channel: Option<String>,
    /// Optional payload transform (JSONPath expression or template).
    #[serde(default)]
    pub transform: Option<String>,
}

// ─── Handlers ────────────────────────────────────────────────────────────────

/// `POST /hooks/wake` Wake the agent (immediate or on next heartbeat).
#[handler]
pub(crate) async fn wake_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !authenticate(req, depot, res).await {
        return;
    }

    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(b) => b.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let body: Value = match req.parse_json().await {
        Ok(v) => v,
        Err(_) => Value::Object(serde_json::Map::new()),
    };

    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("wake");
    let agent = body
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    info!(agent = agent, "hook: wake received");

    match bridge.invoke_agent_text(message, agent).await {
        Ok(reply) => {
            res.render(Text::Json(
                json!({"status": "ok", "response": reply}).to_string(),
            ));
        }
        Err(err) => {
            warn!("wake hook agent error: {err}");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({"status": "error", "message": format!("{err}")}).to_string(),
            ));
        }
    }
}

/// `POST /hooks/agent`: Direct message dispatch to an agent.
#[handler]
pub(crate) async fn agent_hook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !authenticate(req, depot, res).await {
        return;
    }

    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(b) => b.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let body: Value = match req.parse_json().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": format!("invalid body: {err}")}).to_string(),
            ));
            return;
        }
    };

    let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let agent = body
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    if message.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "missing 'message' field"}).to_string(),
        ));
        return;
    }

    info!(agent = agent, "hook: agent message received");

    match bridge.invoke_agent_text(message, agent).await {
        Ok(reply) => {
            res.render(Text::Json(
                json!({"status": "ok", "response": reply}).to_string(),
            ));
        }
        Err(err) => {
            warn!("agent hook error: {err}");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({"status": "error", "message": format!("{err}")}).to_string(),
            ));
        }
    }
}

/// `POST /hooks/{mapping_id}`: Custom hook mapping route.
#[handler]
pub(crate) async fn custom_hook_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    if !authenticate(req, depot, res).await {
        return;
    }

    let mapping_id = req.param::<String>("mapping_id").unwrap_or_default();

    let bridge = match depot.obtain::<Arc<GatewayBridge>>() {
        Ok(b) => b.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let body: Value = match req.parse_json().await {
        Ok(v) => v,
        Err(_) => Value::Object(serde_json::Map::new()),
    };

    info!(mapping_id = %mapping_id, "hook: custom mapping triggered");

    // For custom mappings, extract the message from the body.
    // The transform field would apply a JSONPath or template to extract the message,
    // but for now we support a simple "message" field or stringify the entire body.
    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| {
            // Use the entire body as the message context.
            serde_json::to_string_pretty(&body).unwrap_or_default()
        });

    let agent = body
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    match bridge.invoke_agent_text(&message, agent).await {
        Ok(reply) => {
            res.render(Text::Json(
                json!({
                    "status": "ok",
                    "mapping_id": mapping_id,
                    "response": reply,
                })
                .to_string(),
            ));
        }
        Err(err) => {
            warn!("custom hook error: {err}");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({"status": "error", "message": format!("{err}")}).to_string(),
            ));
        }
    }
}

// ─── Auth helper ─────────────────────────────────────────────────────────────

/// Validate bearer token. Returns `true` if authenticated, `false` if rejected
/// (and the response has already been set).
async fn authenticate(req: &Request, depot: &mut Depot, res: &mut Response) -> bool {
    let auth = match depot.obtain::<Arc<GatewayAuth>>() {
        Ok(a) => a.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return false;
        }
    };

    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_owned());

    let Some(token) = token else {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(
            json!({"error": "missing Authorization header"}).to_string(),
        ));
        return false;
    };

    if auth.validate(&token).await.is_none() {
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Text::Json(json!({"error": "invalid token"}).to_string()));
        return false;
    }

    true
}
