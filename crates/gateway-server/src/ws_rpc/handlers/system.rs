#![allow(unused_imports)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::types::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, PLUGIN_ROUTE_RATE_LIMIT_PER_MINUTE, RpcResult,
};
use super::config::load_config_intermediate;
use crate::channel::GatewayChannel;
use crate::home_paths::log_rotation_config_path;
use crate::session::{GatewaySessionManager, SessionStore};
use crate::{log_store, pairing_store, plugin};

// ── Core ────────────────────────────────────────────────────────────────────

/// WS-RPC protocol version. Increment when breaking changes are made to the
/// protocol (new required fields, removed methods, changed semantics).
const PROTOCOL_VERSION: u32 = 1;

pub(crate) async fn handle_connect(params: &Value) -> RpcResult {
    let client_version = params
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    Ok(json!({
        "status": "connected",
        "protocol_version": PROTOCOL_VERSION,
        "server_version": env!("CARGO_PKG_VERSION"),
        "client_version": client_version,
    }))
}

pub(crate) async fn handle_health() -> RpcResult {
    Ok(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

pub(crate) async fn handle_status(
    session_mgr: &Arc<GatewaySessionManager>,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let count = session_mgr.session_count().await;
    let ids = session_mgr.session_ids().await;
    let audit_summary = crate::security_audit::run_audit(&channel.config().savfox_home)
        .await
        .summary;
    let plugins = plugin::discover_snapshot(&channel.config().savfox_home)
        .await
        .unwrap_or_default();
    let plugin_routes = plugin::describe_http_routes(&plugins, PLUGIN_ROUTE_RATE_LIMIT_PER_MINUTE);
    Ok(json!({
        "connected_clients": count,
        "session_ids": ids,
        "security_audit": audit_summary,
        "plugin_routes": plugin_routes,
    }))
}

// ── QR Code Pairing (#62) ───────────────────────────────────────────────────

/// Generate a QR-code pairing URL with a short-lived token.
pub(crate) async fn handle_device_pair_qr(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let host = params
        .get("host")
        .and_then(|v| v.as_str())
        .unwrap_or("localhost");
    let port = params
        .get("port")
        .and_then(|v| v.as_u64())
        .unwrap_or(u64::from(crate::config::DEFAULT_PORT));

    // Generate a pairing token (UUID v7).
    let token = uuid::Uuid::now_v7().to_string();

    // Expiry: default 5 minutes from now.
    let ttl_secs = params
        .get("ttl_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(300);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let expires_at = now + ttl_secs;

    let url = format!("savfox://{host}:{port}/pair?token={token}");

    // Persist the pairing request so `device.pair.list` can see it.
    let device_label = format!("qr-{}", &token[..8]);
    let _ = pairing_store::create_request_for_home(
        &channel.config().savfox_home,
        &token,
        Some(device_label.as_str()),
        Some("qr-pairing"),
    )
    .await;

    Ok(json!({
        "url": url,
        "token": token,
        "expires_at": expires_at,
        "ttl_secs": ttl_secs,
    }))
}

// ── Usage Export (#64) ──────────────────────────────────────────────────────

/// Export session usage data in CSV or JSON format.
pub(crate) async fn handle_usage_export(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");
    let date_from = params.get("date_from").and_then(|v| v.as_u64());
    let date_to = params.get("date_to").and_then(|v| v.as_u64());

    if format != "csv" && format != "json" {
        return Err((
            INVALID_PARAMS,
            format!("unsupported format: {format}. Must be 'csv' or 'json'"),
        ));
    }

    let sessions = session_store.list().await;

    // Filter by date range (epoch ms).
    let filtered: Vec<_> = sessions
        .iter()
        .filter(|s| {
            if let Some(from) = date_from {
                if s.created_at < from {
                    return false;
                }
            }
            if let Some(to) = date_to {
                if s.created_at > to {
                    return false;
                }
            }
            true
        })
        .collect();

    let count = filtered.len();

    let data: Value = match format {
        "csv" => {
            let mut lines = Vec::with_capacity(count + 1);
            lines.push(
                "session_id,session_id,model,input_tokens,output_tokens,total_tokens,created_at,updated_at"
                    .to_string(),
            );
            for s in &filtered {
                lines.push(format!(
                    "{},{},{},{},{},{},{},{}",
                    s.session_id,
                    s.session_id,
                    s.model.as_deref().unwrap_or(""),
                    s.input_tokens,
                    s.output_tokens,
                    s.total_tokens,
                    s.created_at,
                    s.updated_at,
                ));
            }
            json!(lines.join("\n"))
        }
        _ => {
            let entries: Vec<Value> = filtered
                .iter()
                .map(|s| {
                    json!({
                        "session_id": s.session_id,
                        "model": s.model,
                        "input_tokens": s.input_tokens,
                        "output_tokens": s.output_tokens,
                        "total_tokens": s.total_tokens,
                        "created_at": s.created_at,
                        "updated_at": s.updated_at,
                    })
                })
                .collect();
            json!(entries)
        }
    };

    Ok(json!({
        "data": data,
        "format": format,
        "count": count,
    }))
}

// ── Log Rotation (#65) ─────────────────────────────────────────────────────

/// Path to log rotation config file.
fn log_config_path(channel: &GatewayChannel) -> std::path::PathBuf {
    log_rotation_config_path(&channel.config().savfox_home)
}

/// Trigger log rotation: clear in-memory log buffer and archive to a timestamped file.
pub(crate) async fn handle_logs_rotate(channel: &GatewayChannel) -> RpcResult {
    // Drain current logs from in-memory store.
    let entries = log_store::list_logs(usize::MAX).await;
    let count = entries.len();

    if count > 0 {
        // Archive to a timestamped JSONL file.
        let logs_dir = channel.config().savfox_home.join("logs");
        if !logs_dir.exists() {
            tokio::fs::create_dir_all(&logs_dir)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("mkdir error: {e}")))?;
        }

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let archive_path = logs_dir.join(format!("gateway-{ts}.jsonl"));

        let mut buf = String::new();
        for entry in &entries {
            if let Ok(line) = serde_json::to_string(entry) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }

        tokio::fs::write(&archive_path, &buf)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

        // Prune old archives based on max_files setting.
        let _ = prune_log_archives(channel).await;
    }

    log_store::append_log("info", "logs.rotate", format!("rotated {count} entries")).await;

    Ok(json!({
        "status": "ok",
        "rotated_entries": count,
    }))
}

/// Prune old log archives beyond the configured max_files limit.
async fn prune_log_archives(channel: &GatewayChannel) -> Result<usize, String> {
    let config = read_log_rotation_config(channel).await;
    let max_files = config
        .get("max_files")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;

    let logs_dir = channel.config().savfox_home.join("logs");
    if !logs_dir.exists() {
        return Ok(0);
    }

    let mut archives: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut entries = tokio::fs::read_dir(&logs_dir)
        .await
        .map_err(|e| format!("readdir error: {e}"))?;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("gateway-") && name.ends_with(".jsonl") {
            archives.push((name, entry.path()));
        }
    }

    // Sort oldest first.
    archives.sort_by(|a, b| a.0.cmp(&b.0));

    let mut removed = 0;
    while archives.len() > max_files {
        if let Some((_, path)) = archives.first() {
            let _ = tokio::fs::remove_file(path).await;
            archives.remove(0);
            removed += 1;
        } else {
            break;
        }
    }

    Ok(removed)
}

