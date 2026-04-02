#![allow(unused_imports)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::types::{INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, RpcResult};
use super::super::utils::now_ms;
use super::channel_management::{load_nostr_profile, save_nostr_profile};
use crate::channel::GatewayChannel;
use crate::session::{SessionEntry, SessionStore};

// ── Send / Wake / Channels ──────────────────────────────────────────────────

pub(crate) async fn handle_send(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let channel_id = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");

    if channel_id.is_empty() || text.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'channel' or 'text' parameter".to_owned(),
        ));
    }

    match channel
        .send_platform_message(channel_id, text, None, None, None)
        .await
    {
        Ok(()) => Ok(json!({ "status": "sent" })),
        Err(err) => Err((INTERNAL_ERROR, format!("send error: {err}"))),
    }
}

pub(crate) async fn handle_send_metrics() -> RpcResult {
    let metrics = crate::channels::runtime::send_metrics_snapshot().await;
    Ok(json!({ "metrics": metrics }))
}

pub(crate) async fn handle_wake(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("wake");
    let agent = params
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let heartbeat = params
        .get("heartbeat")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if heartbeat {
        // Heartbeat mode: just acknowledge without invoking agent.
        return Ok(json!({ "status": "heartbeat", "timestamp": chrono::Utc::now().to_rfc3339() }));
    }

    match channel.invoke_agent_text(message, agent).await {
        Ok(reply) => Ok(json!({ "status": "awake", "response": reply })),
        Err(err) => Err((INTERNAL_ERROR, format!("wake error: {err}"))),
    }
}

pub(crate) async fn handle_channels_list(_channel: &Arc<GatewayChannel>) -> RpcResult {
    // List all supported platforms with their webhook endpoints.
    let channels = vec![
        json!({"platform": "discord", "endpoint": "/webhooks/discord", "type": "channel"}),
        json!({"platform": "dingtalk", "endpoint": "/webhooks/dingtalk", "type": "webhook"}),
        json!({"platform": "telegram", "endpoint": "/webhooks/telegram", "type": "channel"}),
        json!({"platform": "slack", "endpoint": "/webhooks/slack", "type": "channel"}),
        json!({"platform": "msteams", "endpoint": "/webhooks/msteams", "type": "channel"}),
        json!({"platform": "webhook", "endpoint": "/webhooks/webhook", "type": "generic"}),
        json!({"platform": "matrix", "endpoint": "/webhooks/matrix", "type": "channel"}),
        json!({"platform": "mattermost", "endpoint": "/webhooks/mattermost", "type": "webhook"}),
        json!({"platform": "googlechat", "endpoint": "/webhooks/googlechat", "type": "webhook"}),
        json!({"platform": "line", "endpoint": "/webhooks/line", "type": "webhook"}),
        json!({"platform": "qq", "endpoint": "/webhooks/qq", "type": "webhook"}),
        json!({"platform": "wechat", "endpoint": "/webhooks/wechat", "type": "webhook"}),
        json!({"platform": "feishu", "endpoint": "/webhooks/feishu", "type": "channel"}),
        json!({"platform": "irc", "endpoint": "/webhooks/irc", "type": "webhook"}),
        json!({"platform": "nostr", "endpoint": "/webhooks/nostr", "type": "channel"}),
        json!({"platform": "zalo", "endpoint": "/webhooks/zalo", "type": "webhook"}),
    ];
    Ok(json!({ "channels": channels }))
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SavedChannelState {
    pub(crate) exists: bool,
    pub(crate) enabled: bool,
    pub(crate) ready: bool,
    pub(crate) channel_name: Option<String>,
    pub(crate) channel_slug: Option<String>,
    pub(crate) config: Option<savfox_core::config::channel_store::ChannelConfig>,
}

pub(crate) fn canonical_channel_platform(platform: &str) -> String {
    match platform.trim().to_ascii_lowercase().as_str() {
        "lark" => "feishu".to_owned(),
        other => other.to_owned(),
    }
}

fn channel_platform_matches_kind(kind: &str, platform: &str) -> bool {
    canonical_channel_platform(kind) == canonical_channel_platform(platform)
}

fn first_non_empty_channel_config_string(
    map: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            let text = value.as_str()?.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_owned())
            }
        })
    })
}

fn first_channel_config_bool(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(Value::as_bool))
}

