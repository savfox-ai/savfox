use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::types::{INTERNAL_ERROR, INVALID_REQUEST, RpcResult};
use super::super::utils::require_str;
use crate::channel::GatewayChannel;
use crate::session::SessionStore;

pub(in crate::ws_rpc) async fn handle_web_login_start(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("whatsapp");
    if platform == "whatsapp" {
        return Ok(json!({
            "platform": "whatsapp",
            "status": "started",
            "next": "web.login.wait",
            "message": "Scan the QR code in the WhatsApp page and poll web.login.wait.",
        }));
    }
    super::super::handle_channels_login(&json!({ "platform": platform }), channel, session_store)
        .await
}

pub(in crate::ws_rpc) async fn handle_web_login_wait(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("whatsapp");

    let status =
        super::super::handle_channels_status(&json!({ "channel": platform }), channel).await?;
    let connected = status
        .get("connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(json!({
        "platform": platform,
        "connected": connected,
        "status": if connected { "connected" } else { "pending" },
    }))
}

fn nostr_profile_path(channel: &GatewayChannel) -> PathBuf {
    channel
        .config()
        .savfox_home
        .join("gateway")
        .join("nostr-profile.json")
}

fn default_nostr_profile() -> Value {
    json!({
        "name": "",
        "about": "",
        "picture": "",
        "nip05": "",
        "public_key": "",
        "private_key": "",
        "relays": ["wss://relay.damus.io", "wss://nos.lol"],
    })
}

pub(in crate::ws_rpc) async fn load_nostr_profile(channel: &GatewayChannel) -> Value {
    let path = nostr_profile_path(channel);
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(default_nostr_profile)
}

pub(in crate::ws_rpc) async fn save_nostr_profile(
    channel: &GatewayChannel,
    profile: &Value,
) -> Result<(), String> {
    let path = nostr_profile_path(channel);
    crate::json_store::ensure_parent_dir(&path).await?;
    let payload = serde_json::to_string_pretty(profile)
        .map_err(|err| format!("failed to serialize nostr profile: {err}"))?;
    tokio::fs::write(path, payload)
        .await
        .map_err(|err| format!("failed to write nostr profile: {err}"))
}

pub(in crate::ws_rpc) async fn handle_channels_nostr_profile_get(
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let profile = load_nostr_profile(channel).await;
    Ok(json!({ "profile": profile }))
}

pub(in crate::ws_rpc) async fn handle_channels_nostr_profile_set(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let mut profile = load_nostr_profile(channel).await;
    if let Some(incoming) = params.get("profile").and_then(|v| v.as_object()) {
        for (key, value) in incoming {
            profile[key] = value.clone();
        }
    } else {
        for key in [
            "name",
            "about",
            "picture",
            "nip05",
            "public_key",
            "private_key",
            "relays",
        ] {
            if let Some(value) = params.get(key) {
                profile[key] = value.clone();
            }
        }
    }
    save_nostr_profile(channel, &profile)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    Ok(json!({ "status": "saved", "profile": profile }))
}

pub(in crate::ws_rpc) async fn handle_channels_nostr_profile_import(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let private_key = params
        .get("private_key")
        .or_else(|| params.get("keypair"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if private_key.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'private_key' parameter".to_string(),
        ));
    }
    let mut profile = load_nostr_profile(channel).await;
    profile["private_key"] = json!(private_key);
    if let Some(public_key) = params.get("public_key").and_then(|v| v.as_str()) {
        profile["public_key"] = json!(public_key);
    }
    save_nostr_profile(channel, &profile)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    Ok(json!({
        "status": "imported",
        "profile": profile,
    }))
}

pub(in crate::ws_rpc) async fn handle_channels_nostr_profile_export(
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let profile = load_nostr_profile(channel).await;
    Ok(json!({
        "status": "exported",
        "profile": profile,
    }))
}

pub(in crate::ws_rpc) async fn handle_channels_nostr_relays_get(
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let profile = load_nostr_profile(channel).await;
    let relays = profile.get("relays").cloned().unwrap_or_else(|| json!([]));
    Ok(json!({ "relays": relays }))
}

pub(in crate::ws_rpc) async fn handle_channels_nostr_relays_set(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let relays = params.get("relays").cloned().unwrap_or_else(|| json!([]));
    if !relays.is_array() {
        return Err((INVALID_REQUEST, "'relays' must be an array".to_string()));
    }
    let mut profile = load_nostr_profile(channel).await;
    profile["relays"] = relays;
    save_nostr_profile(channel, &profile)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    Ok(json!({
        "status": "saved",
        "relays": profile.get("relays").cloned().unwrap_or_else(|| json!([])),
    }))
}

