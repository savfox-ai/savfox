#![allow(clippy::module_inception)]

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::super::heartbeat_config_path;
use super::super::types::{INTERNAL_ERROR, INVALID_REQUEST, RpcResult};
use super::super::utils::{opt_bool, opt_str, opt_u64};
use crate::channel::GatewayChannel;
use crate::cron_service::CronService;
use crate::log_store;
use crate::protocol::GatewayMessage;
use crate::session::{GatewaySessionManager, SessionStore};

pub(crate) async fn handle_config_schema() -> RpcResult {
    Ok(json!({"schema": crate::config::catalog::schema()}))
}

// ── Usage ───────────────────────────────────────────────────────────────────

pub(crate) async fn handle_usage_status(session_store: &Arc<SessionStore>) -> RpcResult {
    let sessions = session_store.list().await;
    let total_input: u64 = sessions.iter().map(|s| s.input_tokens).sum();
    let total_output: u64 = sessions.iter().map(|s| s.output_tokens).sum();
    let total: u64 = sessions.iter().map(|s| s.total_tokens).sum();

    // Build hourly distribution from session update times.
    let mut hourly = vec![0u64; 24];
    for s in &sessions {
        let secs = s.updated_at / 1000;
        let hour = ((secs % 86400) / 3600) as usize;
        if hour < 24 {
            hourly[hour] += 1;
        }
    }

    Ok(json!({
        "total_tokens": total,
        "prompt_tokens": total_input,
        "completion_tokens": total_output,
        "session_count": sessions.len(),
        "total_messages": null,
        "tool_calls": null,
        "errors": null,
        "cache_hits": null,
        "cache_misses": null,
        "hourly_distribution": hourly,
    }))
}

pub(crate) async fn handle_usage_cost(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let period = opt_str(params, "period", "all");
    let session_id = params.get("session_id").and_then(|v| v.as_str());

    if let Some(key) = session_id {
        // Per-session usage.
        match session_store.get(key).await {
            Some(entry) => Ok(json!({
                "period": period,
                "session_id": key,
                "input_tokens": entry.input_tokens,
                "output_tokens": entry.output_tokens,
                "total_tokens": entry.total_tokens,
            })),
            None => Ok(json!({
                "period": period,
                "session_id": key,
                "total_tokens": 0,
            })),
        }
    } else {
        // Return per-session entries with token breakdown.
        let sessions = session_store.list().await;

        // Time filtering based on period.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let cutoff_ms = match period {
            "today" => now_ms.saturating_sub(24 * 60 * 60 * 1000),
            "week" => now_ms.saturating_sub(7 * 24 * 60 * 60 * 1000),
            "month" => now_ms.saturating_sub(30 * 24 * 60 * 60 * 1000),
            _ => 0, // "all"
        };

        let entries: Vec<Value> = sessions
            .iter()
            .filter(|s| s.updated_at >= cutoff_ms || cutoff_ms == 0)
            .filter(|s| s.total_tokens > 0)
            .map(|s| {
                json!({
                    "session_id": s.session_id,
                    "model": s.model,
                    "tokens": s.total_tokens,
                    "input_tokens": s.input_tokens,
                    "output_tokens": s.output_tokens,
                    "cost": null,
                })
            })
            .collect();

        let total_tokens: u64 = entries
            .iter()
            .filter_map(|e| e.get("tokens").and_then(|v| v.as_u64()))
            .sum();
        let total_input: u64 = entries
            .iter()
            .filter_map(|e| e.get("input_tokens").and_then(|v| v.as_u64()))
            .sum();
        let total_output: u64 = entries
            .iter()
            .filter_map(|e| e.get("output_tokens").and_then(|v| v.as_u64()))
            .sum();

        Ok(json!({
            "period": period,
            "total_tokens": total_tokens,
            "prompt_tokens": total_input,
            "completion_tokens": total_output,
            "total_sessions": entries.len(),
            "entries": entries,
        }))
    }
}

// ── Logs ────────────────────────────────────────────────────────────────────

pub(crate) async fn handle_logs_tail(params: &Value) -> RpcResult {
    let lines = opt_u64(params, "lines", 50);
    let entries = log_store::list_logs(lines as usize).await;
    let value = serde_json::to_value(entries).unwrap_or(json!([]));
    Ok(json!({ "lines": lines, "entries": value }))
}