/// Read log rotation config from disk.
async fn read_log_rotation_config(channel: &GatewayChannel) -> Value {
    let path = log_config_path(channel);
    crate::json_store::load_json_value(&path).await
}

/// Export recent log entries with optional filtering.
pub(crate) async fn handle_logs_export(params: &Value) -> RpcResult {
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
    let level_filter = params.get("level").and_then(|v| v.as_str());
    let source_filter = params.get("source").and_then(|v| v.as_str());

    let all_entries = log_store::list_logs(limit).await;

    let filtered: Vec<_> = all_entries
        .into_iter()
        .filter(|e| {
            if let Some(level) = level_filter {
                if e.level != level {
                    return false;
                }
            }
            if let Some(source) = source_filter {
                if !e.source.contains(source) {
                    return false;
                }
            }
            true
        })
        .collect();

    let count = filtered.len();
    let value = serde_json::to_value(&filtered).unwrap_or(json!([]));

    Ok(json!({
        "entries": value,
        "count": count,
    }))
}

/// Get or set log rotation configuration (max_file_size_mb, max_files).
pub(crate) async fn handle_logs_config(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("get");

    let path = log_config_path(channel);

    match action {
        "set" => {
            let mut config = read_log_rotation_config(channel).await;

            if let Some(max_size) = params.get("max_file_size_mb") {
                config["max_file_size_mb"] = max_size.clone();
            }
            if let Some(max_files) = params.get("max_files") {
                config["max_files"] = max_files.clone();
            }

            let json_str = serde_json::to_string_pretty(&config)
                .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
            tokio::fs::write(&path, json_str)
                .await
                .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

            Ok(json!({
                "status": "ok",
                "config": config,
            }))
        }
        _ => {
            // "get" (default)
            let config = read_log_rotation_config(channel).await;
            let defaults = json!({
                "max_file_size_mb": config.get("max_file_size_mb").cloned().unwrap_or(json!(50)),
                "max_files": config.get("max_files").cloned().unwrap_or(json!(10)),
            });
            Ok(json!({ "config": defaults }))
        }
    }
}