pub(in crate::ws_rpc) async fn handle_channels_config_list(
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    use savfox_core::config::channel_store;

    let configs = channel_store::list_channel_configs(&channel.config().savfox_home)
        .await
        .map_err(|e| {
            (
                INTERNAL_ERROR,
                format!("failed to list channel configs: {e}"),
            )
        })?;
    Ok(channel_store::channel_configs_to_json(&configs))
}

pub(in crate::ws_rpc) async fn handle_channels_config_get(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    use savfox_core::config::channel_store;

    let channel_id = require_str(params, "channel")?;
    match channel_store::get_channel_config(&channel.config().savfox_home, channel_id).await {
        Ok(Some(config)) => Ok(json!({ "config": channel_store::channel_config_to_json(&config) })),
        Ok(None) => Ok(json!({ "config": serde_json::Value::Null })),
        Err(e) => Err((INTERNAL_ERROR, format!("failed to get channel config: {e}"))),
    }
}

pub(in crate::ws_rpc) async fn handle_channels_config_save(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    use savfox_core::config::channel_store;

    let channel_kind = require_str(params, "channel")?;
    let fallback_name = channel_kind.to_string();
    let channel_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&fallback_name);
    let config_value = params.get("config").cloned().unwrap_or_else(|| json!({}));

    let mut patch = if config_value.is_object() {
        config_value
    } else {
        json!({})
    };
    let nested_enabled = patch
        .as_object_mut()
        .and_then(|obj| obj.remove("enabled"))
        .and_then(|value| value.as_bool());
    if let Some(enabled) = nested_enabled {
        patch["enabled"] = json!(enabled);
    }
    if let Some(id) = params
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        patch["id"] = json!(id);
    }
    if let Some(router) = params.get("router") {
        patch["router"] = router.clone();
    }
    if let Some(dm_policy) = params.get("dm_policy") {
        patch["dm_policy"] = dm_policy.clone();
    }
    if let Some(group_policy) = params.get("group_policy") {
        patch["group_policy"] = group_policy.clone();
    }

    // Validate uniqueness for Matrix appservice channels: no two appservice
    // channels may share the same server_name AND user_prefix.
    if channel_kind.eq_ignore_ascii_case("matrix") {
        let mode = patch.get("mode").and_then(|v| v.as_str()).unwrap_or("user");
        if mode.eq_ignore_ascii_case("appservice") {
            let new_server_name = patch
                .get("serverName")
                .or_else(|| patch.get("server_name"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("");
            let new_user_prefix = patch
                .get("userPrefix")
                .or_else(|| patch.get("user_prefix"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("_savfox_");
            let this_id = patch.get("id").and_then(|v| v.as_str()).unwrap_or("");

            if !new_server_name.is_empty() {
                let existing = channel_store::list_channel_configs(&channel.config().savfox_home)
                    .await
                    .unwrap_or_default();
                for cfg in &existing {
                    if !cfg.kind.eq_ignore_ascii_case("matrix") || cfg.id == this_id {
                        continue;
                    }
                    let cfg_mode = cfg
                        .config
                        .get("mode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("user");
                    if !cfg_mode.eq_ignore_ascii_case("appservice") {
                        continue;
                    }
                    let cfg_server = cfg
                        .config
                        .get("serverName")
                        .or_else(|| cfg.config.get("server_name"))
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .unwrap_or("");
                    let cfg_prefix = cfg
                        .config
                        .get("userPrefix")
                        .or_else(|| cfg.config.get("user_prefix"))
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("_savfox_");
                    if cfg_server.eq_ignore_ascii_case(new_server_name)
                        && cfg_prefix == new_user_prefix
                    {
                        return Err((
                            INVALID_REQUEST,
                            format!(
                                "Another appservice channel ({}) already uses the same server '{}' with user prefix '{}'. \
                                 Each appservice must have a unique server_name + user_prefix combination.",
                                cfg.id, cfg_server, cfg_prefix
                            ),
                        ));
                    }
                }
            }
        }
    }

    match channel_store::merge_channel_config(
        &channel.config().savfox_home,
        channel_kind,
        channel_name,
        &patch,
    )
    .await
    {
        Ok(config) => Ok(
            json!({ "config": channel_store::channel_config_to_json(&config), "status": "saved" }),
        ),
        Err(e) => Err((
            INTERNAL_ERROR,
            format!("failed to save channel config: {e}"),
        )),
    }
}

pub(in crate::ws_rpc) async fn handle_channels_config_delete(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    use savfox_core::config::channel_store;

    let channel_id = require_str(params, "channel")?;

    match channel_store::delete_channel_config(&channel.config().savfox_home, channel_id).await {
        Ok(deleted) => Ok(json!({ "deleted": deleted, "channel": channel_id })),
        Err(e) => Err((
            INTERNAL_ERROR,
            format!("failed to delete channel config: {e}"),
        )),
    }
}