fn channel_config_collection_len(
    map: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<u32> {
    keys.iter().find_map(|key| {
        let value = map.get(*key)?;
        if let Some(items) = value.as_array() {
            return Some(items.len() as u32);
        }
        if let Some(text) = value.as_str() {
            let count = text
                .lines()
                .flat_map(|line| line.split(','))
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .count();
            if count > 0 {
                return Some(count as u32);
            }
        }

        None
    })
}

pub(crate) fn saved_channel_config_ready(
    config: &savfox_core::config::channel_store::ChannelConfig,
) -> bool {
    let Some(raw) = config.config.as_object() else {
        return false;
    };

    match canonical_channel_platform(&config.kind).as_str() {
        "discord" => {
            first_non_empty_channel_config_string(raw, &["bot_token", "botToken", "token"])
                .is_some()
        }
        "telegram" => {
            first_non_empty_channel_config_string(raw, &["bot_token", "botToken", "token"])
                .is_some()
        }
        "slack" => first_non_empty_channel_config_string(raw, &["bot_token", "botToken", "token"])
            .is_some(),
        "webhook" => {
            first_non_empty_channel_config_string(
                raw,
                &["secret", "webhook_secret", "verify_token"],
            )
            .is_some()
                || first_non_empty_channel_config_string(
                    raw,
                    &["callback_url", "url", "webhook_url"],
                )
                .is_some()
        }
        "dingtalk" => {
            savfox_channels::dingtalk::DingtalkChannelConfig::from_channel_config(config).is_some()
        }
        "feishu" => savfox_channels::feishu::FeishuChannelConfig::from_channel_config(config)
            .is_some_and(|parsed| {
                parsed
                    .app_access_token
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                    || (parsed
                        .app_id
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                        && parsed
                            .app_secret
                            .as_deref()
                            .is_some_and(|value| !value.trim().is_empty()))
            }),
        "matrix" => savfox_channels::matrix::MatrixChannelConfig::from_channel_config(config)
            .is_some_and(|parsed| parsed.is_ready()),
        "mattermost" => {
            first_non_empty_channel_config_string(raw, &["server_url", "serverUrl"]).is_some()
                && first_non_empty_channel_config_string(raw, &["bot_token", "botToken", "token"])
                    .is_some()
        }
        "googlechat" => first_non_empty_channel_config_string(
            raw,
            &[
                "webhook_url",
                "webhookUrl",
                "incoming_webhook_url",
                "incomingWebhookUrl",
                "url",
            ],
        )
        .is_some(),
        "line" => first_non_empty_channel_config_string(
            raw,
            &[
                "channel_token",
                "channel_access_token",
                "channelAccessToken",
                "channelToken",
                "token",
                "access_token",
                "accessToken",
            ],
        )
        .is_some(),
        "whatsapp" => {
            first_non_empty_channel_config_string(raw, &["access_token", "accessToken"]).is_some()
                && first_non_empty_channel_config_string(raw, &["phone_number_id", "phoneNumberId"])
                    .is_some()
        }
        "qq" | "wechat" => first_non_empty_channel_config_string(
            raw,
            &["webhook_url", "webhookUrl", "send_url", "sendUrl", "url"],
        )
        .is_some(),
        "zalo" => {
            first_non_empty_channel_config_string(raw, &["app_id", "appId"]).is_some()
                && first_non_empty_channel_config_string(raw, &["app_secret", "appSecret"])
                    .is_some()
                && first_non_empty_channel_config_string(raw, &["access_token", "accessToken"])
                    .is_some()
        }
        "nextcloud" => {
            first_non_empty_channel_config_string(raw, &["server_url", "serverUrl"]).is_some()
                && first_non_empty_channel_config_string(raw, &["username", "user"]).is_some()
                && first_non_empty_channel_config_string(raw, &["password", "app_password"])
                    .is_some()
                && channel_config_collection_len(raw, &["rooms", "room_tokens"]).unwrap_or(0) > 0
        }
        "twitch" => {
            first_non_empty_channel_config_string(raw, &["bot_username", "botUsername"]).is_some()
                && first_non_empty_channel_config_string(raw, &["oauth_token", "oauthToken"])
                    .is_some()
                && channel_config_collection_len(raw, &["channels"]).unwrap_or(0) > 0
        }
        "tlon" => {
            first_non_empty_channel_config_string(raw, &["ship_url", "shipUrl"]).is_some()
                && first_non_empty_channel_config_string(raw, &["access_code", "accessCode"])
                    .is_some()
                && first_non_empty_channel_config_string(raw, &["ship_name", "shipName"]).is_some()
        }
        _ => !raw.is_empty(),
    }
}

fn saved_channel_stream_enabled(
    config: &savfox_core::config::channel_store::ChannelConfig,
) -> bool {
    match canonical_channel_platform(&config.kind).as_str() {
        "discord" => savfox_channels::discord::DiscordChannelConfig::from_channel_config(config)
            .is_some_and(|parsed| {
                parsed.stream_enabled()
                    && parsed
                        .bot_token
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
            }),
        "telegram" => savfox_channels::telegram::TelegramChannelConfig::from_channel_config(config)
            .is_some_and(|parsed| {
                parsed.polling
                    && parsed
                        .bot_token
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
            }),
        "dingtalk" => savfox_channels::dingtalk::DingtalkChannelConfig::from_channel_config(config)
            .is_some_and(|parsed| parsed.stream_enabled()),
        "feishu" => savfox_channels::feishu::FeishuChannelConfig::from_channel_config(config)
            .is_some_and(|parsed| {
                parsed.stream_enabled()
                    && parsed
                        .app_id
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                    && parsed
                        .app_secret
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
            }),
        _ => false,
    }
}

pub(crate) fn saved_channel_state(
    saved_configs: &[savfox_core::config::channel_store::ChannelConfig],
    platform: &str,
) -> SavedChannelState {
    let preferred = saved_configs
        .iter()
        .find(|cfg| {
            channel_platform_matches_kind(&cfg.kind, platform)
                && cfg.enabled
                && saved_channel_config_ready(cfg)
        })
        .or_else(|| {
            saved_configs
                .iter()
                .find(|cfg| channel_platform_matches_kind(&cfg.kind, platform) && cfg.enabled)
        })
        .or_else(|| {
            saved_configs
                .iter()
                .find(|cfg| channel_platform_matches_kind(&cfg.kind, platform))
        });

    SavedChannelState {
        exists: saved_configs
            .iter()
            .any(|cfg| channel_platform_matches_kind(&cfg.kind, platform)),
        enabled: saved_configs
            .iter()
            .any(|cfg| channel_platform_matches_kind(&cfg.kind, platform) && cfg.enabled),
        ready: saved_configs.iter().any(|cfg| {
            channel_platform_matches_kind(&cfg.kind, platform)
                && cfg.enabled
                && saved_channel_config_ready(cfg)
        }),
        channel_name: preferred.map(|cfg| cfg.name.clone()),
        channel_slug: preferred.map(|cfg| cfg.slug.clone()),
        config: preferred.cloned(),
    }
}

pub(crate) async fn load_saved_channel_configs(
    channel: &GatewayChannel,
) -> Vec<savfox_core::config::channel_store::ChannelConfig> {
    savfox_core::config::channel_store::list_channel_configs(&channel.config().savfox_home)
        .await
        .unwrap_or_default()
}

pub(crate) fn started_saved_channel_count(
    platform: &str,
    saved_configs: &[savfox_core::config::channel_store::ChannelConfig],
    started_channel_ids: &HashSet<String>,
) -> usize {
    saved_configs
        .iter()
        .filter(|config| {
            channel_platform_matches_kind(&config.kind, platform)
                && config.enabled
                && saved_channel_config_ready(config)
                && started_channel_ids.contains(&config.id)
        })
        .count()
}

fn runtime_channel_configured(
    platform: &str,
    runtime: &crate::channel::RuntimeBridgeSecrets,
) -> bool {
    match canonical_channel_platform(platform).as_str() {
        "discord" => {
            runtime.discord_bot_token.is_some() || std::env::var("DISCORD_BOT_TOKEN").is_ok()
        }
        "telegram" => {
            runtime.telegram_bot_token.is_some() || std::env::var("TELEGRAM_BOT_TOKEN").is_ok()
        }
        "slack" => runtime.slack_bot_token.is_some() || std::env::var("SLACK_BOT_TOKEN").is_ok(),
        "webhook" => {
            runtime.webhook_secret.is_some()
                || std::env::var("WEBHOOK_SECRET").is_ok()
                || std::env::var("WEBHOOK_CALLBACK_URL").is_ok()
        }
        "dingtalk" => {
            std::env::var("DINGTALK_WEBHOOK_URL").is_ok()
                || std::env::var("DINGTALK_ACCESS_TOKEN").is_ok()
        }
        "feishu" => {
            std::env::var("FEISHU_TENANT_ACCESS_TOKEN").is_ok()
                || std::env::var("FEISHU_APP_ACCESS_TOKEN").is_ok()
        }
        "matrix" => {
            std::env::var("MATRIX_ACCESS_TOKEN")
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
                || (std::env::var("MATRIX_PASSWORD")
                    .ok()
                    .is_some_and(|value| !value.trim().is_empty())
                    && std::env::var("MATRIX_USER_ID")
                        .ok()
                        .is_some_and(|value| !value.trim().is_empty()))
        }
        "whatsapp" => {
            std::env::var("WHATSAPP_ACCESS_TOKEN").is_ok()
                && std::env::var("WHATSAPP_PHONE_NUMBER_ID").is_ok()
        }
        "mattermost" => {
            std::env::var("MATTERMOST_URL").is_ok() && std::env::var("MATTERMOST_TOKEN").is_ok()
        }
        "googlechat" => std::env::var("GOOGLECHAT_WEBHOOK_URL").is_ok(),
        "line" => {
            std::env::var("LINE_CHANNEL_TOKEN").is_ok()
                || std::env::var("LINE_CHANNEL_ACCESS_TOKEN").is_ok()
        }
        "qq" => std::env::var("QQ_WEBHOOK_URL").is_ok(),
        "wechat" => std::env::var("WECHAT_WEBHOOK_URL").is_ok(),
        "twitch" => {
            std::env::var("TWITCH_OAUTH_TOKEN").is_ok()
                && std::env::var("TWITCH_BOT_USERNAME").is_ok()
        }
        "zalo" => std::env::var("ZALO_OA_ACCESS_TOKEN").is_ok(),
        _ => false,
    }
}

pub(crate) fn channel_is_configured(
    platform: &str,
    runtime: &crate::channel::RuntimeBridgeSecrets,
    saved_configs: &[savfox_core::config::channel_store::ChannelConfig],
    nostr_configured: bool,
) -> bool {
    let platform = canonical_channel_platform(platform);
    if platform == "nostr" {
        return nostr_configured;
    }

    runtime_channel_configured(&platform, runtime)
        || saved_channel_state(saved_configs, &platform).ready
}

fn insert_saved_channel_metadata(
    info: &mut serde_json::Map<String, Value>,
    platform: &str,
    saved_state: &SavedChannelState,
) {
    info.insert("saved".to_owned(), json!(saved_state.exists));
    if !saved_state.exists {
        return;
    }

    info.insert("enabled".to_owned(), json!(saved_state.enabled));
    if let Some(ref cfg) = saved_state.config
        && !cfg.id.is_empty()
    {
        info.insert("id".to_owned(), json!(cfg.id));
    }
    if let Some(name) = saved_state.channel_name.as_ref() {
        info.insert("channel_name".to_owned(), json!(name));
    }
    if let Some(slug) = saved_state.channel_slug.as_ref() {
        info.insert("slug".to_owned(), json!(slug));
    }

    let Some(config_obj) = saved_state
        .config
        .as_ref()
        .and_then(|config| config.config.as_object())
    else {
        return;
    };

    match canonical_channel_platform(platform).as_str() {
        "discord" => {
            let mode = first_non_empty_channel_config_string(
                config_obj,
                &[
                    "mode",
                    "event_mode",
                    "eventMode",
                    "inbound_mode",
                    "inboundMode",
                ],
            )
            .unwrap_or_else(|| "stream".to_owned());
            info.insert("mode".to_owned(), json!(mode));
            if let Some(guild_id) =
                first_non_empty_channel_config_string(config_obj, &["guild_id", "guildId"])
            {
                info.insert("guild_id".to_owned(), json!(guild_id));
            }
        }
        "telegram" => {
            let mode = if first_channel_config_bool(config_obj, &["polling"]).unwrap_or(false) {
                "polling"
            } else {
                "webhook"
            };
            info.insert("mode".to_owned(), json!(mode));
        }
        "slack" => {
            if let Some(workspace_name) = first_non_empty_channel_config_string(
                config_obj,
                &["workspace_name", "workspace", "team_name", "team"],
            ) {
                info.insert("workspace_name".to_owned(), json!(workspace_name));
            }
        }
        "matrix" => {
            if let Some(mode) = first_non_empty_channel_config_string(config_obj, &["mode"]) {
                info.insert("mode".to_owned(), json!(mode));
            }
            if let Some(homeserver) = first_non_empty_channel_config_string(
                config_obj,
                &["homeserver", "homeserver_url", "server_url"],
            ) {
                info.insert("homeserver".to_owned(), json!(homeserver));
            }
            if let Some(user_id) =
                first_non_empty_channel_config_string(config_obj, &["userId", "user_id"])
            {
                info.insert("user_id".to_owned(), json!(user_id));
            }
            if let Some(sender_localpart) = first_non_empty_channel_config_string(
                config_obj,
                &["senderLocalpart", "sender_localpart"],
            ) {
                info.insert("sender_localpart".to_owned(), json!(sender_localpart));
            }
            if let Some(public_url) =
                first_non_empty_channel_config_string(config_obj, &["publicUrl", "public_url"])
            {
                info.insert("appservice_url".to_owned(), json!(public_url));
            }
        }
        "mattermost" => {
            if let Some(server_url) =
                first_non_empty_channel_config_string(config_obj, &["server_url", "serverUrl"])
            {
                info.insert("server_url".to_owned(), json!(server_url));
            }
            if let Some(team_name) =
                first_non_empty_channel_config_string(config_obj, &["team_name", "teamName"])
            {
                info.insert("team_name".to_owned(), json!(team_name));
            }
        }
        "feishu" => {
            if let Some(app_id) =
                first_non_empty_channel_config_string(config_obj, &["appId", "app_id"])
            {
                info.insert("app_id".to_owned(), json!(app_id));
            }
        }
        "webhook" => {
            if let Some(callback_url) = first_non_empty_channel_config_string(
                config_obj,
                &["callback_url", "webhook_url", "url"],
            ) {
                info.insert("callback_url".to_owned(), json!(callback_url));
            }
        }
        "dingtalk" => {
            if let Some(webhook_url) =
                first_non_empty_channel_config_string(config_obj, &["webhook_url", "url"])
            {
                info.insert("webhook_url".to_owned(), json!(webhook_url));
            }
        }
        "zalo" => {
            if let Some(app_id) =
                first_non_empty_channel_config_string(config_obj, &["app_id", "appId"])
            {
                info.insert("app_id".to_owned(), json!(app_id));
            }
        }
        "nextcloud" => {
            if let Some(server_url) =
                first_non_empty_channel_config_string(config_obj, &["server_url", "serverUrl"])
            {
                info.insert("server_url".to_owned(), json!(server_url));
            }
            if let Some(room_count) =
                channel_config_collection_len(config_obj, &["rooms", "room_tokens"])
            {
                info.insert("room_count".to_owned(), json!(room_count));
            }
        }
        "twitch" => {
            if let Some(bot_username) =
                first_non_empty_channel_config_string(config_obj, &["bot_username", "botUsername"])
            {
                info.insert("bot_username".to_owned(), json!(bot_username));
            }
            if let Some(channel_count) = channel_config_collection_len(config_obj, &["channels"]) {
                info.insert("channel_count".to_owned(), json!(channel_count));
            }
        }
        "tlon" => {
            if let Some(ship_url) =
                first_non_empty_channel_config_string(config_obj, &["ship_url", "shipUrl"])
            {
                info.insert("ship_url".to_owned(), json!(ship_url));
            }
            if let Some(ship_name) =
                first_non_empty_channel_config_string(config_obj, &["ship_name", "shipName"])
            {
                info.insert("ship_name".to_owned(), json!(ship_name));
            }
            if let Some(channel_count) = channel_config_collection_len(config_obj, &["channels"]) {
                info.insert("channel_count".to_owned(), json!(channel_count));
            }
        }
        _ => {}
    }
}

pub(crate) async fn handle_channels_status(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let runtime = channel.runtime_channel_secrets().await;
    let saved_configs = load_saved_channel_configs(channel).await;
    let probe_requested = params
        .get("probe")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let health_metrics = crate::channels::runtime::channel_health_snapshot().await;
    let send_metrics = crate::channels::runtime::send_metrics_snapshot().await;
    let nostr_profile = load_nostr_profile(channel).await;
    let nostr_configured = nostr_profile
        .get("private_key")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty());
    let nostr_public_key = nostr_profile
        .get("public_key")
        .cloned()
        .unwrap_or(Value::Null);
    let nostr_relay_count = nostr_profile
        .get("relays")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len() as u32)
        .unwrap_or(0);
    let discord_configured =
        channel_is_configured("discord", &runtime, &saved_configs, nostr_configured);
    let discord_saved = saved_channel_state(&saved_configs, "discord");
    let discord_runtime_states = savfox_channels::discord::discord_stream_state_snapshot();
    let discord_runtime = saved_configs
        .iter()
        .filter(|config| channel_platform_matches_kind(&config.kind, "discord"))
        .find_map(|config| discord_runtime_states.get(&config.id).cloned())
        .or_else(|| discord_runtime_states.values().next().cloned());
    let discord_running = if let Some(config) = discord_saved.config.as_ref() {
        if saved_channel_stream_enabled(config) {
            savfox_channels::discord::is_discord_stream_running(&config.id).await
        } else {
            false
        }
    } else {
        false
    };
    let discord_connected = discord_runtime
        .as_ref()
        .map(|state| state.connected)
        .unwrap_or(false);
    let telegram_configured =
        channel_is_configured("telegram", &runtime, &saved_configs, nostr_configured);
    let telegram_saved = saved_channel_state(&saved_configs, "telegram");
    let telegram_running = if let Some(config) = telegram_saved.config.as_ref() {
        if saved_channel_stream_enabled(config) {
            savfox_channels::telegram::is_telegram_polling_running(&config.id).await
        } else {
            telegram_configured
        }
    } else {
        telegram_configured
    };
    let slack_configured =
        channel_is_configured("slack", &runtime, &saved_configs, nostr_configured);
    let matrix_configured =
        channel_is_configured("matrix", &runtime, &saved_configs, nostr_configured);
    let matrix_saved = saved_channel_state(&saved_configs, "matrix");
    let matrix_runtime_states = crate::channels::matrix::matrix_runtime_state_snapshot();
    let matrix_runtime = saved_configs
        .iter()
        .filter(|config| channel_platform_matches_kind(&config.kind, "matrix"))
        .find_map(|config| matrix_runtime_states.get(&config.id).cloned())
        .or_else(|| matrix_runtime_states.values().next().cloned());
    let matrix_registry_running = {
        let registry = channel.channel_registry();
        let started_channel_ids = registry
            .read()
            .await
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        started_saved_channel_count("matrix", &saved_configs, &started_channel_ids) > 0
            || (!matrix_saved.ready && runtime_channel_configured("matrix", &runtime))
    };
    let matrix_running = matrix_runtime.is_some() || matrix_registry_running;
    let matrix_connected = matrix_runtime
        .as_ref()
        .map(|state| state.connected)
        .unwrap_or(matrix_running);
    let whatsapp_configured =
        channel_is_configured("whatsapp", &runtime, &saved_configs, nostr_configured);
    let signal_configured =
        channel_is_configured("signal", &runtime, &saved_configs, nostr_configured);
    let mattermost_configured =
        channel_is_configured("mattermost", &runtime, &saved_configs, nostr_configured);
    let googlechat_configured =
        channel_is_configured("googlechat", &runtime, &saved_configs, nostr_configured);
    let irc_configured = channel_is_configured("irc", &runtime, &saved_configs, nostr_configured);
    let line_configured = channel_is_configured("line", &runtime, &saved_configs, nostr_configured);
    let qq_configured = channel_is_configured("qq", &runtime, &saved_configs, nostr_configured);
    let wechat_configured =
        channel_is_configured("wechat", &runtime, &saved_configs, nostr_configured);
    let dingtalk_saved = saved_channel_state(&saved_configs, "dingtalk");
    let dingtalk_configured =
        runtime_channel_configured("dingtalk", &runtime) || dingtalk_saved.ready;
    let dingtalk_running = dingtalk_saved
        .config
        .as_ref()
        .is_some_and(saved_channel_stream_enabled);
    let feishu_saved = saved_channel_state(&saved_configs, "feishu");
    let feishu_configured = runtime_channel_configured("feishu", &runtime) || feishu_saved.ready;
    let feishu_running = if let Some(config) = feishu_saved.config.as_ref() {
        saved_channel_stream_enabled(config)
            && savfox_channels::feishu::is_feishu_stream_running(&config.id).await
    } else {
        false
    };
    let webhook_saved = saved_channel_state(&saved_configs, "webhook");
    let webhook_configured = runtime_channel_configured("webhook", &runtime) || webhook_saved.ready;
    let nextcloud_configured =
        channel_is_configured("nextcloud", &runtime, &saved_configs, nostr_configured);
    let twitch_configured =
        channel_is_configured("twitch", &runtime, &saved_configs, nostr_configured);
    let tlon_configured = channel_is_configured("tlon", &runtime, &saved_configs, nostr_configured);
    let zalo_configured = channel_is_configured("zalo", &runtime, &saved_configs, nostr_configured);

    let mut channels = json!({
        "discord": {
            "configured": discord_configured,
            "running": discord_running,
            "connected": discord_connected,
        },
        "telegram": {
            "configured": telegram_configured,
            "running": telegram_running,
            "connected": telegram_running,
        },
        "slack": {
            "configured": slack_configured,
            "running": slack_configured,
            "connected": slack_configured,
        },
        "matrix": {
            "configured": matrix_configured,
            "running": matrix_running,
            "connected": matrix_connected,
        },
        "whatsapp": {
            "configured": whatsapp_configured,
            "running": whatsapp_configured,
            "connected": whatsapp_configured,
            "linked": false,
            "qr_data_url": Value::Null,
        },
        "signal": {
            "configured": signal_configured,
            "running": false,
            "connected": false,
        },
        "mattermost": {
            "configured": mattermost_configured,
            "running": mattermost_configured,
            "connected": mattermost_configured,
        },
        "googlechat": {
            "configured": googlechat_configured,
            "running": googlechat_configured,
            "connected": googlechat_configured,
        },
        "webhook": {
            "configured": webhook_configured,
            "running": webhook_configured,
            "connected": webhook_configured,
        },
        "irc": {
            "configured": irc_configured,
            "running": false,
            "connected": false,
        },
        "line": {
            "configured": line_configured,
            "running": line_configured,
            "connected": line_configured,
        },
        "qq": {
            "configured": qq_configured,
            "running": qq_configured,
            "connected": qq_configured,
        },
        "wechat": {
            "configured": wechat_configured,
            "running": wechat_configured,
            "connected": wechat_configured,
        },
        "dingtalk": {
            "configured": dingtalk_configured,
            "running": dingtalk_running,
            "connected": false,
        },
        "zalo": {
            "configured": zalo_configured,
            "running": false,
            "connected": false,
        },
        "nextcloud": {
            "configured": nextcloud_configured,
            "running": false,
            "connected": false,
        },
        "twitch": {
            "configured": twitch_configured,
            "running": false,
            "connected": false,
        },
        "tlon": {
            "configured": tlon_configured,
            "running": false,
            "connected": false,
        },
        "feishu": {
            "configured": feishu_configured,
            "running": feishu_running,
            "connected": feishu_running,
        },
        "nostr": {
            "configured": nostr_configured,
            "running": nostr_configured,
            "connected": nostr_configured,
            "public_key": nostr_public_key,
            "relay_count": nostr_relay_count,
        },
    });

    // Overlay persisted channel configs so UI can restore configured channels on page load.
    if let Some(channels_map) = channels.as_object_mut() {
        let mut processed_platforms = HashSet::new();
        for saved in &saved_configs {
            let key = canonical_channel_platform(&saved.kind);
            if !processed_platforms.insert(key.clone()) {
                continue;
            }

            let saved_state = saved_channel_state(&saved_configs, &key);
            let entry = channels_map.entry(key.clone()).or_insert_with(|| {
                json!({
                    "configured": false,
                    "running": false,
                    "connected": false,
                })
            });
            if let Some(obj) = entry.as_object_mut() {
                let configured = obj
                    .get("configured")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || saved_state.ready;
                obj.insert("configured".to_owned(), json!(configured));
                insert_saved_channel_metadata(obj, &key, &saved_state);
            }
        }

        if let Some(discord_runtime) = discord_runtime.as_ref()
            && let Some(discord_entry) = channels_map
                .get_mut("discord")
                .and_then(Value::as_object_mut)
        {
            discord_entry.insert("running".to_owned(), json!(discord_running));
            discord_entry.insert("connected".to_owned(), json!(discord_runtime.connected));
            if let Some(bot_user_id) = discord_runtime.bot_user_id.as_deref() {
                discord_entry.insert("bot_user_id".to_owned(), json!(bot_user_id));
            }
            if let Some(bot_username) = discord_runtime.bot_username.as_deref() {
                discord_entry.insert("bot_username".to_owned(), json!(bot_username));
            }
            if let Some(guild_count) = discord_runtime.guild_count {
                discord_entry.insert("guild_count".to_owned(), json!(guild_count));
            }
            discord_entry.insert("last_error".to_owned(), json!(discord_runtime.last_error));
        }

        if let Some(matrix_runtime) = matrix_runtime.as_ref()
            && let Some(matrix_entry) = channels_map
                .get_mut("matrix")
                .and_then(Value::as_object_mut)
        {
            matrix_entry.insert("running".to_owned(), json!(true));
            matrix_entry.insert("connected".to_owned(), json!(matrix_runtime.connected));
            if let Some(mode) = matrix_runtime.mode.as_deref() {
                matrix_entry.insert("mode".to_owned(), json!(mode));
            }
            if let Some(homeserver) = matrix_runtime.homeserver.as_deref() {
                matrix_entry.insert("homeserver".to_owned(), json!(homeserver));
            }
            if let Some(user_id) = matrix_runtime.user_id.as_deref() {
                matrix_entry.insert("user_id".to_owned(), json!(user_id));
            }
            if let Some(room_count) = matrix_runtime.room_count {
                matrix_entry.insert("room_count".to_owned(), json!(room_count));
            }
            if let Some(appservice_url) = matrix_runtime.appservice_url.as_deref() {
                matrix_entry.insert("appservice_url".to_owned(), json!(appservice_url));
            }
            if let Some(sender_localpart) = matrix_runtime.sender_localpart.as_deref() {
                matrix_entry.insert("sender_localpart".to_owned(), json!(sender_localpart));
            }
            if let Some(user_prefix) = matrix_runtime.user_prefix.as_deref() {
                matrix_entry.insert("user_prefix".to_owned(), json!(user_prefix));
            }
            if let Some(server_name) = matrix_runtime.server_name.as_deref() {
                matrix_entry.insert("server_name".to_owned(), json!(server_name));
            }
            if let Some(config_id) = matrix_runtime.config_id.as_deref() {
                matrix_entry.insert("config_id".to_owned(), json!(config_id));
            }
            if let Some(registration) = matrix_runtime.registration.as_ref() {
                matrix_entry.insert("registration".to_owned(), registration.clone());
            }
            matrix_entry.insert("last_error".to_owned(), json!(matrix_runtime.last_error));
        }
    }

    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let to_rfc3339 = |timestamp_ms: Option<u64>| -> Value {
        timestamp_ms
            .and_then(|ts| chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ts as i64))
            .map(|dt| Value::String(dt.to_rfc3339()))
            .unwrap_or(Value::Null)
    };

    if let Some(map) = channels.as_object_mut() {
        for (platform, info) in map.iter_mut() {
            let metrics = health_metrics.get(platform).cloned().unwrap_or_default();
            let send = send_metrics.get(platform).cloned().unwrap_or_default();
            let error_rate = if send.attempts == 0 {
                0.0
            } else {
                send.failed as f64 / send.attempts as f64
            };
            let connection_uptime_ms = metrics
                .connected_since_ms
                .map(|since| now_ms.saturating_sub(since));
            let last_activity_ms = [metrics.last_event_time_ms, metrics.last_message_time_ms]
                .into_iter()
                .flatten()
                .max();

            let configured = info
                .get("configured")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let connected = info
                .get("connected")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let probe_status = if probe_requested {
                if !configured {
                    "not_configured".to_owned()
                } else if connected {
                    "ok".to_owned()
                } else {
                    "degraded".to_owned()
                }
            } else {
                metrics
                    .probe_status
                    .clone()
                    .unwrap_or_else(|| "unknown".to_owned())
            };
            if probe_requested {
                crate::channels::runtime::record_channel_probe(platform, &probe_status).await;
            }

            if let Some(obj) = info.as_object_mut() {
                obj.insert(
                    "last_message_time".to_owned(),
                    json!(metrics.last_message_time_ms),
                );
                obj.insert(
                    "last_event_time".to_owned(),
                    json!(metrics.last_event_time_ms),
                );
                obj.insert(
                    "reconnect_attempt_count".to_owned(),
                    json!(metrics.reconnect_attempt_count),
                );
                obj.insert("probe_status".to_owned(), json!(probe_status));
                obj.insert(
                    "connection_uptime_ms".to_owned(),
                    json!(connection_uptime_ms),
                );
                obj.insert("error_rate".to_owned(), json!(error_rate));
                obj.insert("messages_total".to_owned(), json!(send.attempts));
                obj.insert("messages_failed".to_owned(), json!(send.failed));
                obj.insert(
                    "last_activity".to_owned(),
                    json!(last_activity_ms.map(|ts| ts as i64)),
                );
                obj.insert(
                    "last_probe_at".to_owned(),
                    json!(metrics.last_probe_time_ms.map(|ts| ts as i64)),
                );
                obj.insert(
                    "probe".to_owned(),
                    json!({
                        "ok": probe_status == "ok",
                        "status": probe_status,
                    }),
                );

                obj.insert(
                    "lastMessageTime".to_owned(),
                    to_rfc3339(metrics.last_message_time_ms),
                );
                obj.insert(
                    "lastEventTime".to_owned(),
                    to_rfc3339(metrics.last_event_time_ms),
                );
                obj.insert(
                    "reconnectAttemptCount".to_owned(),
                    json!(metrics.reconnect_attempt_count),
                );
                obj.insert("probeStatus".to_owned(), json!(probe_status));
                obj.insert("uptimeMs".to_owned(), json!(connection_uptime_ms));
                obj.insert("errorRate".to_owned(), json!(error_rate));
                obj.insert(
                    "lastProbeAt".to_owned(),
                    to_rfc3339(metrics.last_probe_time_ms),
                );
                obj.insert("lastActivity".to_owned(), to_rfc3339(last_activity_ms));
            }
        }
    }

    let requested_channel = params
        .get("channel")
        .or_else(|| params.get("platform"))
        .and_then(|v| v.as_str())
        .map(canonical_channel_platform);
    if let Some(channel) = requested_channel.as_deref() {
        if let Some(entry) = channels.get(channel) {
            let mut payload = entry.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("platform".to_owned(), Value::String(channel.to_owned()));
            }
            return Ok(payload);
        }
        return Err((INVALID_REQUEST, format!("unknown channel: {channel}")));
    }

    Ok(json!({ "channels": channels }))
}