// ── Prompt Injection Detection (#66) ────────────────────────────────────────

/// Injection pattern definition.
struct InjectionPattern {
    name: &'static str,
    patterns: &'static [&'static str],
    weight: f64,
}

/// Well-known prompt injection / jailbreak patterns.
const INJECTION_PATTERNS: &[InjectionPattern] = &[
    InjectionPattern {
        name: "system_prompt_override",
        patterns: &[
            "ignore previous instructions",
            "ignore all previous",
            "disregard previous instructions",
            "forget your instructions",
            "override your system prompt",
            "new system prompt",
            "from now on you are",
            "you are now",
        ],
        weight: 0.9,
    },
    InjectionPattern {
        name: "jailbreak_phrase",
        patterns: &[
            "do anything now",
            "dan mode",
            "jailbreak",
            "developer mode",
            "bypass your restrictions",
            "ignore your safety",
            "pretend you have no restrictions",
            "act as an unrestricted",
        ],
        weight: 0.95,
    },
    InjectionPattern {
        name: "role_confusion",
        patterns: &[
            "you are not an ai",
            "you are a human",
            "pretend to be",
            "roleplay as",
            "act as if you are",
            "simulate being",
            "behave as",
        ],
        weight: 0.6,
    },
    InjectionPattern {
        name: "instruction_override",
        patterns: &[
            "ignore the above",
            "disregard the above",
            "do not follow",
            "stop following",
            "new instructions:",
            "updated instructions:",
            "real instructions:",
            "actual instructions:",
            "true instructions:",
        ],
        weight: 0.85,
    },
];

/// Analyze text for common prompt injection / jailbreak patterns.
pub(crate) async fn handle_security_analyze(params: &Value) -> RpcResult {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");

    if text.is_empty() {
        return Err((INVALID_PARAMS, "missing 'text' parameter".to_string()));
    }

    let lower = text.to_lowercase();
    let mut found: Vec<Value> = Vec::new();
    let mut max_weight: f64 = 0.0;

    for category in INJECTION_PATTERNS {
        for &pattern in category.patterns {
            if lower.contains(pattern) {
                found.push(json!({
                    "category": category.name,
                    "pattern": pattern,
                    "weight": category.weight,
                }));
                if category.weight > max_weight {
                    max_weight = category.weight;
                }
            }
        }
    }

    // Score: 0.0 = safe, 1.0 = definitely injection.
    // Take max weight of matched patterns, scale by number of unique categories hit.
    let categories_hit: std::collections::HashSet<&str> = found
        .iter()
        .filter_map(|v| v.get("category").and_then(|c| c.as_str()))
        .collect();

    let score = if found.is_empty() {
        0.0
    } else {
        // Base score from heaviest pattern, boosted by multi-category matches.
        let cat_boost = ((categories_hit.len() as f64 - 1.0) * 0.05).min(0.15);
        (max_weight + cat_boost).min(1.0)
    };

    let safe = score < 0.5;

    Ok(json!({
        "safe": safe,
        "score": score,
        "patterns_found": found,
        "categories_hit": categories_hit.len(),
        "text_length": text.len(),
    }))
}

