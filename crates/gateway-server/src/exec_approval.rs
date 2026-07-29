use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use salvo::prelude::*;
use savfox_utils::home_dir::GATEWAY_SUBDIR;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::auth::{TokenInfo, TokenScope};
use crate::channel::GatewayChannel;
use crate::security::approval_coordinator::{
    AuthenticatedApprovalOutcome, resolve_authenticated_approval,
};
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
    savfox_home.join(GATEWAY_SUBDIR).join("exec-approvals.json")
}

pub(crate) fn sanitized_approval_text(input: &str, max_chars: usize) -> String {
    let redacted = crate::redaction::redact_text_always(input);
    let mut chars = redacted.chars();
    let mut output = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        output.push('…');
    }
    output
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

/// Result of an attempt to resolve an approval. The nonce check and the
/// pending-list mutation happen inside one lock so the same nonce cannot
/// race against itself for two concurrent resolutions (TOCTOU).
pub(crate) enum ResolveOutcome {
    /// The matching approval was on the pending list with a valid nonce
    /// and has now been moved to `resolved`. Returns `true`.
    Resolved,
    /// The approval id was already resolved or was never on the pending list.
    /// The store is not mutated, preventing replay and audit-log poisoning.
    NotPending,
    /// The presented nonce did not match — refused without mutating.
    NonceMismatch,
    /// The pending entry exists but has no server-issued nonce (legacy
    /// requests persisted before S3). Refused without mutating.
    LegacyMissingNonce,
}

pub(crate) async fn persist_resolved_approval(
    savfox_home: &Path,
    resolution: &ExecApprovalResolution,
) -> Result<ResolveOutcome, String> {
    use subtle::ConstantTimeEq;
    let _guard = approval_store_lock().lock().await;
    let path = approval_store_path(savfox_home);
    let mut store = load_store(&path).await;

    // Verify the nonce before mutating anything. Both the pending entry
    // and the presented nonce are required; everything else is rejected
    // before a write hits the disk.
    let pending_entry = store.pending.iter().find(|r| r.id == resolution.id);
    match pending_entry {
        Some(entry) if entry.nonce.is_empty() => {
            return Ok(ResolveOutcome::LegacyMissingNonce);
        }
        Some(entry) => {
            if !bool::from(entry.nonce.as_bytes().ct_eq(resolution.nonce.as_bytes())) {
                return Ok(ResolveOutcome::NonceMismatch);
            }
        }
        None => return Ok(ResolveOutcome::NotPending),
    }

    store.pending.retain(|entry| entry.id != resolution.id);
    let mut audit_resolution = resolution.clone();
    // The nonce has served its single-use correlation purpose. Do not retain
    // it in the resolved audit history.
    audit_resolution.nonce.clear();
    store.resolved.push(audit_resolution);
    save_store(&path, &store).await?;
    Ok(ResolveOutcome::Resolved)
}

pub(crate) async fn list_pending_approvals(
    savfox_home: &Path,
) -> Result<Vec<ExecApprovalRequest>, String> {
    let _guard = approval_store_lock().lock().await;
    let path = approval_store_path(savfox_home);
    let mut store = load_store(&path).await;
    let now_ms = crate::json_store::now_ms();
    let mut expired = Vec::new();
    store.pending.retain(|request| {
        let is_expired = request.expires_at_ms != 0 && request.expires_at_ms <= now_ms;
        if is_expired {
            expired.push(ExecApprovalResolution {
                id: request.id.clone(),
                approved: false,
                resolved_by: Some("gateway".to_owned()),
                reason: Some("approval request expired".to_owned()),
                nonce: String::new(),
            });
        }
        !is_expired
    });
    if !expired.is_empty() {
        store.resolved.extend(expired);
        save_store(&path, &store).await?;
    }
    Ok(store.pending)
}

pub(crate) async fn find_pending_approval(
    savfox_home: &Path,
    request_id: &str,
) -> Result<Option<ExecApprovalRequest>, String> {
    Ok(list_pending_approvals(savfox_home)
        .await?
        .into_iter()
        .find(|request| request.id == request_id))
}

/// An exec approval request from an agent that needs human authorization.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Server-generated single-use nonce that resolves of this approval
    /// MUST echo (S3 in the security review). The listing endpoint
    /// returns the nonce alongside the request so a legitimate operator
    /// who has Read access can resolve it; an attacker that has only
    /// the resolve scope (or knows the request id from a leak but never
    /// listed) cannot guess the nonce. The nonce is also single-use:
    /// once a resolution consumes it the same nonce cannot resolve a
    /// different request — defends against replay.
    #[serde(default)]
    pub nonce: String,
    /// Unified coordinator request kind (`exec`, `patch`, ...).
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub channel_instance_id: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub peer_id: Option<String>,
    #[serde(default)]
    pub logical_session_id: Option<String>,
    #[serde(default)]
    pub core_session_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub available_decisions: Vec<String>,
    #[serde(default)]
    pub policy_fingerprint: Option<String>,
}