pub(crate) async fn handle_channels_login(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .map(canonical_channel_platform)
        .unwrap_or_default();
    if platform.is_empty() {
        return Err((INVALID_REQUEST, "missing 'platform' parameter".to_owned()));
    }

    let runtime = channel.runtime_channel_secrets().await;
    let saved_configs = load_saved_channel_configs(channel).await;
    let nostr_profile = load_nostr_profile(channel).await;
    let nostr_configured = nostr_profile
        .get("private_key")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty());
    let is_configured =
        channel_is_configured(&platform, &runtime, &saved_configs, nostr_configured);

    if platform == "discord" {
        if !is_configured {
            return Ok(json!({
                "platform": platform,
                "status": "needs_config",
                "configured": false,
                "message": "Please configure discord in the channel settings",
            }));
        }

        let mut started = 0_u32;
        let mut already_running = 0_u32;
        let mut webhook_mode = 0_u32;
        let mut ready_configs = 0_u32;

        for saved in &saved_configs {
            if !channel_platform_matches_kind(&saved.kind, "discord") || !saved.enabled {
                continue;
            }
            let Some(parsed) =
                savfox_channels::discord::DiscordChannelConfig::from_channel_config(saved)
            else {
                continue;
            };
            if !saved_channel_config_ready(saved) {
                continue;
            }

            ready_configs = ready_configs.saturating_add(1);
            if !parsed.stream_enabled() {
                webhook_mode = webhook_mode.saturating_add(1);
                continue;
            }
            if savfox_channels::discord::is_discord_stream_running(&saved.id).await {
                already_running = already_running.saturating_add(1);
                continue;
            }

            let sink = crate::channels::discord::discord_sink(
                Arc::clone(channel),
                Arc::clone(session_store),
                saved.id.clone(),
            );
            savfox_channels::discord::start_discord_stream(&saved.id, &parsed, sink)
                .await
                .map_err(|err| {
                    (
                        INTERNAL_ERROR,
                        format!("failed to start Discord stream '{}': {err}", saved.id),
                    )
                })?;
            started = started.saturating_add(1);
        }

        let (status, message) = if started > 0 {
            (
                "started",
                format!("Started {started} Discord stream channel(s)"),
            )
        } else if already_running > 0 {
            (
                "already_running",
                format!("{already_running} Discord stream channel(s) already running"),
            )
        } else if webhook_mode > 0 {
            (
                "webhook_mode",
                "Discord is configured in webhook mode; no gateway stream was started".to_owned(),
            )
        } else if ready_configs > 0 {
            (
                "configured",
                "Discord is configured, but no enabled stream channel was found".to_owned(),
            )
        } else {
            (
                "already_configured",
                "Discord credentials are configured via environment/runtime settings".to_owned(),
            )
        };

        return Ok(json!({
            "platform": platform,
            "status": status,
            "configured": true,
            "started": started,
            "already_running": already_running,
            "message": message,
        }));
    }

    if platform == "matrix" {
        if !is_configured {
            return Ok(json!({
                "platform": platform,
                "status": "needs_config",
                "configured": false,
                "message": "Please configure matrix in the channel settings",
            }));
        }

        let registry = channel.channel_registry();
        let started_channel_ids = registry
            .read()
            .await
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let mut started = 0_u32;
        let mut already_running = 0_u32;
        let mut ready_configs = 0_u32;

        for saved in &saved_configs {
            if !channel_platform_matches_kind(&saved.kind, "matrix")
                || !saved.enabled
                || !saved_channel_config_ready(saved)
            {
                continue;
            }

            ready_configs = ready_configs.saturating_add(1);
            if started_channel_ids.contains(&saved.id) {
                already_running = already_running.saturating_add(1);
                continue;
            }

            crate::channels::start_matrix_channel(saved, &registry, channel, session_store)
                .await
                .map_err(|err| {
                    (
                        INTERNAL_ERROR,
                        format!("failed to start Matrix channel '{}': {err}", saved.id),
                    )
                })?;
            started = started.saturating_add(1);
        }

        let (status, message) = if started > 0 {
            ("started", format!("Started {started} Matrix channel(s)"))
        } else if already_running > 0 {
            (
                "already_running",
                format!("{already_running} Matrix channel(s) already running"),
            )
        } else if ready_configs > 0 {
            (
                "configured",
                "Matrix is configured, but no enabled channel was started".to_owned(),
            )
        } else {
            (
                "already_configured",
                "Matrix credentials are configured via environment/runtime settings".to_owned(),
            )
        };

        return Ok(json!({
            "platform": platform,
            "status": status,
            "configured": true,
            "started": started,
            "already_running": already_running,
            "message": message,
        }));
    }

    if platform == "feishu" {
        if !is_configured {
            return Ok(json!({
                "platform": platform,
                "status": "needs_config",
                "configured": false,
                "message": "Please configure feishu in the channel settings",
            }));
        }

        let mut started = 0_u32;
        let mut already_running = 0_u32;
        let mut stream_disabled = 0_u32;
        for saved in &saved_configs {
            if !channel_platform_matches_kind(&saved.kind, "feishu") || !saved.enabled {
                continue;
            }
            let Some(parsed) =
                savfox_channels::feishu::FeishuChannelConfig::from_channel_config(saved)
            else {
                continue;
            };
            if !parsed.stream_enabled() {
                stream_disabled = stream_disabled.saturating_add(1);
                continue;
            }
            if savfox_channels::feishu::is_feishu_stream_running(&saved.id).await {
                already_running = already_running.saturating_add(1);
                continue;
            }

            let sink = crate::channels::feishu::feishu_sink(
                Arc::clone(channel),
                Arc::clone(session_store),
            );
            savfox_channels::feishu::start_feishu_stream(&saved.id, &parsed, sink)
                .await
                .map_err(|err| {
                    (
                        INTERNAL_ERROR,
                        format!("failed to start Feishu stream '{}': {err}", saved.id),
                    )
                })?;
            started = started.saturating_add(1);
        }

        let (status, message) = if started > 0 {
            (
                "started",
                format!("Started {started} Feishu stream channel(s)"),
            )
        } else if already_running > 0 {
            (
                "already_running",
                format!("{already_running} Feishu stream channel(s) already running"),
            )
        } else if stream_disabled > 0 {
            (
                "webhook_mode",
                "Feishu is configured in webhook mode; no stream channel was started".to_owned(),
            )
        } else {
            (
                "configured",
                "Feishu is configured, but no enabled stream channel was found".to_owned(),
            )
        };

        return Ok(json!({
            "platform": platform,
            "status": status,
            "configured": true,
            "started": started,
            "already_running": already_running,
            "message": message,
        }));
    }

    if platform == "telegram" {
        if !is_configured {
            return Ok(json!({
                "platform": platform,
                "status": "needs_config",
                "configured": false,
                "message": "Please configure telegram in the channel settings",
            }));
        }

        let mut started = 0_u32;
        let mut already_running = 0_u32;
        let mut polling_disabled = 0_u32;
        for saved in &saved_configs {
            if !channel_platform_matches_kind(&saved.kind, "telegram") || !saved.enabled {
                continue;
            }
            let Some(parsed) =
                savfox_channels::telegram::TelegramChannelConfig::from_channel_config(saved)
            else {
                continue;
            };
            if !parsed.polling {
                polling_disabled = polling_disabled.saturating_add(1);
                continue;
            }
            if savfox_channels::telegram::is_telegram_polling_running(&saved.id).await {
                already_running = already_running.saturating_add(1);
                continue;
            }

            let sink = crate::channels::telegram::telegram_sink(
                Arc::clone(channel),
                Arc::clone(session_store),
            );
            savfox_channels::telegram::start_telegram_polling(&saved.id, &parsed, sink)
                .await
                .map_err(|err| {
                    (
                        INTERNAL_ERROR,
                        format!("failed to start Telegram polling '{}': {err}", saved.id),
                    )
                })?;
            started = started.saturating_add(1);
        }

        let (status, message) = if started > 0 {
            (
                "started",
                format!("Started {started} Telegram polling channel(s)"),
            )
        } else if already_running > 0 {
            (
                "already_running",
                format!("{already_running} Telegram polling channel(s) already running"),
            )
        } else if polling_disabled > 0 {
            (
                "webhook_mode",
                "Telegram is configured in webhook mode; no polling channel was started".to_owned(),
            )
        } else {
            (
                "configured",
                "Telegram is configured, but no enabled polling channel was found".to_owned(),
            )
        };

        return Ok(json!({
            "platform": platform,
            "status": status,
            "configured": true,
            "started": started,
            "already_running": already_running,
            "message": message,
        }));
    }

    Ok(json!({
        "platform": platform,
        "status": if is_configured { "already_configured" } else { "needs_config" },
        "configured": is_configured,
        "message": if is_configured {
            format!("{platform} is already configured")
        } else {
            format!("Please configure {platform} in the channel settings")
        }
    }))
}

