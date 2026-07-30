use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::types::{INTERNAL_ERROR, INVALID_REQUEST, RpcResult};
use super::super::utils::require_str;
use crate::channel::GatewayChannel;
use crate::session::SessionStore;

#[cfg(feature = "arkret")]
async fn validate_arkret_config_before_save(
    savfox_home: &std::path::Path,
    config: &savfox_core::config::channel_store::ChannelConfig,
) -> Result<(), String> {
    let raw = config
        .config
        .as_object()
        .ok_or_else(|| "Arkret config must be a JSON object".to_owned())?;
    let mode = raw
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| "Arkret config requires mode='agent' or mode='applet'".to_owned())?;
    if mode == "applet" {
        let mut enabled = config.clone();
        enabled.enabled = true;
        let parsed =
            savfox_channels::arkret::applet::ArkretAppletConfig::from_channel_config(&enabled)
                .ok_or_else(|| "Arkret applet config is invalid".to_owned())?;
        return parsed.validate().map_err(|error| error.to_string());
    }
    if mode != "agent" {
        return Err(format!(
            "Arkret mode '{mode}' is invalid; only 'agent' and 'applet' are accepted"
        ));
    }

    let parsed = savfox_channels::arkret::ArkretChannelConfig::from_strict_agent_config(config)
        .map_err(|error| error.to_string())?;
    let account = &parsed.accounts[0];
    let key_ref = account
        .key_ref
        .as_ref()
        .ok_or_else(|| "Arkret agent config is missing keyRef".to_owned())?;
    let verification_method = account
        .verification_method
        .as_deref()
        .ok_or_else(|| "Arkret agent config is missing verificationMethod".to_owned())?;
    let runtime_public_key_digest =
        savfox_channels::arkret::ed25519_runtime_public_key_digest(key_ref, verification_method)
            .map_err(|error| error.to_string())?;
    if account.authorized_event_ref.is_some()
        && let Some(verified) = savfox_channels::arkret::load_verified_runtime_scope(
            savfox_home,
            &config.id,
            account,
            &runtime_public_key_digest,
        )
        .await
        .map_err(|error| error.to_string())?
        && !verified.permits(&account.requested_scope)
    {
        let excess = account
            .requested_scope
            .iter()
            .filter(|action| !verified.actions.contains(action))
            .cloned()
            .collect::<Vec<_>>();
        return Err(format!(
            "requestedScope exceeds the last service-accepted Agent authorization scope: {}",
            excess.join(", ")
        ));
    }
    Ok(())
}

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
            "missing 'private_key' parameter".to_owned(),
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
        return Err((INVALID_REQUEST, "'relays' must be an array".to_owned()));
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
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    use savfox_core::config::channel_store;

    let channel_kind = require_str(params, "channel")?;
    let fallback_name = channel_kind.to_owned();
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

    // Fail-closed rebind guard: a single Arkret runtime channel binds exactly
    // one Agent. Refuse to persist a pairing for a *different* Agent while the
    // channel is still bound — silently overwriting the binding orphaned the
    // previous Agent's KeyPackage pool (empty pool → `direct_conversation_
    // unavailable`). The operator must explicitly unbind first
    // (`channels.arkret.unbind`), which revokes the old pool and purges local
    // state. Progression saves for the *same* Agent, and saves that clear the
    // binding (unbind), carry no differing agent id and pass through.
    #[cfg(feature = "arkret")]
    if channel_kind.eq_ignore_ascii_case("arkret")
        && let Some(incoming_agent) = arkret_patch_agent_id(&patch)
    {
        let target_id = patch
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let existing = channel_store::list_channel_configs(&channel.config().savfox_home)
            .await
            .unwrap_or_default();
        for cfg in &existing {
            if !arkret_save_targets_config(cfg, target_id, channel_name) {
                continue;
            }
            let Ok(parsed) =
                savfox_channels::arkret::ArkretChannelConfig::from_strict_agent_config(cfg)
            else {
                continue;
            };
            for account in &parsed.accounts {
                let bound = account
                    .authorized_event_ref
                    .as_deref()
                    .is_some_and(|reference| !reference.trim().is_empty());
                let bound_agent = account
                    .inkson_bootstrap
                    .as_ref()
                    .map(|bootstrap| bootstrap.agent_id.to_string())
                    .unwrap_or_else(|| account.principal_id.clone());
                if bound && bound_agent.trim() != incoming_agent.trim() {
                    return Err((
                        INVALID_REQUEST,
                        format!(
                            "Arkret channel '{}' is already bound to Agent {bound_agent}. \
                             Unbind the current Agent (channels.arkret.unbind) before pairing \
                             {incoming_agent}; rebinding without unbinding would orphan the \
                             current Agent's KeyPackage pool.",
                            cfg.id
                        ),
                    ));
                }
            }
        }
    }

    #[cfg(feature = "arkret")]
    if channel_kind.eq_ignore_ascii_case("arkret") {
        let candidate = channel_store::preview_merged_channel_config(
            &channel.config().savfox_home,
            channel_kind,
            channel_name,
            &patch,
        )
        .await
        .map_err(|error| {
            (
                INVALID_REQUEST,
                format!("failed to validate Arkret config patch: {error}"),
            )
        })?;
        validate_arkret_config_before_save(&channel.config().savfox_home, &candidate)
            .await
            .map_err(|error| (INVALID_REQUEST, error))?;
    }

    match channel_store::merge_channel_config(
        &channel.config().savfox_home,
        channel_kind,
        channel_name,
        &patch,
    )
    .await
    {
        Ok(config) => {
            // Reconcile only the immutable ID returned by the durable save.
            // This applies mode validation/capability reporting uniformly and
            // prevents edits to one instance from broadcasting to its type.
            let runtime =
                crate::channels::reconcile_channel_instance(&config, channel, session_store).await;
            Ok(json!({
                "config": channel_store::channel_config_to_json(&config),
                "status": "saved",
                "runtime": runtime,
            }))
        }
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

    let selector = require_str(params, "channel")?;
    let Some(config) = channel_store::get_channel_config(&channel.config().savfox_home, selector)
        .await
        .map_err(|e| {
            (
                INTERNAL_ERROR,
                format!("failed to load channel config before deletion: {e}"),
            )
        })?
    else {
        return Ok(json!({ "deleted": false, "channel": selector, "stopped": 0 }));
    };

    // Runtime instances are keyed by the persisted config ID. Stop that exact
    // instance before removing its credentials so deletion cannot leave an
    // orphaned listener running until the gateway is restarted.
    let stopped = u32::from(
        crate::channels::stop_channel_instance(&config, channel)
            .await
            .map_err(|e| {
                (
                    INTERNAL_ERROR,
                    format!("failed to stop channel '{}': {e}", config.id),
                )
            })?,
    );

    match channel_store::delete_channel_config(&channel.config().savfox_home, &config.id).await {
        Ok(deleted) => {
            if deleted {
                crate::channels::recovery::remove_channel_report(
                    &channel.channel_recovery_registry(),
                    &config.id,
                )
                .await;
            }
            Ok(json!({
                "deleted": deleted,
                "channel": config.id,
                "platform": config.kind,
                "stopped": stopped,
            }))
        }
        Err(e) => Err((
            INTERNAL_ERROR,
            format!("failed to delete channel config: {e}"),
        )),
    }
}

