use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::types::{INTERNAL_ERROR, INVALID_PARAMS, RpcResult};
use crate::channel::GatewayChannel;
use crate::home_paths::{config_backup_path, config_candidates, config_toml_path};

// ── Config ──────────────────────────────────────────────────────────────────

fn to_title_case_segment(segment: &str) -> String {
    let mut chars = segment.chars();
    if let Some(first) = chars.next() {
        let mut out = String::new();
        out.push(first.to_ascii_uppercase());
        out.push_str(chars.as_str());
        out
    } else {
        String::new()
    }
}

pub(crate) fn humanize_hyphenated_id(raw: &str) -> String {
    raw.split('-')
        .filter(|segment| !segment.is_empty())
        .map(to_title_case_segment)
        .collect::<Vec<_>>()
        .join(" ")
}

fn deep_merge_patch(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target_obj), Value::Object(patch_obj)) => {
            for (key, patch_value) in patch_obj {
                if patch_value.is_null() {
                    target_obj.remove(key);
                    continue;
                }
                if let Some(existing_value) = target_obj.get_mut(key) {
                    deep_merge_patch(existing_value, patch_value);
                } else {
                    target_obj.insert(key.clone(), patch_value.clone());
                }
            }
        }
        (target_slot, patch_value) => {
            *target_slot = patch_value.clone();
        }
    }
}

pub(crate) async fn handle_config_get(channel: &Arc<GatewayChannel>) -> RpcResult {
    let session_count = channel.websocket_manager().session_count().await;

    let mut config_value = load_config_value(channel)
        .await
        .map_err(|error| (INTERNAL_ERROR, error))?;
    let revision = config_revision(&config_value);
    let removed = crate::config::catalog::clean(&mut config_value);

    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "connected_clients": session_count,
        "endpoints": {
            "ws": "/ws",
            "health": "/health",
            "chat_completions": "/v1/chat/completions",
            "responses": "/v1/responses",
            "tools_invoke": "/tools/invoke",
        },
        "revision": revision,
        "removed_fields": removed,
        "config": config_value,
    }))
}

pub(crate) async fn load_config_intermediate(
    channel: &GatewayChannel,
) -> Result<crate::security_audit::ConfigFile, String> {
    crate::security_audit::load_config_document(&channel.config().savfox_home).await
}

pub(crate) fn primary_config_toml_path(channel: &GatewayChannel) -> PathBuf {
    config_toml_path(&channel.config().savfox_home)
}

pub(crate) async fn load_config_value_or_empty(channel: &GatewayChannel) -> Value {
    load_config_value(channel)
        .await
        .unwrap_or_else(|_| json!({}))
}

pub(crate) async fn load_config_value(channel: &GatewayChannel) -> Result<Value, String> {
    let path = primary_config_toml_path(channel);
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => return Err(format!("Failed to read configuration: {error}")),
    };
    let value: toml::Value =
        toml::from_str(&text).map_err(|error| format!("Invalid configuration TOML: {error}"))?;
    serde_json::to_value(value).map_err(|error| error.to_string())
}

pub(crate) async fn write_config_toml(
    channel: &GatewayChannel,
    config: &Value,
) -> Result<(), String> {
    let path = primary_config_toml_path(channel);
    let toml_value = savfox_utils::json_to_toml::json_to_toml(config.clone());
    let content = toml::to_string_pretty(&toml_value)
        .map_err(|e| format!("TOML serialization failed: {e}"))?;
    savfox_utils::fs::write_atomically_async(&path, content.into_bytes(), Some(0o600))
        .await
        .map_err(|e| format!("failed to write config: {e}"))
}