pub(crate) async fn handle_channels_logout(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .map(canonical_channel_platform)
        .unwrap_or_default();
    if platform.is_empty() {
        return Err((INVALID_REQUEST, "missing 'platform' parameter".to_owned()));
    }

    let mut secrets = channel.runtime_channel_secrets().await;
    let mut stopped = 0_u32;
    match platform.as_str() {
        "discord" => {
            secrets.discord_bot_token = None;
            for saved in load_saved_channel_configs(channel).await {
                if channel_platform_matches_kind(&saved.kind, "discord")
                    && savfox_channels::discord::stop_discord_stream(&saved.id).await
                {
                    stopped = stopped.saturating_add(1);
                }
            }
        }
        "telegram" => {
            secrets.telegram_bot_token = None;
            for saved in load_saved_channel_configs(channel).await {
                if channel_platform_matches_kind(&saved.kind, "telegram")
                    && savfox_channels::telegram::stop_telegram_polling(&saved.id).await
                {
                    stopped = stopped.saturating_add(1);
                }
            }
        }
        "slack" => {
            secrets.slack_bot_token = None;
            secrets.slack_signing_secret = None;
        }
        "webhook" => secrets.webhook_secret = None,
        "nostr" => {
            let mut profile = load_nostr_profile(channel).await;
            profile["private_key"] = json!("");
            profile["public_key"] = json!("");
            let _ = save_nostr_profile(channel, &profile).await;
        }
        "feishu" => {
            for saved in load_saved_channel_configs(channel).await {
                if channel_platform_matches_kind(&saved.kind, "feishu")
                    && savfox_channels::feishu::stop_feishu_stream(&saved.id).await
                {
                    stopped = stopped.saturating_add(1);
                }
            }
        }
        "matrix" => {
            let registry = channel.channel_registry();
            let mut registry = registry.write().await;
            for saved in load_saved_channel_configs(channel).await {
                if channel_platform_matches_kind(&saved.kind, "matrix") {
                    let removed_registry = registry.remove(&saved.id).is_some();
                    let had_appservice =
                        crate::channels::matrix::matrix_appservice_channel_for(&saved.id).is_some();
                    crate::channels::matrix::remove_matrix_appservice_channel(&saved.id);
                    if removed_registry || had_appservice {
                        stopped = stopped.saturating_add(1);
                    }
                }
            }
        }
        "whatsapp" | "signal" | "mattermost" | "googlechat" | "irc" | "line" | "dingtalk"
        | "zalo" | "nextcloud" | "twitch" | "tlon" | "qq" | "wechat" => {
            // These platforms may not have runtime secrets yet
        }
        _ => {
            return Err((INVALID_REQUEST, format!("unknown platform: {platform}")));
        }
    }
    channel.set_runtime_channel_secrets(secrets).await;

    Ok(json!({
        "platform": platform,
        "status": if matches!(platform.as_str(), "discord" | "feishu" | "matrix") && stopped > 0 {
            "stopped"
        } else if matches!(platform.as_str(), "discord" | "feishu" | "matrix") {
            "already_stopped"
        } else {
            "logged_out"
        },
        "stopped": stopped,
    }))
}