pub(crate) async fn handle_security_audit(
    _params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let report = crate::security_audit::run_audit(&channel.config().savfox_home).await;
    serde_json::to_value(report).map_err(|e| {
        (
            INTERNAL_ERROR,
            format!("security audit serialization failed: {e}"),
        )
    })
}

fn rotated_secret(prefix: &str) -> String {
    format!("{prefix}{}", uuid::Uuid::now_v7().simple())
}

pub(crate) async fn handle_security_rotate(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let rotate_gateway_token = params
        .get("gateway_token")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let rotate_webhook_secrets = params
        .get("webhook_secrets")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if !rotate_gateway_token && !rotate_webhook_secrets {
        return Err((
            INVALID_PARAMS,
            "nothing to rotate: enable gateway_token and/or webhook_secrets".to_string(),
        ));
    }

    let mut result = json!({
        "status": "ok",
        "rotated": {
            "gateway_token": false,
            "webhook_secrets": 0,
        },
        "restart_required": false,
        "suggestions": [],
    });

    if rotate_gateway_token {
        let mut doc = load_config_intermediate(channel)
            .await
            .map_err(|e| (INTERNAL_ERROR, e))?;

        if !doc.value.is_object() {
            doc.value = Value::Object(serde_json::Map::new());
        }
        let root = doc
            .value
            .as_object_mut()
            .ok_or((INTERNAL_ERROR, "config root is not an object".to_string()))?;

        let gateway = root
            .entry("gateway")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if !gateway.is_object() {
            *gateway = Value::Object(serde_json::Map::new());
        }

        let new_token = crate::auth::GatewayAuth::generate_token();
        let gateway_obj = gateway.as_object_mut().ok_or((
            INTERNAL_ERROR,
            "gateway config is not an object".to_string(),
        ))?;
        gateway_obj.insert("token".to_string(), Value::String(new_token.clone()));

        let content = crate::security_audit::serialize_config_value(&doc.value, &doc.format)
            .map_err(|e| (INTERNAL_ERROR, e))?;
        crate::json_store::ensure_parent_dir(&doc.path)
            .await
            .map_err(|e| (INTERNAL_ERROR, e))?;
        tokio::fs::write(&doc.path, content)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("failed to write config: {e}")))?;

        let hint = format!(
            "...{}",
            &new_token[new_token.len().saturating_sub(4)..new_token.len()]
        );
        result["rotated"]["gateway_token"] = json!(true);
        result["gateway_token_hint"] = json!(hint);
        result["gateway_config_path"] = json!(doc.path);
        result["restart_required"] = json!(true);
    }

    if rotate_webhook_secrets {
        let store = crate::webhooks::WebhookStore::new(&channel.config().savfox_home);
        store.load().await;
        let hooks = store.list().await;

        let mut rotated = 0_u32;
        let mut failures = Vec::new();
        for hook in hooks {
            if !hook.enabled {
                continue;
            }
            let new_secret = rotated_secret("whsec_");
            let update = json!({ "secret": new_secret });
            match store.update(&hook.id, update).await {
                Ok(_) => rotated = rotated.saturating_add(1),
                Err(err) => failures.push(format!("{}: {err}", hook.id)),
            }
        }
        result["rotated"]["webhook_secrets"] = json!(rotated);
        if !failures.is_empty() {
            result["failures"] = json!(failures);
        }
    }

    let mut suggestions =
        vec!["Run `savfox security audit` to verify current posture.".to_string()];
    if rotate_gateway_token {
        suggestions.push("Restart gateway to apply the rotated bearer token.".to_string());
    }
    if rotate_webhook_secrets {
        suggestions.push(
            "Update webhook senders with the new signing secrets before sending events."
                .to_string(),
        );
    }
    result["suggestions"] = json!(suggestions);

    Ok(result)
}