/// Agent DID a channel-save patch is trying to bind, if any. Used by the
/// rebind guard and reads the same fields `ArkretChannelConfig` parses.
#[cfg(feature = "arkret")]
fn arkret_patch_agent_id(patch: &Value) -> Option<String> {
    patch
        .get("inksonBootstrap")
        .and_then(|bootstrap| {
            bootstrap
                .get("agent_id")
                .or_else(|| bootstrap.get("agentId"))
        })
        .or_else(|| patch.get("principalId"))
        .or_else(|| patch.get("principal_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(feature = "arkret")]
fn arkret_save_targets_config(
    config: &savfox_core::config::channel_store::ChannelConfig,
    target_id: Option<&str>,
    target_name: &str,
) -> bool {
    config.kind.eq_ignore_ascii_case("arkret")
        && target_id.map_or_else(
            || config.name.trim().eq_ignore_ascii_case(target_name.trim()),
            |id| config.id.eq_ignore_ascii_case(id),
        )
}

/// Explicitly unbind the Agent runtime currently bound to the Arkret channel.
///
/// Single-Agent-per-runtime is intentional; switching Agents is an explicit
/// operation, not a silent overwrite. This revokes the bound Agent's published
/// KeyPackage pool (with its still-current runtime key), stops the listener,
/// purges local Agent MLS identity / durable state, and clears the persisted
/// binding so a new Agent can be paired.
pub(in crate::ws_rpc) async fn handle_channels_arkret_unbind(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    #[cfg(not(feature = "arkret"))]
    {
        let _ = (params, channel);
        Err((
            INVALID_REQUEST,
            "Arkret support is not enabled in this build".to_owned(),
        ))
    }
    #[cfg(feature = "arkret")]
    {
        use savfox_core::config::channel_store;

        let home = channel.config().savfox_home.clone();
        let selector = params
            .get("channel")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("arkret");
        let Some(config) = channel_store::get_channel_config(&home, selector)
            .await
            .map_err(|e| {
                (
                    INTERNAL_ERROR,
                    format!("failed to load Arkret channel config before unbind: {e}"),
                )
            })?
        else {
            return Ok(json!({
                "platform": "arkret",
                "ok": true,
                "unbound": false,
                "message": "no Arkret channel config to unbind",
            }));
        };
        let Ok(parsed) =
            savfox_channels::arkret::ArkretChannelConfig::from_strict_agent_config(&config)
        else {
            return Ok(json!({
                "platform": "arkret",
                "ok": true,
                "unbound": false,
                "channel": config.id,
                "message": "Arkret channel has no bound Agent runtime",
            }));
        };
        let account_id = params
            .get("account_id")
            .or_else(|| params.get("accountId"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(account) = (if let Some(account_id) = account_id {
            parsed
                .accounts
                .iter()
                .find(|account| account.id == account_id)
        } else {
            parsed.accounts.first()
        }) else {
            return Ok(json!({
                "platform": "arkret",
                "ok": true,
                "unbound": false,
                "channel": config.id,
                "message": "Arkret channel has no runtime account to unbind",
            }));
        };

        let report = crate::channels::arkret::unbind_arkret_account(&home, &parsed, account)
            .await
            .map_err(|error| {
                (
                    INTERNAL_ERROR,
                    format!("Arkret Agent unbind stopped before local state was erased: {error:#}"),
                )
            })?;

        // Return the channel to an unbound state so a new Agent can be paired.
        let clear_patch = json!({
            "id": config.id,
            "keyRef": Value::Null,
            "verificationMethod": Value::Null,
            "authorizedEventRef": Value::Null,
            "inksonBootstrap": Value::Null,
        });
        if let Err(e) =
            channel_store::merge_channel_config(&home, &config.kind, &config.name, &clear_patch)
                .await
        {
            return Err((
                INTERNAL_ERROR,
                format!("unbound Agent runtime but failed to clear the channel binding: {e}"),
            ));
        }

        Ok(json!({
            "platform": "arkret",
            "ok": true,
            "unbound": true,
            "channel": config.id,
            "principal_id": report.principal_id,
            "device_id": report.device_id,
            "listeners_stopped": report.listeners_stopped,
            "pool_revoke_attempted": report.revoke_attempted,
            "message": "Unbound Agent runtime: revoked KeyPackage pool, purged local state, and cleared the binding",
        }))
    }
}

#[cfg(all(test, feature = "arkret"))]
mod tests {
    use savfox_core::config::channel_store::ChannelConfig;

    use super::*;

    fn arkret_config(id: &str, name: &str) -> ChannelConfig {
        ChannelConfig {
            id: id.to_owned(),
            kind: "arkret".to_owned(),
            slug: name.to_ascii_lowercase().replace(' ', "-"),
            name: name.to_owned(),
            enabled: true,
            config: json!({}),
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: Some(1),
            updated_at: Some(1),
        }
    }

    #[test]
    fn arkret_rebind_guard_targets_only_the_exact_instance_id() {
        let support = arkret_config("arkret-support", "Support");
        let sales = arkret_config("arkret-sales", "Sales");

        assert!(arkret_save_targets_config(
            &support,
            Some("arkret-support"),
            "Renamed support"
        ));
        assert!(!arkret_save_targets_config(
            &sales,
            Some("arkret-support"),
            "Renamed support"
        ));
    }

    #[test]
    fn arkret_new_save_falls_back_to_exact_name_without_an_id() {
        let support = arkret_config("arkret-support", "Support");
        let sales = arkret_config("arkret-sales", "Sales");

        assert!(arkret_save_targets_config(&support, None, "Support"));
        assert!(!arkret_save_targets_config(&sales, None, "Support"));
    }
}