pub(crate) async fn handle_channels_test(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let platform = params
        .get("platform")
        .or_else(|| params.get("channel"))
        .and_then(|v| v.as_str())
        .map(canonical_channel_platform)
        .unwrap_or_default();
    if platform.is_empty() {
        return Err((INVALID_REQUEST, "missing 'platform' parameter".to_owned()));
    }

    let runtime = channel.runtime_channel_secrets().await;
    let saved_configs = load_saved_channel_configs(channel).await;
    let nostr_profile = load_nostr_profile(channel).await;
    let nostr_configured = nostr_profile
        .get("private_key")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty());
    let configured = channel_is_configured(&platform, &runtime, &saved_configs, nostr_configured);

    Ok(json!({
        "platform": platform,
        "ok": configured,
        "message": if configured {
            format!("{platform} test passed")
        } else {
            format!("{platform} is not configured. Please add configuration in the channel settings.")
        }
    }))
}

pub(crate) async fn handle_channels_account_update(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let platform = params
        .get("platform")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let account = params.get("account").and_then(|v| v.as_str()).unwrap_or("");
    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if platform.is_empty() {
        return Err((INVALID_REQUEST, "missing 'platform' parameter".to_owned()));
    }
    if account.is_empty() {
        return Err((INVALID_REQUEST, "missing 'account' parameter".to_owned()));
    }

    let path = channel
        .config()
        .savfox_home
        .join("gateway")
        .join("channel-accounts.json");
    let mut root = tokio::fs::read_to_string(&path)
        .await
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    if !root.is_object() {
        root = json!({});
    }
    root[platform]["accounts"][account] = json!({
        "enabled": enabled,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });

    let _ = crate::json_store::ensure_parent_dir(&path).await;
    let payload = serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".to_owned());
    if let Err(err) = tokio::fs::write(&path, payload).await {
        return Err((
            INTERNAL_ERROR,
            format!("failed to persist account state: {err}"),
        ));
    }

    Ok(json!({
        "platform": platform,
        "account": account,
        "enabled": enabled,
        "status": "updated",
    }))
}

