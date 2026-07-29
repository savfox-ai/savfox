use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[cfg(feature = "arkret")]
use reqwest::header::CONTENT_TYPE;
use serde_json::{Value, json};
#[cfg(feature = "arkret")]
use url::Url;

use super::super::types::{INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, RpcResult};
use super::channel_management::{load_nostr_profile, save_nostr_profile};
use crate::channel::GatewayChannel;
use crate::session::{SessionEntry, SessionStore};

// ── Send / Wake / Channels ──────────────────────────────────────────────────

pub(crate) async fn handle_send(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let channel_id = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let thread_id = params
        .get("thread_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let reply_target = params
        .get("reply_target")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let saved_channel_config_id = requested_channel_instance_id(params);

    if channel_id.is_empty() || text.is_empty() {
        return Err((
            INVALID_REQUEST,
            "missing 'channel' or 'text' parameter".to_owned(),
        ));
    }

    match channel
        .send_platform_message_with_context(
            channel_id,
            text,
            None,
            None,
            None,
            thread_id,
            reply_target,
            saved_channel_config_id,
        )
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
        json!({"platform": "arkret", "endpoint": "/_arkret/edge/applet/transactions", "type": "channel"}),
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

fn requested_channel_instance_id(params: &Value) -> Option<&str> {
    params
        .get("id")
        .or_else(|| params.get("config_id"))
        .or_else(|| params.get("configId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn request_matches_channel_instance(
    params: &Value,
    config: &savfox_core::config::channel_store::ChannelConfig,
) -> bool {
    requested_channel_instance_id(params).is_none_or(|requested_id| config.id == requested_id)
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

fn arkret_mode_from_config_obj(map: &serde_json::Map<String, Value>) -> String {
    match first_non_empty_channel_config_string(map, &["mode"]).as_deref() {
        Some("applet") => "applet".to_owned(),
        Some("agent") => "agent".to_owned(),
        Some(_) | None => "invalid".to_owned(),
    }
}

fn arkret_namespace_count(map: &serde_json::Map<String, Value>) -> Option<u32> {
    let namespaces = map.get("namespaces").and_then(Value::as_object)?;
    let count = ["actors", "realms", "handles"]
        .into_iter()
        .filter_map(|key| namespaces.get(key).and_then(Value::as_array))
        .map(|items| items.len() as u32)
        .sum::<u32>();
    (count > 0).then_some(count)
}

#[cfg(feature = "arkret")]
fn insert_arkret_listener_summary(
    info: &mut serde_json::Map<String, Value>,
    diagnostics: Vec<Value>,
) {
    let ready = diagnostics.iter().any(|diagnostic| {
        diagnostic
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && diagnostic
                .get("phase")
                .and_then(Value::as_str)
                .is_some_and(|phase| matches!(phase, "subscribing" | "dispatching"))
    });
    let phase = if ready {
        diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.get("phase").and_then(Value::as_str))
            .find(|phase| matches!(*phase, "dispatching" | "subscribing"))
            .unwrap_or("subscribing")
    } else {
        diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.get("phase").and_then(Value::as_str))
            .find(|phase| *phase == "retry_wait")
            .or_else(|| {
                diagnostics
                    .iter()
                    .filter_map(|diagnostic| diagnostic.get("phase").and_then(Value::as_str))
                    .next()
            })
            .unwrap_or("stopped")
    };
    let last_error = diagnostics.iter().find_map(|diagnostic| {
        diagnostic
            .get("last_error")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|error| !error.is_empty())
    });

    info.insert("runtime_ready".to_owned(), json!(ready));
    info.insert("runtime_phase".to_owned(), json!(phase));
    if let Some(error) = last_error {
        info.insert("lastError".to_owned(), json!(error));
        info.insert("last_error".to_owned(), json!(error));
    }
    info.insert("listener_diagnostics".to_owned(), json!(diagnostics));
}

fn arkret_saved_config_running(config: &savfox_core::config::channel_store::ChannelConfig) -> bool {
    #[cfg(not(feature = "arkret"))]
    {
        let _ = config;
        false
    }
    #[cfg(feature = "arkret")]
    {
        let Some(raw) = config.config.as_object() else {
            return false;
        };
        if arkret_mode_from_config_obj(raw) == "applet" {
            crate::channels::arkret_applet::is_arkret_applet_registered(&config.id)
        } else {
            crate::channels::arkret::arkret_account_listener_count(&config.id) > 0
        }
    }
}

fn arkret_saved_config_connected(
    config: &savfox_core::config::channel_store::ChannelConfig,
) -> bool {
    #[cfg(not(feature = "arkret"))]
    {
        let _ = config;
        false
    }
    #[cfg(feature = "arkret")]
    {
        let Some(raw) = config.config.as_object() else {
            return false;
        };
        if arkret_mode_from_config_obj(raw) == "applet" {
            crate::channels::arkret_applet::is_arkret_applet_registered(&config.id)
        } else {
            crate::channels::arkret::arkret_account_runtime_diagnostics(&config.id)
                .iter()
                .any(|diagnostic| {
                    diagnostic
                        .get("running")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        && diagnostic
                            .get("phase")
                            .and_then(Value::as_str)
                            .is_some_and(|phase| matches!(phase, "subscribing" | "dispatching"))
                })
        }
    }
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
        "arkret" => {
            #[cfg(not(feature = "arkret"))]
            {
                false
            }
            #[cfg(feature = "arkret")]
            {
                if arkret_mode_from_config_obj(raw) == "applet" {
                    savfox_channels::arkret::applet::ArkretAppletConfig::from_channel_config(config)
                        .is_some_and(|parsed| parsed.validate().is_ok())
                } else {
                    savfox_channels::arkret::ArkretChannelConfig::from_channel_config(config)
                        .is_some_and(|parsed| parsed.validate().is_ok())
                }
            }
        }
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
        "arkret" => {
            let mode = arkret_mode_from_config_obj(config_obj);
            info.insert("mode".to_owned(), json!(&mode));
            if let Some(base_url) =
                first_non_empty_channel_config_string(config_obj, &["baseUrl", "base_url", "url"])
            {
                info.insert("base_url".to_owned(), json!(base_url));
            }
            if let Some(service_id) =
                first_non_empty_channel_config_string(config_obj, &["serviceId", "service_id"])
            {
                info.insert("service_id".to_owned(), json!(service_id));
            }
            #[cfg(feature = "arkret")]
            if let Some(config) = saved_state.config.as_ref() {
                if mode == "applet" {
                    if let Some(parsed) =
                        savfox_channels::arkret::applet::ArkretAppletConfig::from_channel_config(
                            config,
                        )
                    {
                        info.insert("applet_id".to_owned(), json!(&parsed.applet_id));
                        info.insert("bot_actor_id".to_owned(), json!(&parsed.bot_actor_id));
                        info.insert("base_url".to_owned(), json!(&parsed.base_url));
                        info.insert("service_id".to_owned(), json!(&parsed.service_id));
                        info.insert("protocol_count".to_owned(), json!(parsed.protocols.len()));
                        let namespace_count = parsed.namespaces.actors.len()
                            + parsed.namespaces.realms.len()
                            + parsed.namespaces.handles.len();
                        info.insert("namespace_count".to_owned(), json!(namespace_count));
                    } else {
                        if let Some(applet_id) = first_non_empty_channel_config_string(
                            config_obj,
                            &["appletId", "applet_id"],
                        ) {
                            info.insert("applet_id".to_owned(), json!(applet_id));
                        }
                        if let Some(bot_actor_id) = first_non_empty_channel_config_string(
                            config_obj,
                            &["botActorId", "bot_actor_id"],
                        ) {
                            info.insert("bot_actor_id".to_owned(), json!(bot_actor_id));
                        }
                        if let Some(protocol_count) =
                            channel_config_collection_len(config_obj, &["protocols"])
                        {
                            info.insert("protocol_count".to_owned(), json!(protocol_count));
                        }
                        if let Some(namespace_count) = arkret_namespace_count(config_obj) {
                            info.insert("namespace_count".to_owned(), json!(namespace_count));
                        }
                    }
                } else if let Some(parsed) =
                    savfox_channels::arkret::ArkretChannelConfig::from_channel_config(config)
                {
                    info.insert("base_url".to_owned(), json!(&parsed.base_url));
                    if let Some(service_id) = parsed.service_id.as_deref() {
                        info.insert("service_id".to_owned(), json!(service_id));
                    }
                    if let Some(account) = parsed.accounts.first() {
                        info.insert("account_id".to_owned(), json!(&account.id));
                        info.insert("principal_id".to_owned(), json!(&account.principal_id));
                        if let Some(verification_method) = account.verification_method.as_deref() {
                            info.insert(
                                "verification_method".to_owned(),
                                json!(verification_method),
                            );
                        }
                        if let Some(authorized_event_ref) = account.authorized_event_ref.as_deref()
                        {
                            info.insert(
                                "authorized_event_ref".to_owned(),
                                json!(authorized_event_ref),
                            );
                        }
                        let runtime_pairing_state = if account
                            .authorized_event_ref
                            .as_deref()
                            .is_some_and(|value| !value.trim().is_empty())
                            && account
                                .verification_method
                                .as_deref()
                                .is_some_and(|value| !value.trim().is_empty())
                        {
                            "paired"
                        } else if account.key_ref.is_some() {
                            "pending_authorization"
                        } else {
                            "pending_runtime_key"
                        };
                        info.insert(
                            "runtime_pairing_state".to_owned(),
                            json!(runtime_pairing_state),
                        );
                        let runtime_scope_count = if account.requested_scope.is_empty() {
                            channel_config_collection_len(
                                config_obj,
                                &["requestedScope", "requested_scope"],
                            )
                            .unwrap_or(0) as usize
                        } else {
                            account.requested_scope.len()
                        };
                        info.insert("runtime_scope_count".to_owned(), json!(runtime_scope_count));
                    }
                    insert_arkret_listener_summary(
                        info,
                        crate::channels::arkret::arkret_account_runtime_diagnostics(&config.id),
                    );
                }
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
            false
        }
    } else {
        false
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
    let matrix_pending_invites =
        crate::channels::matrix::MatrixInviteStore::for_savfox_home(&channel.config().savfox_home)
            .list(false)
            .await
            .ok()
            .map(|invites| invites.len() as u32);
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
    let arkret_configured =
        channel_is_configured("arkret", &runtime, &saved_configs, nostr_configured);
    let arkret_running = saved_configs.iter().any(|config| {
        channel_platform_matches_kind(&config.kind, "arkret") && arkret_saved_config_running(config)
    });
    let arkret_connected = saved_configs.iter().any(|config| {
        channel_platform_matches_kind(&config.kind, "arkret")
            && arkret_saved_config_connected(config)
    });
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
    let dingtalk_running = if let Some(config) = dingtalk_saved.config.as_ref() {
        saved_channel_stream_enabled(config)
            && savfox_channels::dingtalk::is_dingtalk_stream_running(&config.id).await
    } else {
        false
    };
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
            "connected": false,
        },
        "matrix": {
            "configured": matrix_configured,
            "running": matrix_running,
            "connected": matrix_connected,
            "pending_invites": matrix_pending_invites,
        },
        "arkret": {
            "configured": arkret_configured,
            "running": arkret_running,
            "connected": arkret_connected,
        },
        "whatsapp": {
            "configured": whatsapp_configured,
            "running": whatsapp_configured,
            "connected": false,
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
            "connected": false,
        },
        "googlechat": {
            "configured": googlechat_configured,
            "running": googlechat_configured,
            "connected": false,
        },
        "webhook": {
            "configured": webhook_configured,
            "running": webhook_configured,
            "connected": false,
        },
        "irc": {
            "configured": irc_configured,
            "running": false,
            "connected": false,
        },
        "line": {
            "configured": line_configured,
            "running": line_configured,
            "connected": false,
        },
        "qq": {
            "configured": qq_configured,
            "running": qq_configured,
            "connected": false,
        },
        "wechat": {
            "configured": wechat_configured,
            "running": wechat_configured,
            "connected": false,
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
            if let Some(pending_invites) = matrix_runtime.pending_invites.or(matrix_pending_invites)
            {
                matrix_entry.insert("pending_invites".to_owned(), json!(pending_invites));
            }
            if let Some(auto_join) = matrix_runtime.auto_join.as_deref() {
                matrix_entry.insert("auto_join".to_owned(), json!(auto_join));
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

    let registry_started_channel_ids = {
        let registry = channel.channel_registry();
        registry
            .read()
            .await
            .keys()
            .cloned()
            .collect::<HashSet<_>>()
    };
    let recovery_reports = channel.channel_recovery_registry().read().await.clone();
    let dingtalk_runtime_states = savfox_channels::dingtalk::dingtalk_stream_state_snapshot();
    let mut instances = serde_json::Map::new();
    for saved in &saved_configs {
        let platform = canonical_channel_platform(&saved.kind);
        let ready = saved_channel_config_ready(saved);
        let (running, connected) = match platform.as_str() {
            "discord" => {
                let running = savfox_channels::discord::is_discord_stream_running(&saved.id).await;
                let connected = discord_runtime_states
                    .get(&saved.id)
                    .map(|state| state.connected)
                    .unwrap_or(running);
                (running, connected)
            }
            "telegram" => {
                let running =
                    savfox_channels::telegram::is_telegram_polling_running(&saved.id).await;
                (running, running)
            }
            "matrix" => {
                let runtime = matrix_runtime_states.get(&saved.id);
                let running = runtime.is_some() || registry_started_channel_ids.contains(&saved.id);
                let connected = runtime.map(|state| state.connected).unwrap_or(running);
                (running, connected)
            }
            "arkret" => {
                let running = arkret_saved_config_running(saved);
                (running, arkret_saved_config_connected(saved))
            }
            "feishu" => {
                let running = savfox_channels::feishu::is_feishu_stream_running(&saved.id).await;
                (running, running)
            }
            "dingtalk" => (
                savfox_channels::dingtalk::is_dingtalk_stream_running(&saved.id).await,
                false,
            ),
            "webhook" | "slack" | "mattermost" | "googlechat" | "line" | "whatsapp" | "qq"
            | "wechat" => {
                let active = saved.enabled && ready;
                (active, false)
            }
            _ => (false, false),
        };
        let mut info = serde_json::Map::new();
        info.insert("platform".to_owned(), json!(&platform));
        info.insert("configured".to_owned(), json!(ready));
        info.insert("running".to_owned(), json!(running));
        info.insert("connected".to_owned(), json!(connected));
        if let Some(report) = recovery_reports.get(&saved.id)
            && let Ok(Value::Object(report_value)) = serde_json::to_value(report)
        {
            for (key, value) in report_value {
                let response_key = match key.as_str() {
                    "phase" => "recovery_phase",
                    "capability" => "runtime_capability",
                    "attempts" => "startup_attempts",
                    "updated_at" => "startup_updated_at",
                    other => other,
                };
                info.insert(response_key.to_owned(), value);
            }
        }
        if let Some(state) = dingtalk_runtime_states.get(&saved.id) {
            info.insert("runtime_phase".to_owned(), json!(state.phase));
            info.insert("runtime_attempts".to_owned(), json!(state.attempt_count));
            info.insert(
                "runtime_updated_at_ms".to_owned(),
                json!(state.updated_at_ms),
            );
            if let Some(error) = state.last_error.as_deref() {
                info.insert("last_error".to_owned(), json!(error));
                info.insert("lastError".to_owned(), json!(error));
                info.insert("recovery_phase".to_owned(), json!("failed"));
                info.insert("health_state".to_owned(), json!("degraded"));
            }
        }
        if let Some(platform_info) = channels.get(&platform).and_then(Value::as_object) {
            for key in [
                "last_message_time",
                "last_event_time",
                "reconnect_attempt_count",
                "probe_status",
                "connection_uptime_ms",
                "error_rate",
                "messages_total",
                "messages_failed",
                "last_activity",
                "last_probe_at",
                "probe",
                "lastMessageTime",
                "lastEventTime",
                "reconnectAttemptCount",
                "probeStatus",
                "uptimeMs",
                "errorRate",
                "lastProbeAt",
                "lastActivity",
            ] {
                if let Some(value) = platform_info.get(key) {
                    info.insert(key.to_owned(), value.clone());
                }
            }
        }
        let saved_state = SavedChannelState {
            exists: true,
            enabled: saved.enabled,
            ready,
            channel_name: Some(saved.name.clone()),
            channel_slug: Some(saved.slug.clone()),
            config: Some(saved.clone()),
        };
        insert_saved_channel_metadata(&mut info, &platform, &saved_state);
        if connected {
            info.insert("health_state".to_owned(), json!("connected"));
        } else if running
            && recovery_reports.get(&saved.id).is_some_and(|report| {
                report.capability == crate::channels::recovery::ChannelRuntimeCapability::Persistent
            })
        {
            info.insert("health_state".to_owned(), json!("listening"));
        }
        if platform == "arkret" {
            match info.get("runtime_phase").and_then(Value::as_str) {
                Some("migration_required") => {
                    info.insert("recovery_phase".to_owned(), json!("migration_required"));
                    info.insert("health_state".to_owned(), json!("migration_required"));
                }
                Some("retry_wait") => {
                    info.insert("recovery_phase".to_owned(), json!("retrying"));
                    info.insert("health_state".to_owned(), json!("degraded"));
                }
                Some("subscribing" | "dispatching") => {
                    info.insert("recovery_phase".to_owned(), json!("ready"));
                    info.insert("health_state".to_owned(), json!("connected"));
                }
                _ => {}
            }
            #[cfg(feature = "arkret")]
            {
                let mut enabled = saved.clone();
                enabled.enabled = true;
                if let Ok(parsed) =
                    savfox_channels::arkret::ArkretChannelConfig::from_strict_agent_config(&enabled)
                {
                    let account = &parsed.accounts[0];
                    info.insert(
                        "local_requested_scope".to_owned(),
                        json!(&account.requested_scope),
                    );
                    let missing = savfox_channels::arkret::missing_required_scope_actions(
                        &account.requested_scope,
                        account.listen,
                        account.send,
                    );
                    info.insert("missing_required_actions".to_owned(), json!(missing));
                    let runtime_public_key_digest = account
                        .key_ref
                        .as_ref()
                        .zip(account.verification_method.as_deref())
                        .and_then(|(key_ref, verification_method)| {
                            savfox_channels::arkret::ed25519_runtime_public_key_digest(
                                key_ref,
                                verification_method,
                            )
                            .ok()
                        });
                    let Some(runtime_public_key_digest) = runtime_public_key_digest else {
                        info.insert("authority_status".to_owned(), json!("error"));
                        info.insert(
                            "authority_error".to_owned(),
                            json!("failed to resolve the configured Arkret runtime key"),
                        );
                        instances.insert(saved.id.clone(), Value::Object(info));
                        continue;
                    };
                    match savfox_channels::arkret::load_verified_runtime_scope(
                        &channel.config().savfox_home,
                        &saved.id,
                        account,
                        &runtime_public_key_digest,
                    )
                    .await
                    {
                        Ok(Some(verified)) => {
                            info.insert("authority_status".to_owned(), json!("verified"));
                            info.insert(
                                "verified_authorization_scope".to_owned(),
                                json!(verified.actions),
                            );
                            info.insert(
                                "runtime_public_key_digest".to_owned(),
                                json!(runtime_public_key_digest),
                            );
                        }
                        Ok(None) => {
                            info.insert("authority_status".to_owned(), json!("pending_session"));
                        }
                        Err(error) => {
                            info.insert("authority_status".to_owned(), json!("error"));
                            info.insert("authority_error".to_owned(), json!(error.to_string()));
                        }
                    }
                }
            }
        }
        instances.insert(saved.id.clone(), Value::Object(info));
    }

    if let Some(arkret) = channels.get_mut("arkret").and_then(Value::as_object_mut) {
        let arkret_instances = instances
            .values()
            .filter(|instance| instance.get("platform").and_then(Value::as_str) == Some("arkret"))
            .collect::<Vec<_>>();
        let ready_count = arkret_instances
            .iter()
            .filter(|instance| {
                instance
                    .get("runtime_phase")
                    .and_then(Value::as_str)
                    .is_some_and(|phase| matches!(phase, "subscribing" | "dispatching"))
            })
            .count();
        let retrying_count = arkret_instances
            .iter()
            .filter(|instance| {
                instance.get("runtime_phase").and_then(Value::as_str) == Some("retry_wait")
            })
            .count();
        let migration_required_count = arkret_instances
            .iter()
            .filter(|instance| {
                instance.get("runtime_phase").and_then(Value::as_str) == Some("migration_required")
            })
            .count();
        let failed_count = arkret_instances.len().saturating_sub(
            ready_count
                .saturating_add(retrying_count)
                .saturating_add(migration_required_count),
        );
        arkret.insert("instance_count".to_owned(), json!(arkret_instances.len()));
        arkret.insert("ready_count".to_owned(), json!(ready_count));
        arkret.insert("retrying_count".to_owned(), json!(retrying_count));
        arkret.insert(
            "migration_required_count".to_owned(),
            json!(migration_required_count),
        );
        arkret.insert("failed_count".to_owned(), json!(failed_count));
        arkret.insert(
            "running".to_owned(),
            json!(ready_count + retrying_count > 0),
        );
        arkret.insert("connected".to_owned(), json!(ready_count > 0));
        arkret.insert(
            "health_state".to_owned(),
            json!(if ready_count > 0 {
                "connected"
            } else if migration_required_count > 0 {
                "migration_required"
            } else if retrying_count > 0 {
                "degraded"
            } else {
                "stopped"
            }),
        );
        arkret.remove("last_error");
        arkret.remove("lastError");
    }

    let requested_channel = params
        .get("channel")
        .or_else(|| params.get("platform"))
        .and_then(|v| v.as_str())
        .map(canonical_channel_platform);
    if let Some(channel) = requested_channel.as_deref() {
        if let Some(requested_id) = requested_channel_instance_id(params) {
            let instance = instances.get(requested_id).ok_or_else(|| {
                (
                    INVALID_REQUEST,
                    format!("unknown {channel} channel instance: {requested_id}"),
                )
            })?;
            if instance.get("platform").and_then(Value::as_str) != Some(channel) {
                return Err((
                    INVALID_REQUEST,
                    format!("channel instance '{requested_id}' is not a {channel} instance"),
                ));
            }
            return Ok(instance.clone());
        }
        if let Some(entry) = channels.get(channel) {
            let mut payload = entry.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("platform".to_owned(), Value::String(channel.to_owned()));
            }
            return Ok(payload);
        }
        return Err((INVALID_REQUEST, format!("unknown channel: {channel}")));
    }

    Ok(json!({
        "channels": channels,
        "instances": instances,
    }))
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
    let is_configured = if requested_channel_instance_id(params).is_some() {
        saved_configs.iter().any(|config| {
            channel_platform_matches_kind(&config.kind, &platform)
                && request_matches_channel_instance(params, config)
                && config.enabled
                && saved_channel_config_ready(config)
        })
    } else {
        channel_is_configured(&platform, &runtime, &saved_configs, nostr_configured)
    };

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
            if !channel_platform_matches_kind(&saved.kind, "discord")
                || !request_matches_channel_instance(params, saved)
                || !saved.enabled
            {
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
            crate::channels::note_channel_started(saved, channel, session_store).await;
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
                || !request_matches_channel_instance(params, saved)
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
            crate::channels::note_channel_started(saved, channel, session_store).await;
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

    if platform == "arkret" {
        if !is_configured {
            return Ok(json!({
                "platform": platform,
                "status": "needs_config",
                "configured": false,
                "message": "Please configure arkret in the channel settings",
            }));
        }

        #[cfg(not(feature = "arkret"))]
        {
            return Err((
                INVALID_REQUEST,
                "Arkret support is not enabled in this build".to_owned(),
            ));
        }
        #[cfg(feature = "arkret")]
        {
            let registry = channel.channel_registry();
            let mut started = 0_u32;
            let mut already_running = 0_u32;
            let mut ready_configs = 0_u32;
            let mut applet_configs = 0_u32;
            let mut account_configs = 0_u32;

            for saved in &saved_configs {
                if !channel_platform_matches_kind(&saved.kind, "arkret")
                    || !request_matches_channel_instance(params, saved)
                    || !saved.enabled
                    || !saved_channel_config_ready(saved)
                {
                    continue;
                }

                ready_configs = ready_configs.saturating_add(1);
                let mode = saved
                    .config
                    .as_object()
                    .map(arkret_mode_from_config_obj)
                    .unwrap_or_else(|| "account".to_owned());

                if mode == "applet" {
                    applet_configs = applet_configs.saturating_add(1);
                    if crate::channels::arkret_applet::is_arkret_applet_registered(&saved.id) {
                        already_running = already_running.saturating_add(1);
                        continue;
                    }
                    crate::channels::arkret_applet::start_arkret_applet_channel(
                        saved,
                        channel,
                        session_store,
                    )
                    .await
                    .map_err(|err| {
                        (
                            INTERNAL_ERROR,
                            format!(
                                "failed to start Arkret applet channel '{}': {err}",
                                saved.id
                            ),
                        )
                    })?;
                } else {
                    account_configs = account_configs.saturating_add(1);
                    if crate::channels::arkret::arkret_account_listener_count(&saved.id) > 0 {
                        already_running = already_running.saturating_add(1);
                        continue;
                    }
                    crate::channels::arkret::start_arkret_channel(
                        saved,
                        &registry,
                        channel,
                        session_store,
                    )
                    .await
                    .map_err(|err| {
                        (
                            INTERNAL_ERROR,
                            format!(
                                "failed to start Arkret account channel '{}': {err}",
                                saved.id
                            ),
                        )
                    })?;
                }

                crate::channels::note_channel_started(saved, channel, session_store).await;
                started = started.saturating_add(1);
            }

            let (status, message) = if started > 0 {
                ("started", format!("Started {started} Arkret channel(s)"))
            } else if already_running > 0 {
                (
                    "already_running",
                    format!("{already_running} Arkret channel(s) already running"),
                )
            } else if ready_configs > 0 {
                (
                    "configured",
                    format!(
                        "Arkret is configured (account: {account_configs}, applet: {applet_configs}), but no runtime was started"
                    ),
                )
            } else {
                (
                    "needs_config",
                    "No enabled Arkret channel has a valid account or applet config".to_owned(),
                )
            };

            return Ok(json!({
                "platform": platform,
                "status": status,
                "configured": true,
                "started": started,
                "already_running": already_running,
                "account_configs": account_configs,
                "applet_configs": applet_configs,
                "message": message,
            }));
        }
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
            if !channel_platform_matches_kind(&saved.kind, "feishu")
                || !request_matches_channel_instance(params, saved)
                || !saved.enabled
            {
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
                Some(saved.id.clone()),
            );
            savfox_channels::feishu::start_feishu_stream(&saved.id, &parsed, sink)
                .await
                .map_err(|err| {
                    (
                        INTERNAL_ERROR,
                        format!("failed to start Feishu stream '{}': {err}", saved.id),
                    )
                })?;
            crate::channels::note_channel_started(saved, channel, session_store).await;
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
            if !channel_platform_matches_kind(&saved.kind, "telegram")
                || !request_matches_channel_instance(params, saved)
                || !saved.enabled
            {
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
                saved.id.clone(),
            );
            savfox_channels::telegram::start_telegram_polling(&saved.id, &parsed, sink)
                .await
                .map_err(|err| {
                    (
                        INTERNAL_ERROR,
                        format!("failed to start Telegram polling '{}': {err}", saved.id),
                    )
                })?;
            crate::channels::note_channel_started(saved, channel, session_store).await;
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
    let instance_scoped = requested_channel_instance_id(params).is_some();
    match platform.as_str() {
        "discord" => {
            if !instance_scoped {
                secrets.discord_bot_token = None;
            }
            for saved in load_saved_channel_configs(channel).await {
                if channel_platform_matches_kind(&saved.kind, "discord")
                    && request_matches_channel_instance(params, &saved)
                    && savfox_channels::discord::stop_discord_stream(&saved.id).await
                {
                    stopped = stopped.saturating_add(1);
                }
            }
        }
        "telegram" => {
            if !instance_scoped {
                secrets.telegram_bot_token = None;
            }
            for saved in load_saved_channel_configs(channel).await {
                if channel_platform_matches_kind(&saved.kind, "telegram")
                    && request_matches_channel_instance(params, &saved)
                    && savfox_channels::telegram::stop_telegram_polling(&saved.id).await
                {
                    stopped = stopped.saturating_add(1);
                }
            }
        }
        "slack" => {
            if !instance_scoped {
                secrets.slack_bot_token = None;
                secrets.slack_signing_secret = None;
            }
        }
        "webhook" => {
            if !instance_scoped {
                secrets.webhook_secret = None;
            }
        }
        "nostr" => {
            let mut profile = load_nostr_profile(channel).await;
            profile["private_key"] = json!("");
            profile["public_key"] = json!("");
            let _ = save_nostr_profile(channel, &profile).await;
        }
        "feishu" => {
            for saved in load_saved_channel_configs(channel).await {
                if channel_platform_matches_kind(&saved.kind, "feishu")
                    && request_matches_channel_instance(params, &saved)
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
                if channel_platform_matches_kind(&saved.kind, "matrix")
                    && request_matches_channel_instance(params, &saved)
                {
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
        "dingtalk" => {
            for saved in load_saved_channel_configs(channel).await {
                if channel_platform_matches_kind(&saved.kind, "dingtalk")
                    && request_matches_channel_instance(params, &saved)
                    && savfox_channels::dingtalk::stop_dingtalk_stream(&saved.id).await
                {
                    stopped = stopped.saturating_add(1);
                }
            }
        }
        "arkret" => {
            #[cfg(feature = "arkret")]
            {
                for saved in load_saved_channel_configs(channel).await {
                    if !channel_platform_matches_kind(&saved.kind, "arkret")
                        || !request_matches_channel_instance(params, &saved)
                    {
                        continue;
                    }
                    // Account mode: abort long-poll listener tasks.
                    let listeners =
                        crate::channels::arkret::stop_arkret_account_listeners(&saved.id);
                    // Applet mode: drop the registry entry so a stale bearer/
                    // namespace can no longer match inbound transactions.
                    let removed_applet =
                        crate::channels::arkret_applet::remove_arkret_applet_channel(&saved.id)
                            .unwrap_or(false);
                    if listeners > 0 || removed_applet {
                        stopped = stopped.saturating_add(1);
                    }
                }
            }
        }
        "whatsapp" | "signal" | "mattermost" | "googlechat" | "irc" | "line" | "zalo"
        | "nextcloud" | "twitch" | "tlon" | "qq" | "wechat" => {
            // These platforms may not have runtime secrets yet
        }
        _ => {
            return Err((INVALID_REQUEST, format!("unknown platform: {platform}")));
        }
    }
    channel.set_runtime_channel_secrets(secrets).await;
    let reports = channel.channel_recovery_registry();
    for saved in load_saved_channel_configs(channel).await {
        if channel_platform_matches_kind(&saved.kind, &platform)
            && request_matches_channel_instance(params, &saved)
            && crate::channels::recovery::runtime_capability(&saved)
                == crate::channels::recovery::ChannelRuntimeCapability::Persistent
        {
            // Login/logout handlers historically stop the transport directly.
            // Also use the common instance stop path so an explicit logout
            // cancels its recovery supervisor instead of being restarted.
            if crate::channels::stop_channel_instance(&saved, channel)
                .await
                .map_err(|error| {
                    (
                        INTERNAL_ERROR,
                        format!("failed to stop channel '{}': {error}", saved.id),
                    )
                })?
            {
                stopped = stopped.saturating_add(1);
            }
            crate::channels::recovery::mark_channel_stopped(&reports, &saved, false).await;
        }
    }

    Ok(json!({
        "platform": platform,
        "status": if matches!(platform.as_str(), "discord" | "telegram" | "dingtalk" | "feishu" | "matrix" | "arkret") && stopped > 0 {
            "stopped"
        } else if matches!(platform.as_str(), "discord" | "telegram" | "dingtalk" | "feishu" | "matrix" | "arkret") {
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
    let requested_id = params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let configured = if let Some(requested_id) = requested_id {
        saved_configs
            .iter()
            .find(|config| {
                config.id == requested_id && channel_platform_matches_kind(&config.kind, &platform)
            })
            .is_some_and(saved_channel_config_ready)
    } else {
        channel_is_configured(&platform, &runtime, &saved_configs, nostr_configured)
    };

    if platform == "matrix" {
        return handle_matrix_channel_test(params, channel, &saved_configs).await;
    }
    if platform == "arkret" {
        return handle_arkret_channel_test(params, &saved_configs).await;
    }

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

#[cfg(not(feature = "arkret"))]
pub(crate) async fn handle_channels_arkret_inspect(
    _params: &Value,
    _channel: &Arc<GatewayChannel>,
) -> RpcResult {
    Err((
        INVALID_REQUEST,
        "Arkret support is not enabled in this build".to_owned(),
    ))
}

#[cfg(feature = "arkret")]
pub(crate) async fn handle_channels_arkret_inspect(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let config_id = requested_channel_instance_id(params).ok_or_else(|| {
        (
            INVALID_REQUEST,
            "channels.arkret.inspect requires an exact channel instance id".to_owned(),
        )
    })?;
    let raw = savfox_core::config::channel_store::get_channel_config(
        &channel.config().savfox_home,
        config_id,
    )
    .await
    .map_err(|error| (INTERNAL_ERROR, error.to_string()))?
    .ok_or_else(|| {
        (
            INVALID_REQUEST,
            format!("Arkret channel instance '{config_id}' was not found"),
        )
    })?;
    if !raw.kind.eq_ignore_ascii_case("arkret") {
        return Err((
            INVALID_REQUEST,
            format!("channel instance '{config_id}' is not an Arkret config"),
        ));
    }
    let parsed = savfox_channels::arkret::ArkretChannelConfig::from_strict_agent_config(&raw)
        .map_err(|error| (INVALID_REQUEST, error.to_string()))?;
    let account = &parsed.accounts[0];
    let verification_method = account.verification_method.as_deref().ok_or_else(|| {
        (
            INVALID_REQUEST,
            "Arkret agent config is missing verificationMethod".to_owned(),
        )
    })?;
    let key_ref = account.key_ref.as_ref().ok_or_else(|| {
        (
            INVALID_REQUEST,
            "Arkret agent config is missing keyRef".to_owned(),
        )
    })?;
    let key_digest =
        savfox_channels::arkret::ed25519_runtime_public_key_digest(key_ref, verification_method)
            .map_err(|error| (INTERNAL_ERROR, error.to_string()))?;
    let diagnostics = crate::channels::arkret::arkret_account_runtime_diagnostics(config_id);
    let runtime_phase = diagnostics
        .iter()
        .find_map(|diagnostic| diagnostic.get("phase").and_then(Value::as_str))
        .unwrap_or("stopped");
    let reason_code = diagnostics
        .iter()
        .find_map(|diagnostic| diagnostic.get("last_reason_code").and_then(Value::as_str));
    let verified = savfox_channels::arkret::load_verified_runtime_scope(
        &channel.config().savfox_home,
        config_id,
        account,
        &key_digest,
    )
    .await
    .map_err(|error| (INTERNAL_ERROR, error.to_string()))?;
    let (authority_status, verified_scope, excess_scope) = if let Some(verified) = verified {
        let excess = account
            .requested_scope
            .iter()
            .filter(|action| !verified.actions.contains(action))
            .cloned()
            .collect::<Vec<_>>();
        ("verified", json!(verified.actions), json!(excess))
    } else {
        ("pending_session", Value::Null, Value::Null)
    };
    Ok(json!({
        "platform": "arkret",
        "instance_id": config_id,
        "agent_id": account.principal_id,
        "account_id": account.id,
        "runtime_public_key_digest": key_digest,
        "authorization_ref": account.authorized_event_ref,
        "local_requested_scope": account.requested_scope,
        "verified_authorization_scope": verified_scope,
        "excess_scope": excess_scope,
        "authority_status": authority_status,
        "runtime_phase": runtime_phase,
        "last_reason_code": reason_code,
        "listener_diagnostics": diagnostics,
    }))
}

fn arkret_test_channel_config(
    params: &Value,
    saved_configs: &[savfox_core::config::channel_store::ChannelConfig],
) -> Option<savfox_core::config::channel_store::ChannelConfig> {
    if let Some(config) = params.get("config").filter(|value| value.is_object()) {
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("arkret-test");
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(id);
        return Some(savfox_core::config::channel_store::ChannelConfig {
            id: id.to_owned(),
            kind: "arkret".to_owned(),
            slug: id.to_owned(),
            name: name.to_owned(),
            enabled: true,
            config: object_without_null_fields(config),
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        });
    }

    if let Some(requested_id) = params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return saved_configs
            .iter()
            .find(|config| {
                config.id == requested_id && channel_platform_matches_kind(&config.kind, "arkret")
            })
            .cloned();
    }

    saved_configs
        .iter()
        .find(|config| {
            channel_platform_matches_kind(&config.kind, "arkret")
                && config.enabled
                && saved_channel_config_ready(config)
        })
        .or_else(|| {
            saved_configs.iter().find(|config| {
                channel_platform_matches_kind(&config.kind, "arkret") && config.enabled
            })
        })
        .cloned()
}

fn object_without_null_fields(value: &Value) -> Value {
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    Value::Object(
        obj.iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| {
                let value = if value.is_object() {
                    object_without_null_fields(value)
                } else {
                    value.clone()
                };
                (key.clone(), value)
            })
            .collect(),
    )
}

#[cfg(not(feature = "arkret"))]
async fn handle_arkret_channel_test(
    _params: &Value,
    _saved_configs: &[savfox_core::config::channel_store::ChannelConfig],
) -> RpcResult {
    Err((
        INVALID_REQUEST,
        "Arkret support is not enabled in this build".to_owned(),
    ))
}

#[cfg(feature = "arkret")]
async fn handle_arkret_channel_test(
    params: &Value,
    saved_configs: &[savfox_core::config::channel_store::ChannelConfig],
) -> RpcResult {
    let Some(raw_config) = arkret_test_channel_config(params, saved_configs) else {
        return Ok(json!({
            "platform": "arkret",
            "ok": false,
            "message": "arkret is not configured. Please add configuration in the channel settings.",
        }));
    };
    let mode = raw_config
        .config
        .as_object()
        .map(arkret_mode_from_config_obj)
        .unwrap_or_else(|| "agent".to_owned());

    if mode == "applet" {
        let parsed =
            savfox_channels::arkret::applet::ArkretAppletConfig::from_channel_config(&raw_config)
                .ok_or_else(|| {
                (
                    INVALID_REQUEST,
                    "Arkret applet channel config must be an object with mode='applet'".to_owned(),
                )
            })?;
        parsed
            .validate()
            .map_err(|err| (INVALID_REQUEST, err.to_string()))?;
        let namespace_count = parsed.namespaces.actors.len()
            + parsed.namespaces.realms.len()
            + parsed.namespaces.handles.len();
        return Ok(json!({
            "platform": "arkret",
            "ok": true,
            "mode": "applet",
            "applet_id": parsed.applet_id.as_str(),
            "service_id": parsed.service_id.as_str(),
            "protocol_count": parsed.protocols.len(),
            "namespace_count": namespace_count,
            "message": "Arkret applet configuration is valid",
        }));
    }

    let parsed =
        savfox_channels::arkret::ArkretChannelConfig::from_strict_agent_config(&raw_config)
            .map_err(|error| (INVALID_REQUEST, error.to_string()))?;
    parsed
        .validate()
        .map_err(|err| (INVALID_REQUEST, err.to_string()))?;
    Ok(json!({
        "platform": "arkret",
        "ok": true,
        "mode": "agent",
        "base_url": parsed.base_url.as_str(),
        "service_id": parsed.service_id.as_deref(),
        "account_count": parsed.accounts.len(),
        "listener_count": parsed.accounts.iter().filter(|account| account.listen).count(),
        "message": "Arkret agent configuration is valid",
    }))
}

#[cfg(not(feature = "arkret"))]
pub(crate) async fn handle_channels_arkret_runtime_key_request(
    _params: &Value,
    _channel: &Arc<GatewayChannel>,
) -> RpcResult {
    Err((
        INVALID_REQUEST,
        "Arkret support is not enabled in this build".to_owned(),
    ))
}

#[cfg(feature = "arkret")]
pub(crate) async fn handle_channels_arkret_runtime_key_request(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let saved_configs = load_saved_channel_configs(channel).await;
    let Some(raw_config) = arkret_test_channel_config(params, &saved_configs) else {
        return Err((
            INVALID_REQUEST,
            "Arkret agent channel config is required".to_owned(),
        ));
    };
    let parsed =
        savfox_channels::arkret::ArkretChannelConfig::from_strict_agent_config(&raw_config)
            .map_err(|error| (INVALID_REQUEST, error.to_string()))?;
    let account_id = params
        .get("account_id")
        .or_else(|| params.get("accountId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let account = if let Some(account_id) = account_id {
        parsed
            .accounts
            .iter()
            .find(|account| account.id == account_id)
    } else {
        parsed.accounts.first()
    }
    .ok_or_else(|| {
        (
            INVALID_REQUEST,
            "Arkret agent channel config has no runtime account to pair".to_owned(),
        )
    })?;
    let request =
        savfox_channels::arkret::build_arkret_runtime_key_request_json(account, chrono::Utc::now())
            .map_err(|err| (INVALID_REQUEST, err.to_string()))?;
    let approval =
        submit_arkret_runtime_key_approval_request(channel.http_client(), account, request)
            .await
            .map_err(|err| (INVALID_REQUEST, err))?;
    Ok(json!({
        "platform": "arkret",
        "ok": true,
        "mode": "agent",
        "account_id": account.id.as_str(),
        "approval_request_id": approval
            .get("approval_request_id")
            .cloned()
            .unwrap_or(Value::Null),
        "status": approval.get("status").cloned().unwrap_or(Value::Null),
        "message": "Arkret runtime key approval request sent to Inkson",
    }))
}

#[cfg(feature = "arkret")]
async fn submit_arkret_runtime_key_approval_request(
    client: &reqwest::Client,
    account: &savfox_channels::arkret::ArkretAccountConfig,
    request: Value,
) -> Result<Value, String> {
    let bootstrap = account
        .inkson_bootstrap
        .as_ref()
        .ok_or_else(|| "Arkret agent account has no resolved Inkson bootstrap".to_owned())?;
    let mut body = request;
    let object = body
        .as_object_mut()
        .ok_or_else(|| "Arkret runtime key request must be a JSON object".to_owned())?;
    object.insert(
        "pairing_code".to_owned(),
        json!(bootstrap.pairing_code.clone()),
    );
    let typed: arkret::AgentRuntimeApprovalRequestBody = serde_json::from_value(body.clone())
        .map_err(|err| {
            format!("Arkret runtime approval request does not match the spec body: {err}")
        })?;
    let request_body = serde_json::to_vec(&typed)
        .map_err(|err| format!("serialize Arkret runtime approval request: {err}"))?;
    let endpoint = format!(
        "{}/_arkret/open/agent-pairing/runtime-key-requests",
        bootstrap.arkret_base_url.trim_end_matches('/')
    );
    let response = client
        .post(&endpoint)
        .header(CONTENT_TYPE, "application/json")
        .body(request_body)
        .send()
        .await
        .map_err(|err| format!("submit Arkret runtime approval request failed: {err}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("read Arkret runtime approval response failed: {err}"))?;
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&bytes);
        let detail = detail.chars().take(300).collect::<String>();
        return Err(format!(
            "Arkret runtime approval endpoint returned HTTP {status}: {detail}"
        ));
    }
    let value = serde_json::from_slice::<Value>(&bytes)
        .map_err(|err| format!("Arkret runtime approval endpoint returned invalid JSON: {err}"))?;
    let _typed: arkret::AgentRuntimeApprovalOutcome = serde_json::from_value(value.clone())
        .map_err(|err| {
            format!("Arkret runtime approval endpoint returned invalid outcome: {err}")
        })?;
    Ok(value)
}

#[cfg(not(feature = "arkret"))]
pub(crate) async fn handle_channels_arkret_runtime_key_request_status(
    _params: &Value,
    _channel: &Arc<GatewayChannel>,
) -> RpcResult {
    Err((
        INVALID_REQUEST,
        "Arkret support is not enabled in this build".to_owned(),
    ))
}

#[cfg(feature = "arkret")]
pub(crate) async fn handle_channels_arkret_runtime_key_request_status(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let saved_configs = load_saved_channel_configs(channel).await;
    let Some(raw_config) = arkret_test_channel_config(params, &saved_configs) else {
        return Err((
            INVALID_REQUEST,
            "Arkret agent channel config is required".to_owned(),
        ));
    };
    let parsed =
        savfox_channels::arkret::ArkretChannelConfig::from_strict_agent_config(&raw_config)
            .map_err(|error| (INVALID_REQUEST, error.to_string()))?;
    let account_id = params
        .get("account_id")
        .or_else(|| params.get("accountId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let account = if let Some(account_id) = account_id {
        parsed
            .accounts
            .iter()
            .find(|account| account.id == account_id)
    } else {
        parsed.accounts.first()
    }
    .ok_or_else(|| {
        (
            INVALID_REQUEST,
            "Arkret agent channel config has no runtime account to poll".to_owned(),
        )
    })?;
    let (request, local_public_key_digest) =
        savfox_channels::arkret::build_arkret_runtime_key_status_request_json(account)
            .map_err(|err| (INVALID_REQUEST, err.to_string()))?;
    let outcome = poll_arkret_runtime_key_status(channel.http_client(), account, request)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    // Two orthogonal axes (key-management.md §3.6.1): `status` is the lifecycle
    // intent (active/paused/deactivated); `runtime_state` is the derived runtime
    // readiness (pending_runtime_key/ready/replacing/pairing_expired).
    let status = outcome
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let runtime_state = outcome
        .get("runtime_state")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let authorized_public_key_digest = outcome
        .get("authorized_public_key_digest")
        .and_then(Value::as_str);
    let key_digest_matches =
        authorized_public_key_digest.map(|digest| digest == local_public_key_digest);
    let (approved, ready, paired_by_other_runtime) =
        arkret_runtime_key_status_flags(&status, key_digest_matches);
    let message = if ready {
        "Runtime key approved by Inkson; agent is active"
    } else if status == "paused" && approved {
        "Runtime key approved, but the agent is paused; resume it in Inkson before starting Savfox"
    } else if paired_by_other_runtime {
        "Pairing was completed by a different runtime key; this Savfox key was not authorized"
    } else if status == "deactivated" {
        "Agent was deactivated"
    } else {
        match runtime_state.as_str() {
            "pending_runtime_key" => "Waiting for Inkson approval",
            "pairing_expired" => "Pairing request expired before approval",
            _ => "Unknown pairing status",
        }
    };
    Ok(json!({
        "platform": "arkret",
        "ok": true,
        "mode": "agent",
        "account_id": account.id.as_str(),
        "status": status,
        "runtime_state": runtime_state,
        "approved": approved,
        "ready": ready,
        "key_digest_matches": key_digest_matches,
        "paired_by_other_runtime": paired_by_other_runtime,
        "authorized_event_ref": outcome.get("authorized_event_ref").cloned().unwrap_or(Value::Null),
        "message": message,
    }))
}

fn arkret_runtime_key_status_flags(
    status: &str,
    key_digest_matches: Option<bool>,
) -> (bool, bool, bool) {
    let approved = matches!(status, "active" | "paused") && key_digest_matches == Some(true);
    let ready = status == "active" && key_digest_matches == Some(true);
    let paired_by_other_runtime =
        matches!(status, "active" | "paused" | "deactivated") && key_digest_matches == Some(false);
    (approved, ready, paired_by_other_runtime)
}

#[cfg(feature = "arkret")]
async fn poll_arkret_runtime_key_status(
    client: &reqwest::Client,
    account: &savfox_channels::arkret::ArkretAccountConfig,
    request: Value,
) -> Result<Value, String> {
    let bootstrap = account
        .inkson_bootstrap
        .as_ref()
        .ok_or_else(|| "Arkret agent account has no resolved Inkson bootstrap".to_owned())?;
    let typed: arkret::AgentRuntimeApprovalStatusRequestBody = serde_json::from_value(request)
        .map_err(|err| {
            format!("Arkret runtime key status request does not match the spec body: {err}")
        })?;
    let request_body = serde_json::to_vec(&typed)
        .map_err(|err| format!("serialize Arkret runtime key status request: {err}"))?;
    let endpoint = format!(
        "{}/_arkret/open/agent-pairing/runtime-key-requests/status",
        bootstrap.arkret_base_url.trim_end_matches('/')
    );
    let response = client
        .post(&endpoint)
        .header(CONTENT_TYPE, "application/json")
        .body(request_body)
        .send()
        .await
        .map_err(|err| format!("poll Arkret runtime key status failed: {err}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("read Arkret runtime key status response failed: {err}"))?;
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&bytes);
        let detail = detail.chars().take(300).collect::<String>();
        return Err(format!(
            "Arkret runtime key status endpoint returned HTTP {status}: {detail}"
        ));
    }
    let value = serde_json::from_slice::<Value>(&bytes).map_err(|err| {
        format!("Arkret runtime key status endpoint returned invalid JSON: {err}")
    })?;
    let _typed: arkret::AgentRuntimeApprovalStatusOutcome = serde_json::from_value(value.clone())
        .map_err(|err| {
        format!("Arkret runtime key status endpoint returned invalid outcome: {err}")
    })?;
    Ok(value)
}

#[cfg(not(feature = "arkret"))]
pub(crate) async fn handle_channels_arkret_resolve_pairing_bootstrap(
    _params: &Value,
    _channel: &Arc<GatewayChannel>,
) -> RpcResult {
    Err((
        INVALID_REQUEST,
        "Arkret support is not enabled in this build".to_owned(),
    ))
}

#[cfg(feature = "arkret")]
pub(crate) async fn handle_channels_arkret_resolve_pairing_bootstrap(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let input = params
        .get("input")
        .or_else(|| params.get("pairing_link"))
        .or_else(|| params.get("pairingLink"))
        .or_else(|| params.get("pairing_token"))
        .or_else(|| params.get("pairingToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                "Arkret pairing link or token is required".to_owned(),
            )
        })?;
    if input.starts_with('{') {
        let value = serde_json::from_str::<Value>(input)
            .map_err(|err| (INVALID_REQUEST, format!("invalid bootstrap JSON: {err}")))?;
        let bootstrap =
            validate_arkret_pairing_bootstrap_value(value).map_err(|err| (INVALID_REQUEST, err))?;
        return Ok(json!({
            "platform": "arkret",
            "ok": true,
            "mode": "agent",
            "inkson_bootstrap": bootstrap,
            "message": "Arkret pairing bootstrap is already resolved",
        }));
    }

    let base_url = params
        .get("base_url")
        .or_else(|| params.get("baseUrl"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let (resolve_url, pairing_token) =
        arkret_pairing_resolve_target(input, base_url).map_err(|err| (INVALID_REQUEST, err))?;
    let bootstrap = fetch_arkret_pairing_bootstrap(
        channel.http_client(),
        resolve_url.as_str(),
        pairing_token.as_str(),
    )
    .await
    .map_err(|err| (INVALID_REQUEST, err))?;
    Ok(json!({
        "platform": "arkret",
        "ok": true,
        "mode": "agent",
        "inkson_bootstrap": bootstrap,
        "message": "Arkret pairing link resolved",
    }))
}

#[cfg(feature = "arkret")]
fn arkret_pairing_resolve_target(
    input: &str,
    base_url: Option<&str>,
) -> Result<(String, String), String> {
    let input = input.trim();
    if let Ok(mut url) = Url::parse(input) {
        let path = url.path().trim_end_matches('/');
        if path != "/_arkret/open/agent-pairing/resolve" {
            return Err(
                "Arkret pairing link must target /_arkret/open/agent-pairing/resolve".to_owned(),
            );
        }
        if url.query().is_some() {
            return Err("Arkret pairing token must be in the URL fragment, not query".to_owned());
        }
        let token = url
            .fragment()
            .and_then(arkret_pairing_token_from_fragment)
            .ok_or_else(|| "Arkret pairing link fragment must contain token".to_owned())?;
        if !is_arkret_pairing_token_shape(&token) {
            return Err("Arkret pairing token shape is invalid".to_owned());
        }
        url.set_fragment(None);
        return Ok((url.to_string(), token));
    }

    let token = input
        .strip_prefix("token=")
        .unwrap_or(input)
        .trim()
        .to_owned();
    if !is_arkret_pairing_token_shape(&token) {
        return Err(
            "Arkret pairing input must be a resolver link or a base64url pairing token".to_owned(),
        );
    }
    let base_url =
        base_url.ok_or_else(|| "Arkret Base URL is required for token-only input".to_owned())?;
    let resolve_url = format!(
        "{}/_arkret/open/agent-pairing/resolve",
        base_url.trim_end_matches('/')
    );
    Url::parse(&resolve_url).map_err(|err| format!("Arkret Base URL is invalid: {err}"))?;
    Ok((resolve_url, token))
}

#[cfg(feature = "arkret")]
fn arkret_pairing_token_from_fragment(fragment: &str) -> Option<String> {
    form_urlencoded::parse(fragment.as_bytes())
        .find(|(key, _)| key == "token")
        .map(|(_, value)| value.into_owned())
}

#[cfg(feature = "arkret")]
fn is_arkret_pairing_token_shape(value: &str) -> bool {
    (22..=512).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(feature = "arkret")]
async fn fetch_arkret_pairing_bootstrap(
    client: &reqwest::Client,
    resolve_url: &str,
    pairing_token: &str,
) -> Result<Value, String> {
    let body = serde_json::to_vec(&json!({ "pairing_token": pairing_token }))
        .map_err(|err| format!("serialize pairing resolver request: {err}"))?;
    let response = client
        .post(resolve_url)
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|err| format!("resolve Arkret pairing link failed: {err}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("read Arkret pairing resolver response failed: {err}"))?;
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&bytes);
        let detail = detail.chars().take(240).collect::<String>();
        return Err(format!(
            "Arkret pairing resolver returned HTTP {status}: {detail}"
        ));
    }
    let value = serde_json::from_slice::<Value>(&bytes)
        .map_err(|err| format!("Arkret pairing resolver returned invalid JSON: {err}"))?;
    validate_arkret_pairing_bootstrap_value(value)
}

#[cfg(feature = "arkret")]
fn validate_arkret_pairing_bootstrap_value(value: Value) -> Result<Value, String> {
    let bootstrap: arkret::AgentPairingBootstrap =
        serde_json::from_value(value.clone()).map_err(|err| {
            format!("Arkret pairing resolver returned invalid AgentPairingBootstrap: {err}")
        })?;
    if bootstrap.pairing_request_id.trim().is_empty()
        || bootstrap.pairing_code.trim().is_empty()
        || bootstrap.arkret_base_url.trim().is_empty()
    {
        return Err("Arkret pairing bootstrap contains empty required fields".to_owned());
    }
    Ok(value)
}

#[cfg(not(feature = "arkret"))]
pub(crate) async fn handle_channels_arkret_generate_runtime_key_ref(
    _params: &Value,
    _channel: &Arc<GatewayChannel>,
) -> RpcResult {
    Err((
        INVALID_REQUEST,
        "Arkret support is not enabled in this build".to_owned(),
    ))
}

#[cfg(feature = "arkret")]
pub(crate) async fn handle_channels_arkret_generate_runtime_key_ref(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let label = params
        .get("account_id")
        .or_else(|| params.get("accountId"))
        .or_else(|| params.get("agent_id"))
        .or_else(|| params.get("agentId"))
        .or_else(|| params.get("principalId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let _ = channel;
    let keyring_account = format!(
        "runtime-{}",
        sanitize_arkret_runtime_key_label(label.unwrap_or("agent"))
    );
    let source_key_ref = params
        .get("key_ref")
        .or_else(|| params.get("keyRef"))
        .filter(|value| !value.is_null())
        .map(|value| {
            serde_json::from_value::<savfox_channels::arkret::ArkretKeyRef>(value.clone())
                .map_err(|err| anyhow::anyhow!("invalid legacy Arkret keyRef: {err}"))
        })
        .transpose()
        .map_err(|err| (INVALID_REQUEST, err.to_string()))?;
    let key_ref = match source_key_ref {
        Some(savfox_channels::arkret::ArkretKeyRef::Keyring { service, account }) => {
            savfox_channels::arkret::ArkretKeyRef::Keyring { service, account }
        }
        Some(_) => {
            return Err((
                INVALID_REQUEST,
                "Arkret agent runtime keys must already use kind='keyring'; legacy key sources are not migrated"
                    .to_owned(),
            ));
        }
        None => savfox_channels::arkret::get_or_generate_ed25519_key_ref_in_keyring(
            "savfox-arkret",
            keyring_account,
        )
        .map_err(|err| (INTERNAL_ERROR, err.to_string()))?,
    };
    let key_ref_json = serde_json::to_value(&key_ref)
        .map_err(|err| (INTERNAL_ERROR, format!("serialize Arkret keyRef: {err}")))?;
    Ok(json!({
        "platform": "arkret",
        "ok": true,
        "mode": "agent",
        "key_ref": key_ref_json,
        "message": "Arkret runtime key stored in the platform credential vault",
    }))
}

#[cfg(feature = "arkret")]
fn sanitize_arkret_runtime_key_label(label: &str) -> String {
    let mut sanitized = String::new();
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            sanitized.push(ch);
        } else if !sanitized.ends_with('-') {
            sanitized.push('-');
        }
        if sanitized.len() >= 64 {
            break;
        }
    }
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "agent".to_owned()
    } else {
        sanitized.to_owned()
    }
}

fn matrix_test_channel_config(
    params: &Value,
    saved_configs: &[savfox_core::config::channel_store::ChannelConfig],
) -> Option<savfox_core::config::channel_store::ChannelConfig> {
    if let Some(config) = params.get("config").filter(|value| value.is_object()) {
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("matrix-test");
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(id);
        return Some(savfox_core::config::channel_store::ChannelConfig {
            id: id.to_owned(),
            kind: "matrix".to_owned(),
            slug: id.to_owned(),
            name: name.to_owned(),
            enabled: true,
            config: config.clone(),
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        });
    }

    if let Some(requested_id) = params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return saved_configs
            .iter()
            .find(|config| {
                config.id == requested_id && channel_platform_matches_kind(&config.kind, "matrix")
            })
            .cloned();
    }

    saved_configs
        .iter()
        .find(|config| {
            channel_platform_matches_kind(&config.kind, "matrix")
                && config.enabled
                && saved_channel_config_ready(config)
        })
        .or_else(|| {
            saved_configs.iter().find(|config| {
                channel_platform_matches_kind(&config.kind, "matrix") && config.enabled
            })
        })
        .cloned()
}

async fn handle_matrix_channel_test(
    params: &Value,
    channel: &Arc<GatewayChannel>,
    saved_configs: &[savfox_core::config::channel_store::ChannelConfig],
) -> RpcResult {
    let Some(raw_config) = matrix_test_channel_config(params, saved_configs) else {
        return Ok(json!({
            "platform": "matrix",
            "ok": false,
            "message": "matrix is not configured. Please add configuration in the channel settings.",
        }));
    };
    let parsed = savfox_channels::matrix::MatrixChannelConfig::from_channel_config(&raw_config)
        .ok_or_else(|| {
            (
                INVALID_REQUEST,
                "Matrix channel config must be an object".to_owned(),
            )
        })?;
    parsed
        .validate_auth()
        .map_err(|err| (INVALID_REQUEST, err.to_string()))?;

    let pending_invites =
        crate::channels::matrix::MatrixInviteStore::for_savfox_home(&channel.config().savfox_home)
            .list(false)
            .await
            .ok()
            .map(|invites| {
                invites
                    .into_iter()
                    .filter(|invite| invite.config_id == raw_config.id)
                    .count() as u32
            });

    match parsed.mode {
        savfox_channels::matrix::MatrixMode::User => {
            let resolved = GatewayChannel::resolve_matrix_client(
                &parsed.homeserver,
                parsed.access_token.as_deref(),
                parsed.user_id.as_deref(),
                parsed.password.as_deref(),
                parsed.device_name.as_deref(),
            )
            .await
            .map_err(|err| {
                (
                    INTERNAL_ERROR,
                    format!("Matrix authentication failed: {err}"),
                )
            })?;
            let joined_rooms = resolved
                .client
                .get_joined_rooms()
                .await
                .map(|rooms| rooms.len() as u32)
                .ok();
            Ok(json!({
                "platform": "matrix",
                "ok": true,
                "mode": "user",
                "user_id": resolved.user_id,
                "room_count": joined_rooms,
                "pending_invites": pending_invites,
                "message": match joined_rooms {
                    Some(count) => format!("Matrix user connection ok ({count} joined rooms)"),
                    None => "Matrix user connection ok".to_owned(),
                },
            }))
        }
        savfox_channels::matrix::MatrixMode::Appservice => Ok(json!({
            "platform": "matrix",
            "ok": true,
            "mode": "appservice",
            "registration": crate::channels::matrix::matrix_appservice_registration_preview(&parsed),
            "message": "Matrix appservice configuration is valid",
        })),
    }
}

pub(crate) async fn handle_channels_matrix_invites(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let include_dismissed = params
        .get("include_dismissed")
        .or_else(|| params.get("includeDismissed"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let config_id = params
        .get("config_id")
        .or_else(|| params.get("configId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let store =
        crate::channels::matrix::MatrixInviteStore::for_savfox_home(&channel.config().savfox_home);
    let mut invites = store.list(include_dismissed).await.map_err(|err| {
        (
            INTERNAL_ERROR,
            format!("failed to load Matrix invites: {err}"),
        )
    })?;
    if let Some(config_id) = config_id {
        invites.retain(|invite| invite.config_id == config_id);
    }

    Ok(json!({
        "status": "ok",
        "invites": invites,
    }))
}

pub(crate) async fn handle_channels_matrix_invite_accept(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let room_id = params
        .get("room_id")
        .or_else(|| params.get("roomId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'room_id' parameter".to_owned()))?;
    let config_id = params
        .get("config_id")
        .or_else(|| params.get("configId"))
        .and_then(Value::as_str)
        .unwrap_or("");

    channel
        .accept_matrix_invite(config_id, room_id)
        .await
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to accept Matrix invite: {err}"),
            )
        })?;

    Ok(json!({
        "status": "accepted",
        "room_id": room_id,
    }))
}

pub(crate) async fn handle_channels_matrix_invite_reject(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let room_id = params
        .get("room_id")
        .or_else(|| params.get("roomId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'room_id' parameter".to_owned()))?;
    let config_id = params
        .get("config_id")
        .or_else(|| params.get("configId"))
        .and_then(Value::as_str)
        .unwrap_or("");

    channel
        .reject_matrix_invite(config_id, room_id)
        .await
        .map_err(|err| {
            (
                INTERNAL_ERROR,
                format!("failed to reject Matrix invite: {err}"),
            )
        })?;

    Ok(json!({
        "status": "rejected",
        "room_id": room_id,
    }))
}

pub(crate) async fn handle_channels_matrix_invite_dismiss(
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let room_id = params
        .get("room_id")
        .or_else(|| params.get("roomId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'room_id' parameter".to_owned()))?;
    let config_id = params
        .get("config_id")
        .or_else(|| params.get("configId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| (INVALID_REQUEST, "missing 'config_id' parameter".to_owned()))?;

    let store =
        crate::channels::matrix::MatrixInviteStore::for_savfox_home(&channel.config().savfox_home);
    let dismissed = store.dismiss(config_id, room_id).await.map_err(|err| {
        (
            INTERNAL_ERROR,
            format!("failed to dismiss Matrix invite: {err}"),
        )
    })?;

    Ok(json!({
        "status": if dismissed { "dismissed" } else { "not_found" },
        "room_id": room_id,
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
        .map(str::to_owned)
        .or_else(|| {
            latest_provenance(entry)
                .map(|item| item.user_id.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            entry
                .identity
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
}

fn session_display_name(entry: &SessionEntry) -> Option<String> {
    latest_provenance(entry)
        .map(|item| item.name.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
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

    #[cfg(feature = "arkret")]
    use super::{
        SavedChannelState, arkret_pairing_resolve_target, arkret_runtime_key_status_flags,
        insert_arkret_listener_summary, insert_saved_channel_metadata,
        sanitize_arkret_runtime_key_label, validate_arkret_pairing_bootstrap_value,
    };
    use super::{
        arkret_test_channel_config, matrix_test_channel_config, request_matches_channel_instance,
        saved_channel_config_ready,
    };

    #[cfg(feature = "arkret")]
    #[test]
    fn paused_agent_key_is_approved_but_not_ready() {
        assert_eq!(
            arkret_runtime_key_status_flags("paused", Some(true)),
            (true, false, false)
        );
        assert_eq!(
            arkret_runtime_key_status_flags("active", Some(true)),
            (true, true, false)
        );
    }

    fn channel_config(
        kind: &str,
        config: serde_json::Value,
    ) -> savfox_core::config::channel_store::ChannelConfig {
        savfox_core::config::channel_store::ChannelConfig {
            id: format!("{kind}-default"),
            kind: kind.to_owned(),
            slug: String::new(),
            name: kind.to_owned(),
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
    fn channel_test_selects_the_requested_instance() {
        let mut arkret_one = channel_config("arkret", json!({"mode": "agent"}));
        arkret_one.id = "arkret-one".to_owned();
        let mut arkret_two = channel_config("arkret", json!({"mode": "agent"}));
        arkret_two.id = "arkret-two".to_owned();
        let mut matrix_one = channel_config("matrix", json!({"homeserver": "one"}));
        matrix_one.id = "matrix-one".to_owned();
        let saved = vec![arkret_one, arkret_two, matrix_one];

        assert_eq!(
            arkret_test_channel_config(&json!({"id": "arkret-two"}), &saved)
                .map(|config| config.id),
            Some("arkret-two".to_owned())
        );
        assert_eq!(
            matrix_test_channel_config(&json!({"id": "matrix-one"}), &saved)
                .map(|config| config.id),
            Some("matrix-one".to_owned())
        );
        assert!(arkret_test_channel_config(&json!({"id": "arkret-missing"}), &saved).is_none());
        assert!(request_matches_channel_instance(
            &json!({"id": "arkret-two"}),
            &saved[1]
        ));
        assert!(!request_matches_channel_instance(
            &json!({"id": "arkret-two"}),
            &saved[0]
        ));
    }

    #[cfg(feature = "arkret")]
    fn sdk_inkson_bootstrap() -> serde_json::Value {
        json!({
            "arkret_base_url": "https://arkret.example.org",
            "service_id": "did:webvh:arkret.example.org",
            "agent_id": "did:webvh:example.org:agents:support",
            "pairing_request_id": "agent_pairing_request:01904100-0000-7000-8000-000000000001",
            "pairing_code": "12345678",
            "pairing_expires_at": "2999-01-01T00:00:00.000Z"
        })
    }

    #[cfg(feature = "arkret")]
    #[test]
    fn arkret_pairing_resolver_target_accepts_fragment_link() {
        let token = "abcdefghijklmnopqrstuvwxyz_123456";
        let link = format!("https://local.host/_arkret/open/agent-pairing/resolve#token={token}");

        let (resolve_url, parsed_token) =
            arkret_pairing_resolve_target(&link, None).expect("resolve target");

        assert_eq!(
            resolve_url,
            "https://local.host/_arkret/open/agent-pairing/resolve"
        );
        assert_eq!(parsed_token, token);
    }

    #[cfg(feature = "arkret")]
    #[test]
    fn arkret_pairing_resolver_target_rejects_query_token() {
        let token = "abcdefghijklmnopqrstuvwxyz_123456";
        let link = format!("https://local.host/_arkret/open/agent-pairing/resolve?token={token}");

        let err = arkret_pairing_resolve_target(&link, None).expect_err("query token must fail");

        assert!(err.contains("fragment"));
    }

    #[cfg(feature = "arkret")]
    #[test]
    fn arkret_pairing_resolver_target_accepts_token_with_base_url() {
        let token = "abcdefghijklmnopqrstuvwxyz_123456";

        let (resolve_url, parsed_token) =
            arkret_pairing_resolve_target(token, Some("https://local.host/"))
                .expect("resolve target");

        assert_eq!(
            resolve_url,
            "https://local.host/_arkret/open/agent-pairing/resolve"
        );
        assert_eq!(parsed_token, token);
    }

    #[cfg(feature = "arkret")]
    #[test]
    fn arkret_pairing_bootstrap_validation_rejects_legacy_scope_payload() {
        let mut value = sdk_inkson_bootstrap();
        value["requested_scope"] = json!({ "actions": ["ak.event.read"] });

        let err = validate_arkret_pairing_bootstrap_value(value)
            .expect_err("legacy bootstrap fields must fail validation");

        assert!(err.contains("unknown field"));
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

    #[cfg(feature = "arkret")]
    #[test]
    fn arkret_runtime_key_label_is_filesystem_safe() {
        assert_eq!(
            sanitize_arkret_runtime_key_label("did:webvh:example.org:agents/support"),
            "did-webvh-example.org-agents-support"
        );
        assert_eq!(sanitize_arkret_runtime_key_label(""), "agent");
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

    #[cfg(feature = "arkret")]
    #[test]
    fn arkret_agent_ready_requires_completed_runtime_pairing() {
        let ready = channel_config(
            "arkret",
            json!({
                "mode": "agent",
                "baseUrl": "https://arkret.example.org",
                "serviceId": "did:webvh:arkret.example.org",
                "inksonBootstrap": sdk_inkson_bootstrap(),
                "principalId": "did:webvh:example.org:agents:support",
                "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
                "verificationMethod": "did:webvh:example.org:agents:support#runtime-1",
                "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000099",
                "requestedScope": [
                    "ak.self.events.stream.subscribe",
                    "ak.self.events.query.scan",
                    "ak.self.authorization_leases.command.issue",
                    "ak.self.events.command.submit",
                    "ak.self.keys.keypackages.upload.create",
                    "ak.self.keys.keypackages.command.consume",
                    "ak.self.keys.keypackages.command.revoke",
                    "ak.self.device_messages.query.list",
                    "ak.self.device_messages.command.ack",
                    "ak.event.read",
                    "ak.message.create"
                ]
            }),
        );
        let missing_authorization = channel_config(
            "arkret",
            json!({
                "mode": "agent",
                "baseUrl": "https://arkret.example.org",
                "inksonBootstrap": sdk_inkson_bootstrap(),
                "principalId": "did:webvh:example.org:agents:support",
                "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
                "verificationMethod": "did:webvh:example.org:agents:support#runtime-1"
            }),
        );

        let parsed_ready =
            savfox_channels::arkret::ArkretChannelConfig::from_channel_config(&ready)
                .expect("ready Arkret config should parse");
        parsed_ready
            .validate()
            .expect("ready Arkret config should validate");
        assert!(saved_channel_config_ready(&ready));
        assert!(!saved_channel_config_ready(&missing_authorization));
    }

    #[cfg(feature = "arkret")]
    #[test]
    fn arkret_agent_ready_rejects_content_scope_without_service_scope() {
        let content_only = channel_config(
            "arkret",
            json!({
                "mode": "agent",
                "baseUrl": "https://arkret.example.org",
                "serviceId": "did:webvh:arkret.example.org",
                "inksonBootstrap": sdk_inkson_bootstrap(),
                "principalId": "did:webvh:example.org:agents:support",
                "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
                "verificationMethod": "did:webvh:example.org:agents:support#runtime-1",
                "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000099",
                "requestedScope": ["ak.event.read", "ak.message.create"],
                "listen": true,
                "send": false
            }),
        );

        assert!(!saved_channel_config_ready(&content_only));
    }

    #[cfg(feature = "arkret")]
    #[test]
    fn arkret_metadata_separates_pairing_from_runtime_readiness() {
        let config = channel_config(
            "arkret",
            json!({
                "mode": "agent",
                "baseUrl": "https://arkret.example.org",
                "serviceId": "did:webvh:arkret.example.org",
                "inksonBootstrap": sdk_inkson_bootstrap(),
                "principalId": "did:webvh:example.org:agents:support",
                "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
                "verificationMethod": "did:webvh:example.org:agents:support#runtime-1",
                "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000099",
                "requestedScope": [
                    "ak.self.events.stream.subscribe",
                    "ak.self.events.query.scan",
                    "ak.self.authorization_leases.command.issue",
                    "ak.self.events.command.submit",
                    "ak.self.keys.keypackages.upload.create",
                    "ak.self.keys.keypackages.command.consume",
                    "ak.self.keys.keypackages.command.revoke",
                    "ak.self.device_messages.query.list",
                    "ak.self.device_messages.command.ack",
                    "ak.event.read",
                    "ak.message.create"
                ]
            }),
        );
        let state = SavedChannelState {
            exists: true,
            enabled: true,
            ready: true,
            channel_name: Some("Arkret".to_owned()),
            channel_slug: Some("arkret".to_owned()),
            config: Some(config),
        };
        let mut info = serde_json::Map::new();

        insert_saved_channel_metadata(&mut info, "arkret", &state);

        assert_eq!(info["runtime_pairing_state"], "paired");
        assert_eq!(info["runtime_phase"], "stopped");
        assert_eq!(info["runtime_ready"], false);
        assert_eq!(
            info["authorized_event_ref"],
            "ak:event:01904100-0000-7000-8000-000000000099"
        );
        assert_eq!(
            info["verification_method"],
            "did:webvh:example.org:agents:support#runtime-1"
        );
        assert_eq!(
            info["runtime_scope_count"],
            savfox_channels::arkret::DEFAULT_AGENT_RUNTIME_SCOPE.len()
        );
    }

    #[cfg(feature = "arkret")]
    #[test]
    fn arkret_listener_summary_surfaces_retry_failure() {
        let mut info = serde_json::Map::new();
        insert_arkret_listener_summary(
            &mut info,
            vec![json!({
                "running": true,
                "phase": "retry_wait",
                "last_error": "session authentication failed"
            })],
        );

        assert_eq!(info["runtime_ready"], false);
        assert_eq!(info["runtime_phase"], "retry_wait");
        assert_eq!(info["lastError"], "session authentication failed");
    }

    #[cfg(feature = "arkret")]
    #[test]
    fn arkret_applet_ready_requires_valid_applet_config() {
        let ready = channel_config(
            "arkret",
            json!({
                "mode": "applet",
                "appletId": "ak:applet:21532600-0000-7000-8000-000000000000",
                "serviceId": "did:webvh:slack-bridge.example",
                "controllerId": "did:webvh:example.com:admin",
                "baseUrl": "https://savfox.example/appservices/arkret/arkret-default",
                "botActorId": "did:webvh:slack-bridge.example:bot",
                "arkretServerUrl": "https://arkret.example.org",
                "arkretServerDid": "did:webvh:arkret.example.org",
                "accessToken": "applet-bearer-1",
                "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_APPLET_KEY" },
                "loginChallenge": "applet-login-challenge-1234",
                "protocols": ["slack"],
                "namespaces": {
                    "actors": [
                        { "pattern": "did:webvh:slack-bridge.example:ghost:*", "exclusive": true }
                    ],
                    "realms": [
                        { "pattern": "slack:team:*:channel:*", "exclusive": true }
                    ],
                    "handles": [
                        { "pattern": "slack.acme.example/*", "exclusive": false }
                    ]
                }
            }),
        );
        let missing_namespaces = channel_config(
            "arkret",
            json!({
                "mode": "applet",
                "appletId": "ak:applet:21532600-0000-7000-8000-000000000000",
                "serviceId": "did:webvh:slack-bridge.example",
                "controllerId": "did:webvh:example.com:admin",
                "baseUrl": "https://savfox.example/appservices/arkret/arkret-default",
                "botActorId": "did:webvh:slack-bridge.example:bot",
                "arkretServerUrl": "https://arkret.example.org",
                "accessToken": "applet-bearer-1",
                "protocols": ["slack"]
            }),
        );

        assert!(saved_channel_config_ready(&ready));
        assert!(!saved_channel_config_ready(&missing_namespaces));
    }
}
