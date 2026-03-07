pub(crate) mod dingtalk;
pub(crate) mod discord;
pub(crate) mod feishu;
pub(crate) mod googlechat;
pub(crate) mod imessage;
pub(crate) mod irc;
pub(crate) mod line;
pub(crate) mod matrix;
pub(crate) mod mattermost;
pub(crate) mod msteams;
pub(crate) mod nextcloud;
pub(crate) mod nostr;
pub(crate) mod runtime;
pub(crate) mod signal;
pub(crate) mod slack;
pub(crate) mod telegram;
pub(crate) mod tlon;
pub(crate) mod twitch;
pub(crate) mod webhook;
pub(crate) mod whatsapp;
pub(crate) mod zalo;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) use savfox_core::channel::{Channel, RichMessage};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::channel::GatewayChannel;
use crate::session::SessionStore;

pub(crate) type ChannelRegistry = Arc<RwLock<HashMap<String, Box<dyn Channel>>>>;

pub(crate) fn create_channel_registry() -> ChannelRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

pub(crate) async fn initialize_and_start_channels(
    savfox_home: &PathBuf,
    registry: ChannelRegistry,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    println!("[startup] Initializing channel instances...");
    info!("Initializing channel instances");

    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home).await?;

    let mut started_count = 0;
    let mut failed_count = 0;

    for config in all_configs {
        if !config.enabled {
            continue;
        }

        let channel_id = config.id.clone();
        let kind = config.kind.to_lowercase();

        println!(
            "[startup] Starting channel '{}' of type '{}'...",
            channel_id, kind
        );
        info!("Starting channel '{}' of type '{}'", channel_id, kind);

        let result = match kind.as_str() {
            "matrix" => start_matrix_channel(&config, &registry, &channel).await,
            "dingtalk" => start_dingtalk_channel(&config, channel, session_store).await,
            "discord" => start_discord_channel(&config, &registry, &channel).await,
            "telegram" => start_telegram_channel(&config, &registry, &channel).await,
            "slack" => start_slack_channel(&config, &registry, &channel).await,
            "feishu" | "lark" => {
                start_feishu_channel(&config, &registry, channel, session_store).await
            }
            _ => {
                println!(
                    "[startup]   Channel type '{}' not yet implemented for persistent connections",
                    kind
                );
                info!(
                    "Channel type '{}' not yet implemented for persistent connections",
                    kind
                );
                continue;
            }
        };

        match result {
            Ok(()) => {
                started_count += 1;
                println!(
                    "[startup]   ✓ Channel '{}' started successfully",
                    channel_id
                );
                info!("Channel '{}' started successfully", channel_id);
            }
            Err(err) => {
                failed_count += 1;
                println!(
                    "[startup]   ✗ Failed to start channel '{}': {}",
                    channel_id, err
                );
                warn!("Failed to start channel '{}': {}", channel_id, err);
            }
        }
    }

    println!(
        "[startup] Channel startup complete: {} started, {} failed",
        started_count, failed_count
    );
    info!(
        "Channel startup complete: {} started, {} failed",
        started_count, failed_count
    );

    Ok(())
}

pub(crate) async fn start_matrix_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    registry: &ChannelRegistry,
    channel: &Arc<GatewayChannel>,
) -> anyhow::Result<()> {
    use crate::channels::matrix::MatrixChannel;

    let raw = config
        .config
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Matrix channel config must be an object"))?;

    let homeserver = raw
        .get("homeserver")
        .or_else(|| raw.get("homeserver_url"))
        .or_else(|| raw.get("server_url"))
        .and_then(|v| v.as_str())
        .unwrap_or("https://matrix.org")
        .to_string();

    let access_token = raw
        .get("accessToken")
        .or_else(|| raw.get("access_token"))
        .or_else(|| raw.get("token"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Matrix channel missing access token"))?
        .to_string();

    let mut channel = MatrixChannel::new(
        homeserver.clone(),
        access_token,
        channel.http_client().clone(),
    );

    channel.start().await?;

    let mut registry = registry.write().await;
    registry.insert(config.id.clone(), Box::new(channel));

    Ok(())
}

async fn start_discord_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    _registry: &ChannelRegistry,
    _channel: &Arc<GatewayChannel>,
) -> anyhow::Result<()> {
    println!(
        "[startup]   Discord channel persistent connection not yet implemented - using webhook mode"
    );
    Ok(())
}

async fn start_dingtalk_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    use crate::channels::dingtalk::{
        dingtalk_sink, load_dingtalk_channel_config, start_dingtalk_stream,
    };

    let dingtalk_config = load_dingtalk_channel_config(&channel.config().savfox_home)
        .await?
        .or_else(|| crate::channels::dingtalk::DingtalkChannelConfig::from_channel_config(config))
        .ok_or_else(|| anyhow::anyhow!("Dingtalk channel config must be an object"))?;

    if dingtalk_config.stream_enabled() {
        let sink = dingtalk_sink(Arc::clone(channel), Arc::clone(session_store));
        start_dingtalk_stream(&config.id, &dingtalk_config, sink).await?;
    }

    Ok(())
}

async fn start_telegram_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    _registry: &ChannelRegistry,
    _channel: &Arc<GatewayChannel>,
) -> anyhow::Result<()> {
    println!(
        "[startup]   Telegram channel persistent connection not yet implemented - using webhook mode"
    );
    Ok(())
}