const DIRECTORY_SUPPORTED_CHANNELS: [&str; 6] =
    ["discord", "slack", "telegram", "whatsapp", "qq", "wechat"];

fn normalize_directory_channel(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    if DIRECTORY_SUPPORTED_CHANNELS.contains(&normalized.as_str()) {
        Some(normalized)
    } else {
        None
    }
}

fn parse_directory_channels(params: &Value) -> Result<Vec<String>, (i64, String)> {
    let mut channels = Vec::new();

    if let Some(raw_channels) = params.get("channels").and_then(|v| v.as_array()) {
        for raw in raw_channels {
            if let Some(value) = raw.as_str() {
                let Some(channel) = normalize_directory_channel(value) else {
                    return Err((INVALID_PARAMS, format!("unsupported channel: {value}")));
                };
                if !channels.contains(&channel) {
                    channels.push(channel);
                }
            }
        }
    }

    for key in ["channel", "platform"] {
        if let Some(value) = params.get(key).and_then(|v| v.as_str()) {
            let Some(channel) = normalize_directory_channel(value) else {
                return Err((INVALID_PARAMS, format!("unsupported channel: {value}")));
            };
            if !channels.contains(&channel) {
                channels.push(channel);
            }
        }
    }

    if channels.is_empty() {
        return Ok(DIRECTORY_SUPPORTED_CHANNELS
            .iter()
            .map(|v| (*v).to_owned())
            .collect());
    }

    Ok(channels)
}

