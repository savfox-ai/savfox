use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::types::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, RpcResult,
};
use super::browser::handle_config_snapshot;
use super::model::model_test_default_base_url;
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

fn non_empty_trimmed(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn normalized_provider_object(provider_id: &str, base_url: Option<&str>) -> Value {
    let resolved_base_url = base_url
        .and_then(non_empty_trimmed)
        .or_else(|| model_test_default_base_url(provider_id));

    json!({
        "id": provider_id,
        "name": humanize_hyphenated_id(provider_id),
        "base_url": resolved_base_url,
    })
}

fn normalized_model_object(
    provider_id: &str,
    model_slug: &str,
    provider_base_url: Option<&str>,
) -> Value {
    json!({
        "id": format!("{provider_id}/{model_slug}"),
        "code": model_slug,
        "name": humanize_hyphenated_id(model_slug),
        "provider": normalized_provider_object(provider_id, provider_base_url),
    })
}

fn extract_provider_id(provider_value: &Value) -> Option<String> {
    match provider_value {
        Value::String(provider_id) => non_empty_trimmed(provider_id),
        Value::Object(provider) => provider
            .get("id")
            .and_then(Value::as_str)
            .and_then(non_empty_trimmed),
        _ => None,
    }
}

fn extract_provider_base_url(provider_value: &Value) -> Option<String> {
    match provider_value {
        Value::Object(provider) => provider
            .get("base_url")
            .and_then(Value::as_str)
            .and_then(non_empty_trimmed),
        _ => None,
    }
}

fn normalize_provider_value(provider_value: &mut Value) {
    if let Some(provider_id) = extract_provider_id(provider_value) {
        let provider_base_url = extract_provider_base_url(provider_value);
        *provider_value = normalized_provider_object(&provider_id, provider_base_url.as_deref());
    }
}

fn normalize_model_value(model_value: &mut Value) {
    match model_value {
        Value::String(model_id) => {
            if let Some((provider_id, model_slug)) =
                savfox_core::parse_provider_prefixed_model(model_id.as_str())
            {
                *model_value = normalized_model_object(provider_id, model_slug, None);
            }
        }
        Value::Object(model) => {
            let id = model
                .get("id")
                .and_then(Value::as_str)
                .and_then(non_empty_trimmed);
            let parsed_from_id = id
                .as_deref()
                .and_then(savfox_core::parse_provider_prefixed_model)
                .map(|(provider_id, model_slug)| (provider_id.to_owned(), model_slug.to_owned()));
            let provider_base_url = model.get("provider").and_then(extract_provider_base_url);

            let provider_id = model
                .get("provider")
                .and_then(extract_provider_id)
                .or_else(|| {
                    parsed_from_id
                        .as_ref()
                        .map(|(provider, _)| provider.clone())
                });
            let model_slug = model
                .get("code")
                .and_then(Value::as_str)
                .and_then(non_empty_trimmed)
                .or_else(|| {
                    model
                        .get("model_slug")
                        .and_then(Value::as_str)
                        .and_then(non_empty_trimmed)
                })
                .or_else(|| parsed_from_id.as_ref().map(|(_, code)| code.clone()));

            if let (Some(provider_id), Some(model_slug)) = (provider_id, model_slug) {
                *model_value = normalized_model_object(
                    &provider_id,
                    &model_slug,
                    provider_base_url.as_deref(),
                );
                return;
            }

            if let Some(provider_value) = model.get_mut("provider") {
                normalize_provider_value(provider_value);
            }
        }
        _ => {}
    }
}

pub(crate) fn normalize_config_model_fields(config: &mut Value) {
    if let Some(model_value) = config.get_mut("model") {
        normalize_model_value(model_value);
    }

    let Some(profiles) = config.get_mut("profiles").and_then(Value::as_object_mut) else {
        return;
    };

    for profile in profiles.values_mut() {
        if let Some(profile_map) = profile.as_object_mut()
            && let Some(model_value) = profile_map.get_mut("model")
        {
            normalize_model_value(model_value);
        }
    }
}

fn normalize_model_reasoning_key(config: &mut Value) {
    let Some(model) = config.get_mut("model").and_then(Value::as_object_mut) else {
        return;
    };

    if model.get("reasoning_effort").is_none()
        && let Some(reasoning_level) = model.get("reasoning_level").cloned()
    {
        model.insert("reasoning_effort".to_owned(), reasoning_level);
    }
    model.remove("reasoning_level");
}

enum DetachedBridgeConfig {
    Upsert(Value),
    Delete,
}

fn take_detached_matrix_channel_config(config: &mut Value) -> Option<DetachedBridgeConfig> {
    let root = config.as_object_mut()?;
    let gateway = root.get_mut("gateway")?.as_object_mut()?;

    // Try both "channels" and the legacy "bridges" alias
    let container_key = if gateway.contains_key("channels") {
        "channels"
    } else if gateway.contains_key("bridges") {
        "bridges"
    } else {
        return None;
    };

    let (matrix_value, remove_container) = {
        let container = gateway.get_mut(container_key)?.as_object_mut()?;
        let matrix = container.remove("matrix")?;
        (matrix, container.is_empty())
    };
    if remove_container {
        gateway.remove(container_key);
    }
    let remove_gateway = gateway.is_empty();
    if remove_gateway {
        root.remove("gateway");
    }

    if matrix_value.is_null() {
        Some(DetachedBridgeConfig::Delete)
    } else {
        Some(DetachedBridgeConfig::Upsert(matrix_value))
    }
}

async fn persist_detached_matrix_channel_config(
    channel: &Arc<GatewayChannel>,
    detached: DetachedBridgeConfig,
) -> Result<(), (i64, String)> {
    use savfox_core::config::channel_store;

    match detached {
        DetachedBridgeConfig::Delete => {
            // Legacy config patch only represented one matrix channel; map delete to that canonical
            // ID.
            let _ = channel_store::delete_channel_config(
                &channel.config().savfox_home,
                "matrix-matrix",
            )
            .await
            .map_err(|e| {
                (
                    INTERNAL_ERROR,
                    format!("failed to delete matrix channel config: {e}"),
                )
            })?;
            let _ = channel_store::delete_channel_config(&channel.config().savfox_home, "matrix")
                .await
                .map_err(|e| {
                    (
                        INTERNAL_ERROR,
                        format!("failed to delete matrix channel config: {e}"),
                    )
                })?;
        }
        DetachedBridgeConfig::Upsert(Value::Object(matrix_config)) => {
            let patch = Value::Object(matrix_config);
            channel_store::merge_channel_config(
                &channel.config().savfox_home,
                "matrix",
                "Matrix",
                &patch,
            )
            .await
            .map_err(|e| {
                (
                    INTERNAL_ERROR,
                    format!("failed to persist matrix channel config: {e}"),
                )
            })?;
        }
        DetachedBridgeConfig::Upsert(_) => {
            return Err((
                INVALID_PARAMS,
                "gateway.channels.matrix must be an object or null".to_owned(),
            ));
        }
    }

    Ok(())
}

pub(crate) async fn sanitize_config_before_write(
    config: &mut Value,
    channel: &Arc<GatewayChannel>,
) -> Result<(), (i64, String)> {
    normalize_model_reasoning_key(config);
    if let Some(detached_matrix) = take_detached_matrix_channel_config(config) {
        persist_detached_matrix_channel_config(channel, detached_matrix).await?;
    }

    Ok(())
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

    let mut config_value = load_config_value_or_empty(channel).await;
    normalize_config_model_fields(&mut config_value);

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
    let mut config = load_config_intermediate(channel)
        .await
        .map(|doc| doc.value)
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
    if !config.is_object() {
        config = Value::Object(serde_json::Map::new());
    }
    config
}

pub(crate) async fn write_config_toml(
    channel: &GatewayChannel,
    config: &Value,
) -> Result<(), String> {
    let path = primary_config_toml_path(channel);
    let toml_value = savfox_utils::json_to_toml::json_to_toml(config.clone());
    let content = toml::to_string_pretty(&toml_value)
        .map_err(|e| format!("TOML serialization failed: {e}"))?;
    tokio::fs::write(&path, content)
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

    if redacted {
        crate::redaction::redact_json_in_place(&mut doc.value);
    }

    let content = match requested {
        "json" => serde_json::to_string_pretty(&doc.value)
            .map_err(|e| (INTERNAL_ERROR, format!("JSON serialization failed: {e}")))?,
        "yaml" => serde_yaml::to_string(&doc.value)
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

pub(crate) async fn handle_config_set(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let config = params.get("config");
    let Some(config_value) = config else {
        return Err((INVALID_REQUEST, "missing 'config' parameter".to_owned()));
    };

    let mut sanitized = config_value.clone();
    sanitize_config_before_write(&mut sanitized, channel).await?;
    write_config_toml(channel, &sanitized)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "status": "ok" }))
}