async fn start_slack_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    _registry: &ChannelRegistry,
    _channel: &Arc<GatewayChannel>,
) -> anyhow::Result<()> {
    println!(
        "[startup]   Slack channel persistent connection not yet implemented - using webhook mode"
    );
    Ok(())
}

async fn start_feishu_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    registry: &ChannelRegistry,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    use crate::channels::feishu::{FeishuChannel, FeishuChannelConfig, feishu_sink};
    use savfox_channels::feishu::start_feishu_stream;

    let feishu_config = FeishuChannelConfig::from_channel_config(config)
        .ok_or_else(|| anyhow::anyhow!("Feishu channel config must be an object"))?;

    println!(
        "[startup]   Starting Feishu channel with config: {:#?}",
        feishu_config
    );
    if feishu_config.stream_enabled() {
        let sink = feishu_sink(Arc::clone(channel), Arc::clone(session_store));
        start_feishu_stream(&config.id, &feishu_config, sink).await?;
    }

    let mut channel = FeishuChannel::new(feishu_config, channel.http_client().clone());
    channel.start().await?;

    let mut registry = registry.write().await;
    registry.insert(config.id.clone(), Box::new(channel));

    Ok(())
}

pub(crate) async fn log_all_configured_channels(savfox_home: &PathBuf) -> anyhow::Result<()> {
    println!(
        "[startup] Loading channel configurations from {:?}...",
        savfox_home
    );
    info!("Loading channel configurations from {:?}", savfox_home);

    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home).await?;

    println!(
        "[startup] Found {} total channel configuration(s)",
        all_configs.len()
    );
    info!("Found {} total channel configuration(s)", all_configs.len());

    if all_configs.is_empty() {
        println!("[startup] No channel configurations found");
        warn!("No channel configurations found in {:?}", savfox_home);
        return Ok(());
    }

    let mut enabled_count = 0;
    let mut disabled_count = 0;
    let mut by_kind: std::collections::HashMap<String, Vec<(String, bool)>> =
        std::collections::HashMap::new();

    for config in &all_configs {
        let configs = by_kind.entry(config.kind.clone()).or_insert_with(Vec::new);
        configs.push((config.id.clone(), config.enabled));

        if config.enabled {
            enabled_count += 1;
        } else {
            disabled_count += 1;
        }
    }

    println!(
        "[startup] Channel summary: {} enabled, {} disabled",
        enabled_count, disabled_count
    );
    info!(
        "Channel summary: {} enabled, {} disabled",
        enabled_count, disabled_count
    );

    for (kind, configs) in &by_kind {
        println!("[startup] {} channel(s) of type '{}':", configs.len(), kind);
        info!("{} channel(s) of type '{}'", configs.len(), kind);

        for (id, enabled) in configs {
            let status = if *enabled { "ENABLED" } else { "DISABLED" };
            println!("[startup]   - {} [{}]", id, status);
            info!("  - {} [{}]", id, status);

            if *enabled && kind.eq_ignore_ascii_case("matrix") {
                if let Err(err) = log_matrix_channel_details(savfox_home, id).await {
                    warn!("Failed to log Matrix channel details for {}: {}", id, err);
                }
            }
        }
    }

    Ok(())
}

async fn log_matrix_channel_details(savfox_home: &PathBuf, channel_id: &str) -> anyhow::Result<()> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home).await?;

    for config in all_configs {
        if config.id == channel_id && config.enabled && config.kind.eq_ignore_ascii_case("matrix") {
            if let Some(raw) = config.config.as_object() {
                let homeserver = raw
                    .get("homeserver")
                    .or_else(|| raw.get("homeserver_url"))
                    .or_else(|| raw.get("server_url"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("https://matrix.org");

                let has_token = raw
                    .get("accessToken")
                    .or_else(|| raw.get("access_token"))
                    .or_else(|| raw.get("token"))
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);

                let rooms: Vec<String> = raw
                    .get("rooms")
                    .or_else(|| raw.get("groups"))
                    .or_else(|| raw.get("roomIds"))
                    .or_else(|| raw.get("room_ids"))
                    .map(|v| match v {
                        serde_json::Value::String(s) => s
                            .split(['\n', ','])
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .collect(),
                        serde_json::Value::Array(arr) => arr
                            .iter()
                            .filter_map(|item| item.as_str().map(String::from))
                            .collect(),
                        _ => Vec::new(),
                    })
                    .unwrap_or_default();

                let rooms_str = if rooms.is_empty() {
                    "none".to_string()
                } else {
                    rooms.join(", ")
                };

                println!("[startup]     Matrix channel details:");
                println!("[startup]       Homeserver: {}", homeserver);
                println!(
                    "[startup]       Access token: {}",
                    if has_token { "configured" } else { "NOT SET" }
                );
                println!("[startup]       Configured rooms: {}", rooms_str);
                info!(
                    "Matrix channel starting with homeserver URL: {}",
                    homeserver
                );
                info!("  Channel ID: {}", channel_id);
                info!(
                    "  Access token: {}",
                    if has_token { "configured" } else { "NOT SET" }
                );
                info!("  Configured rooms: {}", rooms_str);
            }
            break;
        }
    }

    Ok(())
}