fn parse_directory_query(params: &Value) -> Option<String> {
    let query = params.get("query").and_then(|v| v.as_str())?.trim();
    if query.is_empty() {
        None
    } else {
        Some(query.to_ascii_lowercase())
    }
}

fn parse_directory_limit(params: &Value, default_limit: usize) -> usize {
    params
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v.clamp(1, 500) as usize)
        .unwrap_or(default_limit)
}

fn directory_query_match(query: Option<&str>, fields: &[&str]) -> bool {
    let Some(query) = query else {
        return true;
    };
    fields
        .iter()
        .any(|value| value.to_ascii_lowercase().contains(query))
}

fn session_platform(entry: &SessionEntry) -> Option<String> {
    let channel = entry
        .channel
        .as_deref()
        .or(entry.last_channel.as_deref())
        .or(entry.from.as_deref())?;
    if let Some((platform, _)) = channel.split_once(':') {
        let trimmed = platform.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_ascii_lowercase())
        }
    } else {
        let trimmed = channel.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_ascii_lowercase())
        }
    }
}

fn session_channel_id(entry: &SessionEntry) -> Option<String> {
    let channel = entry.channel.as_deref().or(entry.last_channel.as_deref())?;
    if let Some((_, id)) = channel.split_once(':') {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    } else {
        None
    }
}

fn session_group_id(entry: &SessionEntry) -> Option<String> {
    if let Some(group_id) = entry
        .group_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(group_id.to_owned());
    }

    if matches!(entry.chat_type.as_deref(), Some("group" | "channel")) {
        return session_channel_id(entry);
    }
    None
}

fn latest_provenance(entry: &SessionEntry) -> Option<&crate::session::SessionMessageProvenance> {
    entry.provenance.iter().max_by_key(|item| item.timestamp)
}

