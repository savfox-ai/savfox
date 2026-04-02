use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::channel::GatewayChannel;
use crate::session::GatewaySessionManager;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ApprovalStore {
    #[serde(default)]
    pending: Vec<ExecApprovalRequest>,
    #[serde(default)]
    resolved: Vec<ExecApprovalResolution>,
}

fn approval_store_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn approval_store_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("exec-approvals.json")
}

async fn load_store(path: &Path) -> ApprovalStore {
    crate::json_store::load_json(path, "approvals store")
        .await
        .unwrap_or_default()
}

async fn save_store(path: &Path, store: &ApprovalStore) -> Result<(), String> {
    crate::json_store::save_json(path, store, "approvals store").await
}

pub(crate) async fn persist_pending_approval(
    savfox_home: &Path,
    request: &ExecApprovalRequest,
) -> Result<(), String> {
    let _guard = approval_store_lock().lock().await;
    let path = approval_store_path(savfox_home);
    let mut store = load_store(&path).await;
    if let Some(existing) = store
        .pending
        .iter_mut()
        .find(|entry| entry.id == request.id)
    {
        *existing = request.clone();
    } else {
        store.pending.push(request.clone());
    }
    save_store(&path, &store).await
}

pub(crate) async fn persist_resolved_approval(
    savfox_home: &Path,
    resolution: &ExecApprovalResolution,
) -> Result<bool, String> {
    let _guard = approval_store_lock().lock().await;
    let path = approval_store_path(savfox_home);
    let mut store = load_store(&path).await;
    let mut found = false;
    store.pending.retain(|entry| {
        let keep = entry.id != resolution.id;
        if !keep {
            found = true;
        }
        keep
    });
    store.resolved.push(resolution.clone());
    save_store(&path, &store).await?;
    Ok(found)
}

pub(crate) async fn list_pending_approvals(
    savfox_home: &Path,
) -> Result<Vec<ExecApprovalRequest>, String> {
    let _guard = approval_store_lock().lock().await;
    let path = approval_store_path(savfox_home);
    let store = load_store(&path).await;
    Ok(store.pending)
}

/// An exec approval request from an agent that needs human authorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExecApprovalRequest {
    /// Unique ID for this approval request.
    pub id: String,
    /// The command to be executed.
    pub command: String,
    /// Working directory for the command.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Host where the command will run.
    #[serde(default)]
    pub host: Option<String>,
    /// Security classification of the command.
    #[serde(default)]
    pub security: Option<String>,
    /// Explanation of why the command needs approval.
    #[serde(default)]
    pub ask: Option<String>,
    /// Agent that requested the command.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Session id for the requesting session.
    #[serde(default)]
    pub session_id: Option<String>,
    /// When the request was created (epoch ms).
    #[serde(default)]
    pub created_at_ms: u64,
    /// When the request expires (epoch ms).
    #[serde(default)]
    pub expires_at_ms: u64,
}

/// The resolution of an exec approval request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExecApprovalResolution {
    /// The approval request ID being resolved.
    pub id: String,
    /// Whether the request was approved.
    pub approved: bool,
    /// Who resolved it (e.g., "operator:discord:username").
    #[serde(default)]
    pub resolved_by: Option<String>,
    /// Optional reason for the decision.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Configuration for where to forward exec approvals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ApprovalForwardingConfig {
    /// Whether forwarding is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Forwarding mode: "session" (use session's last channel), "targets" (explicit), "both".
    #[serde(default = "defaults::mode")]
    pub mode: String,
    /// Explicit forwarding targets (e.g., "discord:123456").
    #[serde(default)]
    pub targets: Vec<String>,
    /// Only forward approvals from these agent IDs (empty = all).
    #[serde(default)]
    pub agent_filter: Vec<String>,
    /// Only forward approvals from sessions matching these patterns (empty = all).
    #[serde(default)]
    pub session_filter: Vec<String>,
}

mod defaults {
    pub fn mode() -> String {
        "targets".to_owned()
    }
}