pub(crate) async fn handle_config_export(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let mut doc = load_config_intermediate(channel)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    let requested = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or(doc.format.as_str());
    // Default to redacting secrets — exports are meant for sharing/backup, so
    // callers must explicitly opt into including cleartext API keys/tokens.
    let redacted = params
        .get("redacted")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let valid = ["json", "yaml", "toml"];
    if !valid.contains(&requested) {
        return Err((
            INVALID_PARAMS,
            format!(
                "unsupported format: {requested}. Must be one of: {}",
                valid.join(", ")
            ),
        ));
    }

    crate::config::catalog::clean(&mut doc.value);
    if redacted {
        crate::redaction::redact_json_in_place(&mut doc.value);
    }

    let content = match requested {
        "json" => serde_json::to_string_pretty(&doc.value)
            .map_err(|e| (INTERNAL_ERROR, format!("JSON serialization failed: {e}")))?,
        "yaml" => serde_saphyr::to_string(&doc.value)
            .map_err(|e| (INTERNAL_ERROR, format!("YAML serialization failed: {e}")))?,
        "toml" => json_value_to_toml_string(&doc.value).map_err(|e| (INTERNAL_ERROR, e))?,
        _ => unreachable!("validated above"),
    };

    Ok(json!({
        "status": "ok",
        "source_format": doc.format,
        "source_path": doc.path,
        "format": requested,
        "redacted": redacted,
        "content": content,
    }))
}

/// Convert a serde_json::Value to a TOML string.
fn json_value_to_toml_string(value: &Value) -> Result<String, String> {
    let toml_value = savfox_utils::json_to_toml::json_to_toml(value.clone());
    toml::to_string(&toml_value).map_err(|e| format!("TOML serialization failed: {e}"))
}

fn preserve_toml_leading_comments_for_yaml(source_toml: &str, yaml: &str) -> String {
    let mut prefix_lines = Vec::new();

    for line in source_toml.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            prefix_lines.push(line.to_owned());
            continue;
        }
        if trimmed.is_empty() {
            if !prefix_lines.is_empty() {
                prefix_lines.push(String::new());
            }
            continue;
        }
        break;
    }

    if prefix_lines.is_empty() {
        return yaml.to_owned();
    }

    let mut result = prefix_lines.join("\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(yaml);
    result
}

fn config_revision(config: &Value) -> String {
    use sha2::Digest;
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(object) => {
                let mut keys = object.keys().collect::<Vec<_>>();
                keys.sort();
                Value::Object(
                    keys.into_iter()
                        .map(|key| (key.clone(), canonical(&object[key])))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
            value => value.clone(),
        }
    }
    hex::encode(sha2::Sha256::digest(
        canonical(config).to_string().as_bytes(),
    ))
}

/// Serialize writes across HTTP and WebSocket clients. Validate and back up the
/// previous document before the single atomic replacement.
pub(crate) async fn save_config(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    patch: bool,
) -> RpcResult {
    static WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = WRITES.lock().await;
    let previous = load_config_value(channel)
        .await
        .map_err(|error| (INTERNAL_ERROR, error))?;
    if let Some(expected) = params.get("revision").and_then(Value::as_str)
        && expected != config_revision(&previous)
    {
        return Err((
            INVALID_PARAMS,
            "Configuration changed on the server. Reload before saving.".into(),
        ));
    }
    let mut document = if patch {
        let mut document = previous.clone();
        crate::config::catalog::clean(&mut document);
        let changes = params
            .get("patch")
            .ok_or((INVALID_PARAMS, "missing patch".into()))?;
        if !changes.is_object() {
            return Err((INVALID_PARAMS, "Patch must be an object".into()));
        }
        deep_merge_patch(&mut document, changes);
        document
    } else {
        params
            .get("config")
            .cloned()
            .ok_or((INVALID_PARAMS, "missing config".into()))?
    };
    // Existing obsolete fields are removed, but new unsupported input is rejected.
    crate::config::catalog::validate(&document).map_err(|e| (INVALID_PARAMS, e))?;
    // TOML has no null values; optional JSON nulls mean removal.
    remove_nulls(&mut document);
    if document == previous {
        return Ok(
            json!({"status":"unchanged", "restart_required":false, "revision":config_revision(&document), "config":document}),
        );
    }
    let path = primary_config_toml_path(channel);
    if path.exists() {
        tokio::fs::copy(&path, config_backup_path(&channel.config().savfox_home))
            .await
            .map_err(|e| {
                (
                    INTERNAL_ERROR,
                    format!("failed to back up configuration: {e}"),
                )
            })?;
    }
    write_config_toml(channel, &document)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;
    Ok(
        json!({"status":"saved", "restart_required":true, "revision":config_revision(&document), "config":document}),
    )
}