fn session_peer_id(entry: &SessionEntry) -> Option<String> {
    entry
        .to
        .as_deref()
        .or(entry.last_to.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            latest_provenance(entry)
                .map(|item| item.user_id.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            entry
                .identity
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

fn session_display_name(entry: &SessionEntry) -> Option<String> {
    entry
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            latest_provenance(entry)
                .map(|item| item.name.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

fn channel_accounts_path(channel: &GatewayChannel) -> std::path::PathBuf {
    channel
        .config()
        .savfox_home
        .join("gateway")
        .join("channel-accounts.json")
}

async fn load_channel_accounts(channel: &GatewayChannel) -> Value {
    let path = channel_accounts_path(channel);
    let content = tokio::fs::read_to_string(path)
        .await
        .unwrap_or_else(|_| "{}".to_owned());
    serde_json::from_str::<Value>(&content)
        .ok()
        .filter(|value| value.is_object())
        .unwrap_or_else(|| json!({}))
}

fn directory_channel_configured(
    channel: &str,
    runtime: &crate::channel::RuntimeBridgeSecrets,
    saved_configs: &[savfox_core::config::channel_store::ChannelConfig],
) -> bool {
    runtime_channel_configured(channel, runtime)
        || saved_channel_state(saved_configs, channel).ready
}

pub(crate) async fn handle_directory_self(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let channels = parse_directory_channels(params)?;
    let runtime = channel.runtime_channel_secrets().await;
    let saved_configs = load_saved_channel_configs(channel).await;
    let account_doc = load_channel_accounts(channel).await;
    let sessions = session_store.list().await;

    let mut accounts = Vec::new();

    for channel in &channels {
        let configured = directory_channel_configured(channel, &runtime, &saved_configs);
        let mut seen_accounts = HashSet::new();

        if let Some(account_map) = account_doc
            .get(channel)
            .and_then(|v| v.get("accounts"))
            .and_then(|v| v.as_object())
        {
            for (account_id, details) in account_map {
                let account_id = account_id.trim();
                if account_id.is_empty() {
                    continue;
                }
                let enabled = details
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if seen_accounts.insert(account_id.to_owned()) {
                    accounts.push(json!({
                        "channel": channel,
                        "account_id": account_id,
                        "configured": configured,
                        "enabled": enabled,
                        "source": "channel-accounts",
                    }));
                }
            }
        }

        for entry in &sessions {
            if session_platform(entry).as_deref() != Some(channel.as_str()) {
                continue;
            }
            let Some(account_id) = entry
                .account_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if seen_accounts.insert(account_id.to_owned()) {
                accounts.push(json!({
                    "channel": channel,
                    "account_id": account_id,
                    "configured": configured,
                    "enabled": true,
                    "source": "sessions",
                }));
            }
        }

        if seen_accounts.is_empty() {
            let fallback_account = match channel.as_str() {
                "whatsapp" => std::env::var("WHATSAPP_PHONE_NUMBER_ID")
                    .unwrap_or_else(|_| "default".to_owned()),
                _ => "default".to_owned(),
            };
            accounts.push(json!({
                "channel": channel,
                "account_id": fallback_account,
                "configured": configured,
                "enabled": configured,
                "source": "runtime",
            }));
        }
    }

    Ok(json!({
        "channels": channels,
        "accounts": accounts,
    }))
}

pub(crate) async fn handle_directory_peers_list(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let channels = parse_directory_channels(params)?;
    let channel_set: HashSet<String> = channels.iter().cloned().collect();
    let query = parse_directory_query(params);
    let limit = parse_directory_limit(params, 50);

    let mut entries = session_store.list().await;
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));

    let mut seen = HashSet::new();
    let mut peers = Vec::new();

    for entry in entries {
        let Some(channel) = session_platform(&entry) else {
            continue;
        };
        if !channel_set.contains(&channel) {
            continue;
        }

        let Some(peer_id) = session_peer_id(&entry) else {
            continue;
        };
        let name = session_display_name(&entry).unwrap_or_else(|| peer_id.clone());
        let identity = entry.identity.clone().unwrap_or_default();
        let chat_type = entry.chat_type.clone().unwrap_or_else(|| "dm".to_owned());

        if !directory_query_match(
            query.as_deref(),
            &[&channel, &peer_id, &name, &identity, &chat_type],
        ) {
            continue;
        }

        let dedupe_key = format!("{channel}:{peer_id}");
        if !seen.insert(dedupe_key) {
            continue;
        }

        peers.push(json!({
            "channel": channel,
            "peer_id": peer_id,
            "name": name,
            "identity": entry.identity,
            "chat_type": chat_type,
            "group_id": entry.group_id,
            "last_seen_ms": entry.updated_at,
            "session_id": entry.session_id,
        }));

        if peers.len() >= limit {
            break;
        }
    }

    Ok(json!({
        "channels": channels,
        "query": query,
        "limit": limit,
        "peers": peers,
    }))
}

#[derive(Debug, Clone, Default)]
struct DirectoryGroupAccumulator {
    channel: String,
    group_id: String,
    name: String,
    topic: Option<String>,
    members: HashSet<String>,
    sessions: u64,
    last_seen_ms: u64,
}

pub(crate) async fn handle_directory_groups_list(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let channels = parse_directory_channels(params)?;
    let channel_set: HashSet<String> = channels.iter().cloned().collect();
    let query = parse_directory_query(params);
    let limit = parse_directory_limit(params, 50);

    let mut grouped: HashMap<String, DirectoryGroupAccumulator> = HashMap::new();
    for entry in session_store.list().await {
        let Some(channel) = session_platform(&entry) else {
            continue;
        };
        if !channel_set.contains(&channel) {
            continue;
        }

        let Some(group_id) = session_group_id(&entry) else {
            continue;
        };
        let group_name_candidate = entry
            .subject
            .clone()
            .or(entry.group_channel.clone())
            .or(entry.label.clone())
            .filter(|value| !value.trim().is_empty());
        let key = format!("{channel}:{group_id}");
        let accumulator = grouped
            .entry(key)
            .or_insert_with(|| DirectoryGroupAccumulator {
                channel: channel.clone(),
                group_id: group_id.clone(),
                name: group_name_candidate
                    .clone()
                    .unwrap_or_else(|| group_id.clone()),
                topic: group_name_candidate.clone(),
                members: HashSet::new(),
                sessions: 0,
                last_seen_ms: entry.updated_at,
            });

        accumulator.sessions = accumulator.sessions.saturating_add(1);
        accumulator.last_seen_ms = accumulator.last_seen_ms.max(entry.updated_at);
        if (accumulator.name == accumulator.group_id || accumulator.name.trim().is_empty())
            && group_name_candidate.is_some()
        {
            accumulator.name = group_name_candidate.clone().unwrap_or_default();
        }
        if accumulator
            .topic
            .as_deref()
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
            && group_name_candidate.is_some()
        {
            accumulator.topic = group_name_candidate.clone();
        }

        for provenance in &entry.provenance {
            if !provenance.user_id.trim().is_empty() {
                accumulator.members.insert(provenance.user_id.clone());
            }
        }
        if let Some(peer_id) = session_peer_id(&entry) {
            accumulator.members.insert(peer_id);
        }
    }

    let mut groups = grouped
        .into_values()
        .filter(|group| {
            directory_query_match(
                query.as_deref(),
                &[&group.channel, &group.group_id, &group.name],
            )
        })
        .map(|group| {
            json!({
                "channel": group.channel,
                "group_id": group.group_id,
                "name": group.name,
                "topic": group.topic,
                "members_estimate": group.members.len(),
                "sessions": group.sessions,
                "last_seen_ms": group.last_seen_ms,
            })
        })
        .collect::<Vec<_>>();

    groups.sort_by_key(|item| {
        std::cmp::Reverse(
            item.get("last_seen_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        )
    });
    groups.truncate(limit);

    Ok(json!({
        "channels": channels,
        "query": query,
        "limit": limit,
        "groups": groups,
    }))
}

#[derive(Debug, Clone, Default)]
struct DirectoryMemberAccumulator {
    channel: String,
    user_id: String,
    name: String,
    sessions: u64,
    last_seen_ms: u64,
}

pub(crate) async fn handle_directory_groups_members(
    params: &Value,
    session_store: &Arc<SessionStore>,
) -> RpcResult {
    let group_id = params
        .get("group_id")
        .or_else(|| params.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    if group_id.is_empty() {
        return Err((INVALID_PARAMS, "missing 'group_id' parameter".to_owned()));
    }

    let channels = parse_directory_channels(params)?;
    let channel_set: HashSet<String> = channels.iter().cloned().collect();
    let query = parse_directory_query(params);
    let limit = parse_directory_limit(params, 200);

    let mut members: HashMap<String, DirectoryMemberAccumulator> = HashMap::new();

    for entry in session_store.list().await {
        let Some(channel) = session_platform(&entry) else {
            continue;
        };
        if !channel_set.contains(&channel) {
            continue;
        }
        if session_group_id(&entry).as_deref() != Some(group_id.as_str()) {
            continue;
        }

        if entry.provenance.is_empty() {
            if let Some(peer_id) = session_peer_id(&entry) {
                let name = session_display_name(&entry).unwrap_or_else(|| peer_id.clone());
                let key = format!("{channel}:{peer_id}");
                let member = members
                    .entry(key)
                    .or_insert_with(|| DirectoryMemberAccumulator {
                        channel: channel.clone(),
                        user_id: peer_id.clone(),
                        name: name.clone(),
                        sessions: 0,
                        last_seen_ms: entry.updated_at,
                    });
                member.sessions = member.sessions.saturating_add(1);
                member.last_seen_ms = member.last_seen_ms.max(entry.updated_at);
                if member.name.trim().is_empty() && !name.trim().is_empty() {
                    member.name = name;
                }
            }
            continue;
        }

        for provenance in &entry.provenance {
            let user_id = provenance.user_id.trim();
            if user_id.is_empty() {
                continue;
            }
            let name = provenance.name.trim();
            let key = format!("{channel}:{user_id}");
            let member = members
                .entry(key)
                .or_insert_with(|| DirectoryMemberAccumulator {
                    channel: channel.clone(),
                    user_id: user_id.to_owned(),
                    name: if name.is_empty() {
                        user_id.to_owned()
                    } else {
                        name.to_owned()
                    },
                    sessions: 0,
                    last_seen_ms: entry.updated_at.max(provenance.timestamp),
                });

            member.sessions = member.sessions.saturating_add(1);
            member.last_seen_ms = member
                .last_seen_ms
                .max(entry.updated_at)
                .max(provenance.timestamp);
            if member.name == member.user_id && !name.is_empty() {
                member.name = name.to_owned();
            }
        }
    }

    let mut list = members
        .into_values()
        .filter(|member| {
            directory_query_match(
                query.as_deref(),
                &[&member.channel, &member.user_id, &member.name],
            )
        })
        .map(|member| {
            json!({
                "channel": member.channel,
                "user_id": member.user_id,
                "name": member.name,
                "sessions": member.sessions,
                "last_seen_ms": member.last_seen_ms,
            })
        })
        .collect::<Vec<_>>();

    list.sort_by_key(|item| {
        std::cmp::Reverse(
            item.get("last_seen_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        )
    });
    list.truncate(limit);

    Ok(json!({
        "group_id": group_id,
        "channels": channels,
        "query": query,
        "limit": limit,
        "members": list,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::saved_channel_config_ready;

    fn channel_config(
        kind: &str,
        config: serde_json::Value,
    ) -> savfox_core::config::channel_store::ChannelConfig {
        savfox_core::config::channel_store::ChannelConfig {
            id: format!("{kind}-default"),
            kind: kind.to_string(),
            slug: String::new(),
            name: kind.to_string(),
            enabled: true,
            config,
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn line_ready_accepts_channel_access_token_alias() {
        let config = channel_config(
            "line",
            json!({
                "channel_access_token": "token-123",
                "channel_secret": "secret-123"
            }),
        );

        assert!(saved_channel_config_ready(&config));
    }

    #[test]
    fn googlechat_ready_requires_webhook_url() {
        let ready = channel_config(
            "googlechat",
            json!({
                "webhook_url": "https://chat.googleapis.com/v1/spaces/AAAA/messages"
            }),
        );
        let not_ready = channel_config(
            "googlechat",
            json!({
                "space_id": "spaces/AAAA"
            }),
        );

        assert!(saved_channel_config_ready(&ready));
        assert!(!saved_channel_config_ready(&not_ready));
    }

    #[test]
    fn qq_and_wechat_ready_require_webhook_url() {
        let qq = channel_config(
            "qq",
            json!({
                "webhook_url": "https://bridge.example.com/qq/send"
            }),
        );
        let wechat = channel_config(
            "wechat",
            json!({
                "webhook_url": "https://bridge.example.com/wechat/send"
            }),
        );

        assert!(saved_channel_config_ready(&qq));
        assert!(saved_channel_config_ready(&wechat));
    }
}