// ── System ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentHeartbeatSettings {
    enabled: bool,
    interval_ms: u64,
    coalesce_window_ms: u64,
    #[serde(default)]
    cron_job_ids: Vec<String>,
}

impl Default for AgentHeartbeatSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_ms: 30_000,
            coalesce_window_ms: 30_000,
            cron_job_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HeartbeatSettingsDocument {
    #[serde(default)]
    agents: HashMap<String, AgentHeartbeatSettings>,
}

#[derive(Debug, Clone, Default)]
struct PendingHeartbeatEvent {
    event_type: String,
    text: String,
    timestamp: String,
}

#[derive(Debug, Clone, Default)]
struct HeartbeatRuntimeState {
    last_delivered_ms: u64,
    pending: Option<PendingHeartbeatEvent>,
    flush_scheduled: bool,
}

fn heartbeat_runtime_store() -> &'static Mutex<HashMap<String, HeartbeatRuntimeState>> {
    static STORE: OnceLock<Mutex<HashMap<String, HeartbeatRuntimeState>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn heartbeat_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn heartbeat_agent_from_params(params: &Value) -> String {
    params
        .get("agent")
        .or_else(|| params.get("agent_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_owned()
}

async fn load_heartbeat_settings(channel: &GatewayChannel) -> HeartbeatSettingsDocument {
    let path = heartbeat_config_path(channel);
    let content = tokio::fs::read_to_string(&path)
        .await
        .unwrap_or_else(|_| "{}".to_owned());
    serde_json::from_str::<HeartbeatSettingsDocument>(&content).unwrap_or_default()
}

async fn save_heartbeat_settings(
    channel: &GatewayChannel,
    settings: &HeartbeatSettingsDocument,
) -> Result<(), String> {
    let path = heartbeat_config_path(channel);
    let content = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("serialize heartbeat settings failed: {e}"))?;
    tokio::fs::write(path, content)
        .await
        .map_err(|e| format!("write heartbeat settings failed: {e}"))
}

async fn heartbeat_settings_for_agent(
    channel: &GatewayChannel,
    agent_id: &str,
) -> AgentHeartbeatSettings {
    let settings = load_heartbeat_settings(channel).await;
    settings
        .agents
        .get(agent_id)
        .cloned()
        .or_else(|| settings.agents.get("*").cloned())
        .unwrap_or_default()
}

pub(crate) async fn handle_last_heartbeat(params: &Value) -> RpcResult {
    let agent_id = heartbeat_agent_from_params(params);
    let state = heartbeat_runtime_store().lock().await;
    let snapshot = state.get(&agent_id).cloned().unwrap_or_default();
    let timestamp = if snapshot.last_delivered_ms == 0 {
        chrono::Utc::now().to_rfc3339()
    } else {
        chrono::DateTime::from_timestamp_millis(snapshot.last_delivered_ms as i64)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
    };
    Ok(json!({
        "agent": agent_id,
        "timestamp": timestamp,
        "last_delivered_ms": snapshot.last_delivered_ms,
        "has_pending": snapshot.pending.is_some(),
    }))
}

pub(crate) async fn handle_set_heartbeats(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let agent_id = heartbeat_agent_from_params(params);
    let enabled = opt_bool(params, "enabled", true);
    let interval_ms = opt_u64(params, "interval_ms", 30000);
    let coalesce_window_ms = opt_u64(params, "coalesce_window_ms", 30000);

    let cron_job_ids = params
        .get("cron_job_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(str::trim))
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        });

    let mut settings = load_heartbeat_settings(channel).await;
    let entry = settings.agents.entry(agent_id.clone()).or_default();
    entry.enabled = enabled;
    entry.interval_ms = interval_ms.max(1000);
    entry.coalesce_window_ms = coalesce_window_ms.max(1000);
    if let Some(cron_job_ids) = cron_job_ids {
        entry.cron_job_ids = cron_job_ids;
    }
    let response = entry.clone();

    save_heartbeat_settings(channel, &settings)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({
        "agent": agent_id,
        "enabled": response.enabled,
        "interval_ms": response.interval_ms,
        "coalesce_window_ms": response.coalesce_window_ms,
        "cron_job_ids": response.cron_job_ids,
    }))
}