fn remove_nulls(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|_, v| !v.is_null());
            for v in object.values_mut() {
                remove_nulls(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                remove_nulls(v);
            }
        }
        _ => {}
    }
}

pub(crate) async fn handle_config_set(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    save_config(params, channel, false).await
}

pub(crate) async fn handle_config_apply(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    save_config(params, channel, false).await
}

pub(crate) async fn handle_config_patch(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    save_config(params, channel, true).await
}

// ── Hooks Event Bus (#31) ───────────────────────────────────────────────────

fn hooks_config_path(channel: &GatewayChannel) -> std::path::PathBuf {
    crate::home_paths::hooks_config_path(&channel.config().savfox_home)
}

pub(crate) async fn handle_hooks_list(channel: &GatewayChannel) -> RpcResult {
    let path = hooks_config_path(channel);
    let config = crate::json_store::load_json_value(&path).await;

    let hooks_arr = config.get("hooks").cloned().unwrap_or(json!([]));
    let count = hooks_arr.as_array().map(|a| a.len()).unwrap_or(0);

    Ok(json!({
        "hooks": hooks_arr,
        "count": count,
        "builtin_events": [
            "session:start", "session:end", "session:compact",
            "message:received", "message:sent", "message:error",
            "agent:start", "agent:complete", "agent:error",
            "cron:started", "cron:completed", "cron:failed",
            "memory:created", "memory:updated", "memory:deleted",
            "config:changed", "config:reloaded",
        ],
    }))
}

pub(crate) async fn handle_hooks_enable(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let hook_id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if hook_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_owned()));
    }

    let path = hooks_config_path(channel);
    let mut config = crate::json_store::load_json_value(&path).await;

    if let Some(hooks) = config.get_mut("hooks").and_then(|v| v.as_array_mut()) {
        for hook in hooks.iter_mut() {
            if hook.get("id").and_then(|v| v.as_str()) == Some(hook_id) {
                hook["enabled"] = json!(true);
            }
        }
    }

    let json_str = serde_json::to_string_pretty(&config)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(&path, json_str)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "id": hook_id, "status": "enabled" }))
}

pub(crate) async fn handle_hooks_disable(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let hook_id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if hook_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'id' parameter".to_owned()));
    }

    let path = hooks_config_path(channel);
    let mut config = crate::json_store::load_json_value(&path).await;

    if let Some(hooks) = config.get_mut("hooks").and_then(|v| v.as_array_mut()) {
        for hook in hooks.iter_mut() {
            if hook.get("id").and_then(|v| v.as_str()) == Some(hook_id) {
                hook["enabled"] = json!(false);
            }
        }
    }

    let json_str = serde_json::to_string_pretty(&config)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(&path, json_str)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;

    Ok(json!({ "id": hook_id, "status": "disabled" }))
}

// ── Streaming Config (#36) ──────────────────────────────────────────────────

fn streaming_config_path(channel: &GatewayChannel) -> std::path::PathBuf {
    crate::home_paths::streaming_config_path(&channel.config().savfox_home)
}

pub(crate) async fn handle_streaming_config_get(channel: &GatewayChannel) -> RpcResult {
    let path = streaming_config_path(channel);
    let config = crate::json_store::load_json_value(&path).await;
    Ok(json!({
        "config": config,
        "modes": ["token", "sentence", "paragraph", "complete"],
    }))
}