/// Format an approval request into a chat message.
fn format_approval_message(request: &ExecApprovalRequest) -> String {
    let mut msg = String::new();

    msg.push_str("**Exec Approval Required**\n");

    if let Some(agent) = &request.agent_id {
        msg.push_str(&format!("Agent: `{agent}`\n"));
    }

    // Format the command with code fencing.
    if request.command.contains('\n') {
        msg.push_str(&format!("```\n{}\n```\n", request.command));
    } else {
        msg.push_str(&format!("Command: `{}`\n", request.command));
    }

    if let Some(cwd) = &request.cwd {
        msg.push_str(&format!("Working dir: `{cwd}`\n"));
    }

    if let Some(ask) = &request.ask {
        msg.push_str(&format!("Reason: {ask}\n"));
    }

    if let Some(security) = &request.security {
        msg.push_str(&format!("Security: {security}\n"));
    }

    msg.push_str(&format!("\nApproval ID: `{}`", request.id));
    msg.push_str("\nReply `+` to approve or `-` to deny.");
    msg.push_str("\nIf needed, reply `approve:");
    msg.push_str(&request.id);
    msg.push_str("` or `deny:");
    msg.push_str(&request.id);
    msg.push_str("`.");

    if request.expires_at_ms > 0 {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if request.expires_at_ms > now_ms {
            let remaining_secs = (request.expires_at_ms - now_ms) / 1000;
            msg.push_str(&format!(" (expires in {remaining_secs}s)"));
        }
    }

    msg
}

/// Forward an approval request to configured chat channels.
pub(crate) async fn forward_approval_to_chat(
    channel: &GatewayChannel,
    session_mgr: &GatewaySessionManager,
    request: &ExecApprovalRequest,
    config: &ApprovalForwardingConfig,
) {
    if !config.enabled {
        return;
    }

    // Check agent filter.
    if !config.agent_filter.is_empty()
        && let Some(agent_id) = &request.agent_id
            && !config.agent_filter.iter().any(|f| agent_id.contains(f)) {
                return;
            }

    // Check session filter.
    if !config.session_filter.is_empty()
        && let Some(session_id) = &request.session_id
            && !config.session_filter.iter().any(|f| session_id.contains(f)) {
                return;
            }

    let message = format_approval_message(request);

    // Forward to explicit targets.
    if config.mode == "targets" || config.mode == "both" {
        for target in &config.targets {
            if let Err(err) = channel
                .send_platform_message(target, &message, None, None, None)
                .await
            {
                warn!(target = target, "failed to forward approval: {err}");
            } else {
                info!(target = target, approval_id = %request.id, "forwarded approval request");
            }
        }
    }

    // Broadcast to all WebSocket clients.
    session_mgr
        .broadcast_to_all(
            "exec.approval.request",
            json!({
                "id": request.id,
                "command": request.command,
                "cwd": request.cwd,
                "agent_id": request.agent_id,
                "session_id": request.session_id,
                "ask": request.ask,
                "security": request.security,
                "created_at_ms": request.created_at_ms,
                "expires_at_ms": request.expires_at_ms,
            }),
        )
        .await;
}

/// Notify chat channels that an approval was resolved.
pub(crate) async fn notify_approval_resolved(
    channel: &GatewayChannel,
    session_mgr: &GatewaySessionManager,
    resolution: &ExecApprovalResolution,
    config: &ApprovalForwardingConfig,
) {
    if !config.enabled {
        return;
    }

    let status = if resolution.approved {
        "APPROVED"
    } else {
        "DENIED"
    };
    let mut message = format!("Exec approval `{}`: **{status}**", resolution.id);

    if let Some(by) = &resolution.resolved_by {
        message.push_str(&format!(" by {by}"));
    }
    if let Some(reason) = &resolution.reason {
        message.push_str(&format!("  - {reason}"));
    }

    // Notify targets.
    if config.mode == "targets" || config.mode == "both" {
        for target in &config.targets {
            let _ = channel
                .send_platform_message(target, &message, None, None, None)
                .await;
        }
    }

    // Broadcast to WebSocket clients.
    session_mgr
        .broadcast_to_all(
            "exec.approval.resolved",
            json!({
                "id": resolution.id,
                "approved": resolution.approved,
                "resolved_by": resolution.resolved_by,
                "reason": resolution.reason,
            }),
        )
        .await;
}

// ─── REST API Handlers ──────────────────────────────────────────────────────

/// `POST /api/exec/approval/request`  - Submit an exec approval request.
#[handler]
pub(crate) async fn approval_request_handler(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) {
    let channel = if let Ok(b) = depot.obtain::<Arc<GatewayChannel>>() { b.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let session_mgr = if let Ok(m) = depot.obtain::<Arc<GatewaySessionManager>>() { m.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let body = match req.parse_json::<Value>().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(json!({"error": format!("invalid JSON: {err}")})));
            return;
        }
    };

    let request: ExecApprovalRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(
                json!({"error": format!("invalid approval request: {err}")}),
            ));
            return;
        }
    };
    let mut request = request;
    if request.id.trim().is_empty() {
        request.id = uuid::Uuid::now_v7().to_string();
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if request.created_at_ms == 0 {
        request.created_at_ms = now_ms;
    }
    if request.expires_at_ms == 0 {
        request.expires_at_ms = now_ms + 300_000;
    }

    if let Err(err) = persist_pending_approval(&channel.config().savfox_home, &request).await {
        warn!("failed to persist approval request {}: {}", request.id, err);
    }

    // Load forwarding config from env vars.
    let config = load_forwarding_config();

    forward_approval_to_chat(&channel, &session_mgr, &request, &config).await;

    info!(approval_id = %request.id, "exec approval request received");
    res.render(Json(json!({
        "status": "forwarded",
        "id": request.id,
    })));
}