pub(crate) async fn handle_system_presence(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let status = opt_str(params, "status", "online");

    // Broadcast presence change to all connected clients.
    session_mgr
        .broadcast_to_all(
            "system.presence",
            json!({ "status": status, "timestamp": chrono::Utc::now().to_rfc3339() }),
        )
        .await;

    Ok(json!({ "status": status }))
}

pub(crate) async fn handle_system_event(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_mgr: &Arc<GatewaySessionManager>,
    cron_service: &Arc<CronService>,
) -> RpcResult {
    let event_type = opt_str(params, "type", "unknown");
    let text = opt_str(params, "text", "");
    let heartbeat = opt_bool(params, "heartbeat", false);

    if heartbeat {
        let agent_id = heartbeat_agent_from_params(params);
        let settings = heartbeat_settings_for_agent(channel, &agent_id).await;
        if !settings.enabled {
            return Ok(json!({
                "type": event_type,
                "received": true,
                "heartbeat": true,
                "agent": agent_id,
                "delivered": false,
                "reason": "heartbeat disabled",
            }));
        }

        let now_ms = heartbeat_now_ms();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let mut should_flush = false;
        let mut flush_delay_ms = settings.coalesce_window_ms;
        let mut coalesced = false;
        {
            let mut store = heartbeat_runtime_store().lock().await;
            let state = store.entry(agent_id.clone()).or_default();
            let elapsed = now_ms.saturating_sub(state.last_delivered_ms);
            if elapsed < settings.coalesce_window_ms {
                state.pending = Some(PendingHeartbeatEvent {
                    event_type: event_type.to_owned(),
                    text: text.to_owned(),
                    timestamp: timestamp.clone(),
                });
                if !state.flush_scheduled {
                    state.flush_scheduled = true;
                    should_flush = true;
                    flush_delay_ms = settings.coalesce_window_ms.saturating_sub(elapsed).max(1);
                }
                coalesced = true;
            } else {
                state.last_delivered_ms = now_ms;
                state.pending = None;
            }
        }

        if coalesced {
            if should_flush {
                schedule_heartbeat_flush(
                    agent_id.clone(),
                    flush_delay_ms,
                    Arc::clone(session_mgr),
                    Arc::clone(channel),
                    Arc::clone(cron_service),
                );
            }
            return Ok(json!({
                "type": event_type,
                "received": true,
                "heartbeat": true,
                "agent": agent_id,
                "delivered": false,
                "coalesced": true,
                "window_ms": settings.coalesce_window_ms,
            }));
        }

        broadcast_heartbeat_event(session_mgr, event_type, text, &timestamp).await;
        trigger_heartbeat_cron_jobs(&agent_id, &settings, channel, cron_service).await;

        return Ok(json!({
            "type": event_type,
            "received": true,
            "heartbeat": true,
            "agent": agent_id,
            "delivered": true,
            "coalesced": false,
            "timestamp": timestamp,
        }));
    }

    // Broadcast the system event to all connected WebSocket clients.
    session_mgr
        .broadcast_to_all(
            "system.event",
            json!({
                "type": event_type,
                "text": text,
                "heartbeat": heartbeat,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .await;

    // If text is provided, inject it into the main agent session.
    if !text.is_empty() {
        match channel.invoke_agent_text(text, "default").await {
            Ok(reply) => {
                return Ok(json!({
                    "type": event_type,
                    "received": true,
                    "response": reply,
                }));
            }
            Err(err) => {
                return Ok(json!({
                    "type": event_type,
                    "received": true,
                    "error": format!("{err}"),
                }));
            }
        }
    }

    Ok(json!({ "type": event_type, "received": true }))
}

fn schedule_heartbeat_flush(
    agent_id: String,
    delay_ms: u64,
    session_mgr: Arc<GatewaySessionManager>,
    channel: Arc<GatewayChannel>,
    cron_service: Arc<CronService>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        let pending = {
            let mut store = heartbeat_runtime_store().lock().await;
            let state = store.entry(agent_id.clone()).or_default();
            state.flush_scheduled = false;
            let pending = state.pending.take();
            if pending.is_some() {
                state.last_delivered_ms = heartbeat_now_ms();
            }
            pending
        };

        let Some(pending) = pending else {
            return;
        };

        broadcast_heartbeat_event(
            &session_mgr,
            &pending.event_type,
            &pending.text,
            &pending.timestamp,
        )
        .await;

        let settings = heartbeat_settings_for_agent(&channel, &agent_id).await;
        trigger_heartbeat_cron_jobs(&agent_id, &settings, &channel, &cron_service).await;
    });
}

async fn broadcast_heartbeat_event(
    session_mgr: &Arc<GatewaySessionManager>,
    event_type: &str,
    text: &str,
    timestamp: &str,
) {
    session_mgr
        .broadcast_to_all(
            "system.event",
            json!({
                "type": event_type,
                "text": text,
                "heartbeat": true,
                "timestamp": timestamp,
            }),
        )
        .await;
}

async fn trigger_heartbeat_cron_jobs(
    agent_id: &str,
    settings: &AgentHeartbeatSettings,
    channel: &Arc<GatewayChannel>,
    cron_service: &Arc<CronService>,
) {
    for job_id in &settings.cron_job_ids {
        if let Err(err) = cron_service.run_job(job_id, channel).await {
            tracing::warn!(
                agent = agent_id,
                cron_job_id = %job_id,
                "heartbeat cron trigger failed: {err}"
            );
        }
    }
}

// ── System instance management ─────────────────────────────────────────────

pub(crate) async fn handle_system_connections(
    current: &str,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    let mut entries = Vec::new();
    for id in session_mgr.session_ids().await {
        if let Some(session) = session_mgr.get_session(&id).await {
            let session = session.read().await;
            entries.push(
                json!({"session_id":id.to_string(),"name":session.token_info.label,
                "connected_at":session.connected_at,"is_current":id.to_string()==current}),
            );
        }
    }
    entries.sort_by_key(|entry| entry["session_id"].as_str().unwrap_or_default().to_owned());
    Ok(json!({"entries":entries}))
}

/// Disconnect a connected instance (graceful close).
pub(crate) async fn handle_system_disconnect(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    use savfox_protocol::SessionId;

    let session_id_str = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or((INVALID_REQUEST, "missing session_id".to_owned()))?;

    let session_id = SessionId::from_string(session_id_str)
        .map_err(|e| (INVALID_REQUEST, format!("invalid session_id: {e}")))?;

    let session = session_mgr
        .get_session(&session_id)
        .await
        .ok_or((INVALID_REQUEST, "connection not found".to_owned()))?;
    {
        let session = session.read().await;
        session
            .sender
            .send(GatewayMessage::Event {
                event: "system.disconnected".into(),
                payload: json!({"session_id":session_id_str}),
                seq: Some(session.next_seq()),
            })
            .await
            .map_err(|_| (INVALID_REQUEST, "connection already closed".to_owned()))?;
    }
    drop(session);
    session_mgr.remove_session(&session_id).await;

    Ok(json!({
        "disconnected": true,
        "session_id": session_id_str,
    }))
}

/// Kick a connected instance (forceful removal).
pub(crate) async fn handle_system_kick(
    params: &Value,
    session_mgr: &Arc<GatewaySessionManager>,
) -> RpcResult {
    use savfox_protocol::SessionId;

    let session_id_str = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or((INVALID_REQUEST, "missing session_id".to_owned()))?;

    let reason = opt_str(params, "reason", "kicked by operator");

    let session_id = SessionId::from_string(session_id_str)
        .map_err(|e| (INVALID_REQUEST, format!("invalid session_id: {e}")))?;

    // Notify the session before removing it.
    if let Some(session_arc) = session_mgr.get_session(&session_id).await {
        let session = session_arc.read().await;
        let msg = crate::protocol::GatewayMessage::Event {
            event: "system.kicked".to_owned(),
            payload: json!({ "reason": reason }),
            seq: Some(session.next_seq()),
        };
        let _ = session.sender.try_send(msg);
    }

    session_mgr.remove_session(&session_id).await;

    Ok(json!({
        "kicked": true,
        "session_id": session_id_str,
        "reason": reason,
    }))
}

/// Return the current execution approval policy.
pub(crate) async fn handle_approvals_policy(
    _params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    // Re-use existing exec approvals get handler logic.
    super::skill::handle_exec_approvals_get(channel, None).await
}