pub(crate) async fn handle_streaming_config_set(
    params: &Value,
    channel: &GatewayChannel,
) -> RpcResult {
    let config = params.get("config").cloned().unwrap_or(json!({}));

    // Validate stream_mode if present
    if let Some(mode) = config.get("stream_mode").and_then(|v| v.as_str()) {
        let valid = ["token", "sentence", "paragraph", "complete"];
        if !valid.contains(&mode) {
            return Err((
                INVALID_PARAMS,
                format!(
                    "invalid stream_mode: {mode}. Must be one of: {}",
                    valid.join(", ")
                ),
            ));
        }
    }

    let path = streaming_config_path(channel);
    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| (INTERNAL_ERROR, format!("serialize error: {e}")))?;
    tokio::fs::write(&path, json)
        .await
        .map_err(|e| (INTERNAL_ERROR, format!("write error: {e}")))?;
    Ok(json!({ "status": "ok" }))
}

// ── YAML Config Support (#59) ───────────────────────────────────────────────

/// Detect which config format is currently in use (.json, .yaml, .toml).
pub(crate) async fn handle_config_format(channel: &GatewayChannel) -> RpcResult {
    let home = &channel.config().savfox_home;
    let candidates = config_candidates(home);

    let mut detected = "unknown";
    let mut path_str = String::new();
    for (fmt, path) in &candidates {
        if path.exists() {
            detected = fmt;
            path_str = path.to_string_lossy().to_string();
            break;
        }
    }

    let supported = ["json", "yaml", "toml"];
    Ok(json!({
        "format": detected,
        "path": path_str,
        "supported_formats": supported,
    }))
}

/// Convert config content between formats (json, yaml, toml).
pub(crate) async fn handle_config_convert(params: &Value, channel: &GatewayChannel) -> RpcResult {
    let from_format = params
        .get("from_format")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let to_format = params
        .get("to_format")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if from_format.is_empty() || to_format.is_empty() {
        return Err((
            INVALID_PARAMS,
            "missing 'from_format' and/or 'to_format' parameter".to_owned(),
        ));
    }

    let valid = ["json", "yaml", "toml"];
    if !valid.contains(&from_format) {
        return Err((
            INVALID_PARAMS,
            format!(
                "unsupported from_format: {from_format}. Must be one of: {}",
                valid.join(", ")
            ),
        ));
    }
    if !valid.contains(&to_format) {
        return Err((
            INVALID_PARAMS,
            format!(
                "unsupported to_format: {to_format}. Must be one of: {}",
                valid.join(", ")
            ),
        ));
    }

    // Determine source: explicit `content` param or read from disk.
    let source_content = if let Some(content) = params.get("content").and_then(|v| v.as_str()) {
        content.to_owned()
    } else {
        let home = &channel.config().savfox_home;
        let ext = match from_format {
            "yaml" => "yaml",
            "toml" => "toml",
            _ => "json",
        };
        let path = home.join(format!("config.{ext}"));
        tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("failed to read config file: {e}")))?
    };

    // Parse source into a serde_json::Value intermediate.
    let intermediate: Value = match from_format {
        "json" => serde_json::from_str(&source_content)
            .map_err(|e| (INVALID_PARAMS, format!("invalid JSON input: {e}")))?,
        "yaml" => serde_saphyr::from_str(&source_content)
            .map_err(|e| (INVALID_PARAMS, format!("invalid YAML input: {e}")))?,
        "toml" => {
            let toml_val: toml::Value = toml::from_str(&source_content)
                .map_err(|e| (INVALID_PARAMS, format!("invalid TOML input: {e}")))?;
            serde_json::to_value(&toml_val)
                .map_err(|e| (INTERNAL_ERROR, format!("TOML->JSON conversion failed: {e}")))?
        }
        _ => {
            return Err((
                INVALID_PARAMS,
                format!("unsupported from_format: {from_format}"),
            ));
        }
    };

    // Serialize to target format.
    let output = match to_format {
        "json" => serde_json::to_string_pretty(&intermediate)
            .map_err(|e| (INTERNAL_ERROR, format!("JSON serialization failed: {e}")))?,
        "yaml" => {
            let yaml = serde_saphyr::to_string(&intermediate)
                .map_err(|e| (INTERNAL_ERROR, format!("YAML serialization failed: {e}")))?;
            if from_format == "toml" {
                preserve_toml_leading_comments_for_yaml(&source_content, &yaml)
            } else {
                yaml
            }
        }
        "toml" => json_value_to_toml_string(&intermediate).map_err(|e| (INTERNAL_ERROR, e))?,
        _ => {
            return Err((
                INVALID_PARAMS,
                format!("unsupported to_format: {to_format}"),
            ));
        }
    };

    Ok(json!({
        "status": "ok",
        "from_format": from_format,
        "to_format": to_format,
        "content": output,
    }))
}