/// `POST /api/exec/approval/resolve`  - Resolve an exec approval request.
#[handler]
pub(crate) async fn approval_resolve_handler(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
) {
    let channel = if let Ok(b) = depot.obtain::<Arc<GatewayChannel>>() { b.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let session_mgr = if let Ok(m) = depot.obtain::<Arc<GatewaySessionManager>>() { m.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let body = match req.parse_json::<Value>().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(json!({"error": format!("invalid JSON: {err}")})));
            return;
        }
    };

    let resolution: ExecApprovalResolution = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(json!({"error": format!("invalid resolution: {err}")})));
            return;
        }
    };
    let resolved_pending =
        match persist_resolved_approval(&channel.config().savfox_home, &resolution).await {
            Ok(found) => found,
            Err(err) => {
                warn!(
                    "failed to persist approval resolution {}: {}",
                    resolution.id, err
                );
                false
            }
        };

    let config = load_forwarding_config();

    notify_approval_resolved(&channel, &session_mgr, &resolution, &config).await;

    info!(
        approval_id = %resolution.id,
        approved = resolution.approved,
        "exec approval resolved"
    );
    res.render(Json(json!({
        "status": "resolved",
        "id": resolution.id,
        "approved": resolution.approved,
        "resolved_pending": resolved_pending,
    })));
}

/// `GET /api/exec/approvals`  - List pending approval requests.
#[handler]
pub(crate) async fn approvals_list_handler(depot: &mut Depot, res: &mut Response) {
    let channel = if let Ok(b) = depot.obtain::<Arc<GatewayChannel>>() { b.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    match list_pending_approvals(&channel.config().savfox_home).await {
        Ok(approvals) => {
            let count = approvals.len();
            res.render(Json(json!({
                "approvals": approvals,
                "count": count,
            })));
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Json(json!({ "error": err })));
        }
    }
}

/// Load approval forwarding config from environment variables.
fn load_forwarding_config() -> ApprovalForwardingConfig {
    let enabled = std::env::var("SAVFOX_APPROVAL_FORWARDING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let mode = std::env::var("SAVFOX_APPROVAL_MODE").unwrap_or_else(|_| "targets".to_owned());

    let targets = std::env::var("SAVFOX_APPROVAL_TARGETS")
        .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
        .unwrap_or_default();

    let agent_filter = std::env::var("SAVFOX_APPROVAL_AGENT_FILTER")
        .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
        .unwrap_or_default();

    let session_filter = std::env::var("SAVFOX_APPROVAL_SESSION_FILTER")
        .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
        .unwrap_or_default();

    ApprovalForwardingConfig {
        enabled,
        mode,
        targets,
        agent_filter,
        session_filter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approval_store_roundtrip() {
        let root =
            std::env::temp_dir().join(format!("savfox-gateway-approvals-{}", uuid::Uuid::now_v7()));
        let req = ExecApprovalRequest {
            id: "req-1".to_string(),
            command: "echo hello".to_string(),
            cwd: Some("/tmp".to_string()),
            host: None,
            security: None,
            ask: Some("need approval".to_string()),
            agent_id: Some("default".to_string()),
            session_id: Some("0194f7b3-1d7b-7c40-ae3d-95b6ef93e140".to_string()),
            created_at_ms: 1,
            expires_at_ms: 2,
        };

        persist_pending_approval(&root, &req)
            .await
            .expect("persist pending");

        let pending = list_pending_approvals(&root).await.expect("list pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "req-1");

        let resolved = ExecApprovalResolution {
            id: "req-1".to_string(),
            approved: true,
            resolved_by: Some("tester".to_string()),
            reason: None,
        };
        let was_pending = persist_resolved_approval(&root, &resolved)
            .await
            .expect("persist resolved");
        assert!(was_pending);

        let pending_after = list_pending_approvals(&root)
            .await
            .expect("list pending after resolve");
        assert!(pending_after.is_empty());

        let _ = tokio::fs::remove_dir_all(&root).await;
    }
}