pub(crate) async fn handle_config_apply(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let config = params.get("config");
    let Some(config_value) = config else {
        return Err((INVALID_REQUEST, "missing 'config' parameter".to_owned()));
    };

    let mut sanitized = config_value.clone();
    sanitize_config_before_write(&mut sanitized, channel).await?;
    let config_path = primary_config_toml_path(channel);

    // Auto-snapshot before applying (#33)
    let _ = handle_config_snapshot(channel).await;

    // Create a backup before applying.
    if config_path.exists() {
        let backup = config_backup_path(&channel.config().savfox_home);
        let _ = tokio::fs::copy(&config_path, &backup).await;
    }

    write_config_toml(channel, &sanitized)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({
        "status": "applied",
        "note": "restart required for changes to take effect",
    }))
}

pub(crate) async fn handle_config_patch(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let patch = params.get("patch");
    let Some(patch_value) = patch else {
        return Err((INVALID_REQUEST, "missing 'patch' parameter".to_owned()));
    };

    let mut config = load_config_value_or_empty(channel).await;

    // Merge patch fields (deep merge, null deletes keys).
    if patch_value.is_object() {
        deep_merge_patch(&mut config, patch_value);
    }

    sanitize_config_before_write(&mut config, channel).await?;
    write_config_toml(channel, &config)
        .await
        .map_err(|e| (INTERNAL_ERROR, e))?;

    Ok(json!({ "status": "patched" }))
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

// ── Message Reactions (#37) ──────────────────────────────────────────────────

pub(crate) async fn handle_reactions_add(params: &Value, _channel: &GatewayChannel) -> RpcResult {
    let message_id = params
        .get("message_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let emoji = params.get("emoji").and_then(|v| v.as_str()).unwrap_or("");
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");

    if message_id.is_empty() || emoji.is_empty() {
        return Err((INVALID_PARAMS, "missing 'message_id' or 'emoji'".to_owned()));
    }
    let _ = channel;

    // No channel adapter currently dispatches reactions; returning success here
    // would falsely report that the emoji was delivered.
    Err((
        METHOD_NOT_FOUND,
        "reactions.add is not implemented: no channel adapter dispatches reactions yet".to_owned(),
    ))
}

pub(crate) async fn handle_reactions_remove(
    params: &Value,
    _channel: &GatewayChannel,
) -> RpcResult {
    let message_id = params
        .get("message_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let emoji = params.get("emoji").and_then(|v| v.as_str()).unwrap_or("");

    if message_id.is_empty() || emoji.is_empty() {
        return Err((INVALID_PARAMS, "missing 'message_id' or 'emoji'".to_owned()));
    }

    // See handle_reactions_add: reaction dispatch is not wired to any channel yet.
    Err((
        METHOD_NOT_FOUND,
        "reactions.remove is not implemented: no channel adapter dispatches reactions yet"
            .to_owned(),
    ))
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
        "yaml" => serde_yaml::from_str(&source_content)
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
            let yaml = serde_yaml::to_string(&intermediate)
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