#[cfg(test)]
mod save_tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::channel::GatewayBridgeArgs;
    use crate::session::{GatewaySessionManager, SessionStore};

    #[tokio::test]
    async fn save_validates_revision_replaces_deletions_and_backs_up_previous_document() {
        let home = tempfile::tempdir().unwrap();
        for root in [home.path().to_path_buf(), home.path().join("skills")] {
            std::fs::create_dir_all(root.join("registry/github.com/savhub-ai/registry")).unwrap();
        }
        let config = savfox_core::config::ConfigBuilder::default()
            .savfox_home(home.path().to_owned())
            .build()
            .await
            .unwrap();
        let channel = Arc::new(GatewayChannel::new(GatewayBridgeArgs {
            config: Arc::new(config),
            session_store: Arc::new(SessionStore::from_home(home.path())),
            cli_overrides: vec![],
            cloud_requirements: Default::default(),
            websocket_manager: GatewaySessionManager::new(),
            channel_registry: crate::channels::create_channel_registry(),
            channel_recovery_registry: crate::channels::recovery::create_channel_recovery_registry(
            ),
            channel_recovery_supervisors:
                crate::channels::recovery::create_channel_recovery_supervisors(),
        }));
        let original = "model_context_window = 32000\n[auth.cooldowns]\nbilling_max_hours = 1\n[gateway]\nport = 18881\n";
        std::fs::write(home.path().join("config.toml"), original).unwrap();
        let loaded = handle_config_get(&channel).await.unwrap();
        assert_eq!(loaded["removed_fields"], json!(["auth"]));
        let revision = loaded["revision"].clone();
        let document = json!({"gateway":{"port":18882}});
        let saved = save_config(
            &json!({"config":document,"revision":revision}),
            &channel,
            false,
        )
        .await
        .unwrap();
        assert_eq!(saved["restart_required"], true);
        assert_eq!(
            std::fs::read_to_string(home.path().join("config.toml.bak")).unwrap(),
            original
        );
        assert_eq!(load_config_value_or_empty(&channel).await, document);
        assert!(
            save_config(
                &json!({"config":{"gateway":{"port":18883}},"revision":revision}),
                &channel,
                false
            )
            .await
            .is_err()
        );
        assert!(
            save_config(&json!({"config":{"auth":{}}}), &channel, false)
                .await
                .is_err()
        );
        assert_eq!(load_config_value_or_empty(&channel).await, document);
        assert_eq!(
            std::fs::read_to_string(home.path().join("config.toml.bak")).unwrap(),
            original
        );
        let unchanged = save_config(
            &json!({"config":document,"revision":saved["revision"]}),
            &channel,
            false,
        )
        .await
        .unwrap();
        assert_eq!(unchanged["status"], "unchanged");
        assert_eq!(unchanged["restart_required"], false);
    }
}