impl ExecApprovalRequest {
    #[must_use]
    pub(crate) fn is_coordinator_owned(&self) -> bool {
        self.kind.is_some() || self.core_session_id.is_some() || self.turn_id.is_some()
    }
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
    /// Single-use nonce echoed from the [`ExecApprovalRequest::nonce`]
    /// the server generated. Resolutions without a matching nonce are
    /// rejected with `400 Bad Request` (S3).
    #[serde(default)]
    pub nonce: String,
}

/// Generate a fresh single-use nonce for an approval request. 32 bytes
/// of cryptographic randomness, hex-encoded → 64 chars.
pub(crate) fn generate_approval_nonce() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
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
        && !config.agent_filter.iter().any(|f| agent_id.contains(f))
    {
        return;
    }

    // Check session filter.
    if !config.session_filter.is_empty()
        && let Some(session_id) = &request.session_id
        && !config.session_filter.iter().any(|f| session_id.contains(f))
    {
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
    let channel = if let Ok(b) = depot.get_typed::<Arc<GatewayChannel>>() {
        b.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let session_mgr = if let Ok(m) = depot.get_typed::<Arc<GatewaySessionManager>>() {
        m.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };
    let token_info = match depot.get_typed::<TokenInfo>() {
        Ok(info) if info.has_scope(TokenScope::OperatorApprovalsRequest) => info,
        _ => {
            res.status_code(StatusCode::FORBIDDEN);
            res.render(Json(json!({"error": "approval request scope required"})));
            return;
        }
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
    request.id = uuid::Uuid::now_v7().to_string();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    request.created_at_ms = now_ms;
    request.expires_at_ms = now_ms.saturating_add(300_000);
    request.command = sanitized_approval_text(&request.command, 2_048);
    request.ask = request
        .ask
        .map(|reason| sanitized_approval_text(&reason, 512));
    // The REST request endpoint creates legacy durable-only approvals. Core
    // correlation fields are server-owned and cannot be injected by callers.
    request.kind = None;
    request.channel_instance_id = None;
    request.account_id = None;
    request.peer_id = None;
    request.logical_session_id = None;
    request.core_session_id = None;
    request.turn_id = None;
    request.environment_id = None;
    request.available_decisions.clear();
    request.policy_fingerprint = None;
    // S3: always overwrite caller-supplied nonce with a server-generated
    // one. Even if a malicious agent guesses an id, it cannot inject the
    // matching nonce because that field is regenerated here.
    request.nonce = generate_approval_nonce();

    if let Err(err) = persist_pending_approval(&channel.config().savfox_home, &request).await {
        warn!("failed to persist approval request {}: {}", request.id, err);
    }

    // Load forwarding config from env vars.
    let config = load_forwarding_config();

    forward_approval_to_chat(&channel, &session_mgr, &request, &config).await;

    info!(
        approval_id = %request.id,
        requested_by = %token_info.label,
        "exec approval request received"
    );
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
    let channel = if let Ok(b) = depot.get_typed::<Arc<GatewayChannel>>() {
        b.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let session_mgr = if let Ok(m) = depot.get_typed::<Arc<GatewaySessionManager>>() {
        m.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };
    let token_info = match depot.get_typed::<TokenInfo>() {
        Ok(info) if info.has_scope(TokenScope::OperatorApprovalsResolve) => info,
        _ => {
            res.status_code(StatusCode::FORBIDDEN);
            res.render(Json(json!({"error": "approval resolve scope required"})));
            return;
        }
    };

    let body = match req.parse_json::<Value>().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(json!({"error": format!("invalid JSON: {err}")})));
            return;
        }
    };

    let requested_decision = body
        .get("decision")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let resolution: ExecApprovalResolution = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(json!({"error": format!("invalid resolution: {err}")})));
            return;
        }
    };
    let decision = requested_decision
        .as_deref()
        .unwrap_or(if resolution.approved {
            "approve-once"
        } else {
            "deny"
        });
    let authenticated_subject = format!("token:{}", token_info.label);
    let sanitized_reason = resolution
        .reason
        .as_deref()
        .map(|reason| sanitized_approval_text(reason, 512));
    match resolve_authenticated_approval(
        &channel,
        &resolution.id,
        &resolution.nonce,
        decision,
        Some(authenticated_subject.clone()),
        sanitized_reason.clone(),
    )
    .await
    {
        Ok(AuthenticatedApprovalOutcome::Resolved { decision }) => {
            let canonical_resolution = ExecApprovalResolution {
                id: resolution.id.clone(),
                approved: decision.starts_with("approved"),
                resolved_by: Some(authenticated_subject),
                reason: sanitized_reason,
                nonce: resolution.nonce.clone(),
            };
            let config = load_forwarding_config();
            notify_approval_resolved(&channel, &session_mgr, &canonical_resolution, &config).await;
            res.render(Json(json!({
                "status": "resolved",
                "id": resolution.id,
                "decision": decision,
                "resolved_pending": true,
                "coordinated": true,
            })));
            return;
        }
        Ok(AuthenticatedApprovalOutcome::NonceMismatch) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(
                json!({"error": "nonce mismatch", "id": resolution.id}),
            ));
            return;
        }
        Ok(AuthenticatedApprovalOutcome::UnsupportedDecision) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(
                json!({"error": "unsupported decision", "id": resolution.id}),
            ));
            return;
        }
        Ok(AuthenticatedApprovalOutcome::NotCoordinated) => {}
        Err(error) => {
            warn!(approval_id = %resolution.id, %error, "failed to submit coordinated approval");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Json(
                json!({"error": "approval delivery failed", "id": resolution.id}),
            ));
            return;
        }
    }
    match find_pending_approval(&channel.config().savfox_home, &resolution.id).await {
        Ok(Some(request)) if request.is_coordinator_owned() => {
            res.status_code(StatusCode::CONFLICT);
            res.render(Json(json!({
                "error": "approval coordinator is no longer active; re-issue the request",
                "id": resolution.id,
            })));
            return;
        }
        Ok(_) => {}
        Err(error) => {
            warn!(approval_id = %resolution.id, %error, "failed to inspect pending approval");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Json(
                json!({"error": "approval lookup failed", "id": resolution.id}),
            ));
            return;
        }
    }
    // S3: nonce verification + persistence happen atomically inside
    // `persist_resolved_approval`. Without a valid nonce, no write hits
    // disk and the resolution is rejected with 400.
    let canonical_resolution = ExecApprovalResolution {
        id: resolution.id.clone(),
        approved: resolution.approved,
        resolved_by: Some(authenticated_subject),
        reason: sanitized_reason,
        nonce: resolution.nonce.clone(),
    };
    let outcome =
        match persist_resolved_approval(&channel.config().savfox_home, &canonical_resolution).await
        {
            Ok(o) => o,
            Err(err) => {
                warn!(
                    "failed to persist approval resolution {}: {}",
                    resolution.id, err
                );
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Json(
                    json!({"error": "persist failed", "id": resolution.id}),
                ));
                return;
            }
        };
    let resolved_pending = match outcome {
        ResolveOutcome::Resolved => true,
        ResolveOutcome::NotPending => {
            res.status_code(StatusCode::NOT_FOUND);
            res.render(Json(json!({
                "error": "approval is not pending",
                "id": resolution.id,
            })));
            return;
        }
        ResolveOutcome::NonceMismatch => {
            warn!(
                approval_id = %resolution.id,
                "rejecting approval resolution: nonce mismatch"
            );
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(json!({
                "error": "approval nonce missing or invalid",
                "id": resolution.id,
            })));
            return;
        }
        ResolveOutcome::LegacyMissingNonce => {
            warn!(
                approval_id = %resolution.id,
                "rejecting approval resolution: legacy approval has no server-issued nonce"
            );
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(json!({
                "error": "approval has no server-issued nonce; re-issue the request",
                "id": resolution.id,
            })));
            return;
        }
    };

    let config = load_forwarding_config();

    notify_approval_resolved(&channel, &session_mgr, &canonical_resolution, &config).await;

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
    let channel = if let Ok(b) = depot.get_typed::<Arc<GatewayChannel>>() {
        b.clone()
    } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };
    match depot.get_typed::<TokenInfo>() {
        Ok(info) if info.has_scope(TokenScope::OperatorApprovalsRead) => {}
        _ => {
            res.status_code(StatusCode::FORBIDDEN);
            res.render(Json(json!({"error": "approval read scope required"})));
            return;
        }
    }

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

    fn temp_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "savfox-gateway-approvals-{label}-{}",
            uuid::Uuid::now_v7()
        ))
    }

    fn sample_request(id: &str, nonce: &str) -> ExecApprovalRequest {
        let now_ms = crate::json_store::now_ms();
        ExecApprovalRequest {
            id: id.to_owned(),
            command: "echo hello".to_owned(),
            cwd: Some("/tmp".to_owned()),
            host: None,
            security: None,
            ask: Some("need approval".to_owned()),
            agent_id: Some("default".to_owned()),
            session_id: Some("0194f7b3-1d7b-7c40-ae3d-95b6ef93e140".to_owned()),
            created_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(60_000),
            nonce: nonce.to_owned(),
            ..Default::default()
        }
    }

    fn sample_resolution(id: &str, nonce: &str) -> ExecApprovalResolution {
        ExecApprovalResolution {
            id: id.to_owned(),
            approved: true,
            resolved_by: Some("tester".to_owned()),
            reason: None,
            nonce: nonce.to_owned(),
        }
    }

    #[tokio::test]
    async fn approval_store_roundtrip_with_matching_nonce() {
        let root = temp_root("happy");
        let nonce = generate_approval_nonce();
        let req = sample_request("req-1", &nonce);

        persist_pending_approval(&root, &req)
            .await
            .expect("persist pending");

        let pending = list_pending_approvals(&root).await.expect("list pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "req-1");
        assert_eq!(pending[0].nonce, nonce, "nonce surfaces through listing");

        let resolved = sample_resolution("req-1", &nonce);
        let outcome = persist_resolved_approval(&root, &resolved)
            .await
            .expect("persist resolved");
        assert!(matches!(outcome, ResolveOutcome::Resolved));

        let pending_after = list_pending_approvals(&root)
            .await
            .expect("list pending after resolve");
        assert!(pending_after.is_empty());
        let store = load_store(&approval_store_path(&root)).await;
        assert_eq!(store.resolved.len(), 1);
        assert!(
            store.resolved[0].nonce.is_empty(),
            "consumed nonce must not remain in audit history"
        );

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn resolution_with_wrong_nonce_is_rejected() {
        let root = temp_root("nonce-mismatch");
        let req = sample_request("req-2", &generate_approval_nonce());
        persist_pending_approval(&root, &req).await.unwrap();

        let bad = sample_resolution("req-2", &generate_approval_nonce());
        let outcome = persist_resolved_approval(&root, &bad).await.unwrap();
        assert!(matches!(outcome, ResolveOutcome::NonceMismatch));

        // Pending list is untouched — the rejected resolution did not
        // remove the entry.
        let pending = list_pending_approvals(&root).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "req-2");

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn legacy_approval_with_empty_nonce_cannot_be_resolved() {
        let root = temp_root("legacy");
        // A request persisted before this PR landed has an empty nonce
        // (default for the new field). Resolution must refuse rather
        // than silently accept any input.
        let req = sample_request("req-3", "");
        persist_pending_approval(&root, &req).await.unwrap();

        let res = sample_resolution("req-3", "anything");
        let outcome = persist_resolved_approval(&root, &res).await.unwrap();
        assert!(matches!(outcome, ResolveOutcome::LegacyMissingNonce));
        assert_eq!(list_pending_approvals(&root).await.unwrap().len(), 1);

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn nonce_is_single_use_and_cannot_be_replayed() {
        let root = temp_root("replay");
        let nonce = generate_approval_nonce();
        let req = sample_request("req-4", &nonce);
        persist_pending_approval(&root, &req).await.unwrap();

        // First resolution succeeds.
        let outcome = persist_resolved_approval(&root, &sample_resolution("req-4", &nonce))
            .await
            .unwrap();
        assert!(matches!(outcome, ResolveOutcome::Resolved));

        // A second resolution with the *same* nonce must fail because
        // the entry is no longer in pending.
        let outcome = persist_resolved_approval(&root, &sample_resolution("req-4", &nonce))
            .await
            .unwrap();
        assert!(matches!(outcome, ResolveOutcome::NotPending));
        let store = load_store(&approval_store_path(&root)).await;
        assert_eq!(
            store.resolved.len(),
            1,
            "replay must not append a second audit entry"
        );

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn unknown_id_returns_not_pending() {
        let root = temp_root("unknown");
        let outcome = persist_resolved_approval(&root, &sample_resolution("nope", "xx"))
            .await
            .unwrap();
        assert!(matches!(outcome, ResolveOutcome::NotPending));
        let store = load_store(&approval_store_path(&root)).await;
        assert!(
            store.resolved.is_empty(),
            "unknown ids must not poison the resolved audit log"
        );
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[test]
    fn approval_text_is_redacted_and_bounded() {
        let input = format!("token=super-secret-value {}", "x".repeat(3_000));
        let sanitized = sanitized_approval_text(&input, 128);
        assert!(!sanitized.contains("super-secret-value"));
        assert!(sanitized.chars().count() <= 129);
    }

    #[test]
    fn generate_approval_nonce_is_64_hex_chars_and_distinct() {
        let a = generate_approval_nonce();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        let b = generate_approval_nonce();
        assert_ne!(a, b);
    }
}
