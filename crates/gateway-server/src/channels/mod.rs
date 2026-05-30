pub(crate) mod channel_stream;
#[cfg(feature = "contrix")]
pub(crate) mod contrix;
#[cfg(feature = "contrix")]
pub(crate) mod contrix_applet;
pub(crate) mod dingtalk;
pub(crate) mod discord;
pub(crate) mod feishu;
pub(crate) mod googlechat;
pub(crate) mod irc;
pub(crate) mod line;
pub(crate) mod matrix;
pub(crate) mod mattermost;
pub(crate) mod msteams;
pub(crate) mod policy;
pub(crate) mod qq;
pub(crate) mod runtime;
pub(crate) mod signal;
pub(crate) mod slack;
pub(crate) mod telegram;
pub(crate) mod webhook;
pub(crate) mod wechat;
pub(crate) mod whatsapp;
pub(crate) mod zalo;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use salvo::http::StatusCode;
use salvo::prelude::*;
pub(crate) use savfox_core::channel::{Channel, RichMessage};
use serde_json::json;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::channel::GatewayChannel;
use crate::session::SessionStore;

/// Obtain gateway channel and session store from the depot, rendering an error
/// response and returning `None` if either is unavailable.
pub(crate) fn obtain_channel_and_store(
    depot: &mut Depot,
    res: &mut Response,
) -> Option<(Arc<GatewayChannel>, Arc<SessionStore>)> {
    let gateway_channel = match depot.obtain::<Arc<GatewayChannel>>() {
        Ok(ch) => ch.clone(),
        Err(err) => {
            warn!("webhook: missing gateway channel state: {err:?}");
            render_error(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "state_unavailable",
                "gateway channel state unavailable",
            );
            return None;
        }
    };
    let session_store = match depot.obtain::<Arc<SessionStore>>() {
        Ok(store) => store.clone(),
        Err(err) => {
            warn!("webhook: missing session store state: {err:?}");
            render_error(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "state_unavailable",
                "session store state unavailable",
            );
            return None;
        }
    };
    Some((gateway_channel, session_store))
}

/// Parse the request body as JSON, rendering a BAD_REQUEST error if parsing fails.
pub(crate) async fn parse_json_body(
    req: &mut Request,
    res: &mut Response,
    channel_name: &str,
) -> Option<serde_json::Value> {
    match req.parse_json::<serde_json::Value>().await {
        Ok(body) => Some(body),
        Err(err) => {
            warn!("{channel_name} webhook: failed to parse body: {err}");
            render_error(
                res,
                StatusCode::BAD_REQUEST,
                "invalid_payload",
                format!("failed to parse {channel_name} payload: {err}"),
            );
            None
        }
    }
}

/// Render a JSON error response. Shared across all channel webhook handlers.
pub(crate) fn render_error(
    res: &mut Response,
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
) {
    res.status_code(status);
    res.render(Text::Json(
        json!({
            "error": {
                "code": code,
                "message": message.into(),
            }
        })
        .to_string(),
    ));
}

pub(crate) type ChannelRegistry = Arc<RwLock<HashMap<String, Box<dyn Channel>>>>;

pub(crate) fn create_channel_registry() -> ChannelRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

fn saved_channel_platform_matches(kind: &str, platform: &str) -> bool {
    let normalize = |value: &str| match value.trim().to_ascii_lowercase().as_str() {
        "lark" => "feishu".to_owned(),
        other => other.to_owned(),
    };
    normalize(kind) == normalize(platform)
}

pub(crate) async fn saved_channel_enabled_state(
    savfox_home: &PathBuf,
    platform: &str,
) -> anyhow::Result<Option<bool>> {
    let configs = savfox_core::config::channel_store::list_channel_configs(savfox_home).await?;
    let mut matched = false;
    let mut any_enabled = false;
    for config in configs {
        if !saved_channel_platform_matches(&config.kind, platform) {
            continue;
        }
        matched = true;
        if config.enabled {
            any_enabled = true;
            break;
        }
    }
    if matched {
        Ok(Some(any_enabled))
    } else {
        Ok(None)
    }
}

pub(crate) async fn ensure_inbound_channel_enabled(
    depot: &mut Depot,
    res: &mut Response,
    platform: &str,
) -> bool {
    let gateway_channel = match depot.obtain::<Arc<GatewayChannel>>() {
        Ok(ch) => ch.clone(),
        Err(err) => {
            warn!("webhook: missing gateway channel state for {platform}: {err:?}");
            render_error(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "state_unavailable",
                "gateway channel state unavailable",
            );
            return false;
        }
    };

    match saved_channel_enabled_state(&gateway_channel.config().savfox_home, platform).await {
        Ok(Some(true) | None) => true,
        Ok(Some(false)) => {
            render_error(
                res,
                StatusCode::SERVICE_UNAVAILABLE,
                "channel_disabled",
                format!("{platform} channel is disabled"),
            );
            false
        }
        Err(err) => {
            warn!("webhook: failed to load saved config state for {platform}: {err}");
            render_error(
                res,
                StatusCode::INTERNAL_SERVER_ERROR,
                "config_unavailable",
                format!("failed to load {platform} channel configuration"),
            );
            false
        }
    }
}

pub(crate) async fn initialize_and_start_channels(
    savfox_home: &PathBuf,
    registry: ChannelRegistry,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
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

        info!("Starting channel '{}' of type '{}'", channel_id, kind);

        let result = match kind.as_str() {
            "matrix" => start_matrix_channel(&config, &registry, channel, session_store).await,
            #[cfg(feature = "contrix")]
            "contrix" => {
                // Branch on the inner `mode` discriminator: `"applet"` runs as
                // a registered Applet (≈ Matrix Appservice), anything else
                // (default `"account"`) runs as a controlled-account client.
                let mode = config
                    .config
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .map(|m| m.to_ascii_lowercase())
                    .unwrap_or_else(|| "account".to_owned());
                if mode == "applet" {
                    crate::channels::contrix_applet::start_contrix_applet_channel(
                        &config,
                        channel,
                        session_store,
                    )
                    .await
                } else {
                    crate::channels::contrix::start_contrix_channel(
                        &config,
                        &registry,
                        channel,
                        session_store,
                    )
                    .await
                }
            }
            "dingtalk" => start_dingtalk_channel(&config, channel, session_store).await,
            "discord" => start_discord_channel(&config, &registry, channel, session_store).await,
            "telegram" => start_telegram_channel(&config, &registry, channel, session_store).await,
            "slack" => start_slack_channel(&config, &registry, channel).await,
            "mattermost" | "googlechat" | "line" | "whatsapp" | "qq" | "wechat" => {
                start_webhook_only_channel(&config).await
            }
            "feishu" | "lark" => {
                start_feishu_channel(&config, &registry, channel, session_store).await
            }
            _ => {
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
                info!("Channel '{}' started successfully", channel_id);
            }
            Err(err) => {
                failed_count += 1;
                warn!("Failed to start channel '{}': {}", channel_id, err);
            }
        }
    }

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
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    use savfox_channels::matrix::{MatrixChannelConfig as MatrixPlatformConfig, MatrixMode};

    let matrix_config = MatrixPlatformConfig::from_channel_config(config)
        .ok_or_else(|| anyhow::anyhow!("Matrix channel config must be an object"))?;
    matrix_config.validate_auth()?;
    let mode_label = match matrix_config.mode {
        MatrixMode::User => "user",
        MatrixMode::Appservice => "appservice",
    };
    info!(
        "matrix: channel '{}' validated in mode='{}'",
        config.id, mode_label
    );
    if matches!(matrix_config.mode, MatrixMode::Appservice) {
        info!(
            "matrix: appservice public_url='{}' sender_localpart='{}' server_name='{}' user_prefix='{}'",
            matrix_config.public_url.as_deref().unwrap_or("NOT SET"),
            matrix_config
                .sender_localpart
                .as_deref()
                .unwrap_or("NOT SET"),
            matrix_config.server_name.as_deref().unwrap_or("NOT SET"),
            matrix_config.user_prefix,
        );
    }

    let mut channel: Box<dyn Channel> = match matrix_config.mode {
        MatrixMode::User => Box::new(crate::channels::matrix::MatrixChannel::new(
            matrix_config,
            Arc::clone(channel),
            Arc::clone(session_store),
        )),
        MatrixMode::Appservice => Box::new(crate::channels::matrix::MatrixAppserviceChannel::new(
            matrix_config,
            Arc::clone(channel),
            Arc::clone(session_store),
        )?),
    };

    channel.start().await?;
    info!(
        "matrix: channel '{}' start() completed and is being registered",
        config.id
    );

    let mut registry = registry.write().await;
    registry.insert(config.id.clone(), channel);
    info!(
        "matrix: channel '{}' registered in channel registry",
        config.id
    );

    Ok(())
}

async fn start_discord_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    _registry: &ChannelRegistry,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    let discord_config =
        savfox_channels::discord::DiscordChannelConfig::from_channel_config(config)
            .ok_or_else(|| anyhow::anyhow!("Discord channel config must be an object"))?;

    if discord_config.stream_enabled() {
        let sink = crate::channels::discord::discord_sink(
            Arc::clone(channel),
            Arc::clone(session_store),
            config.id.clone(),
        );
        savfox_channels::discord::start_discord_stream(&config.id, &discord_config, sink).await?;
        info!("Discord channel '{}' started in stream mode", config.id);
    } else {
        info!(
            "Discord channel '{}' configured in webhook mode; interaction webhook remains enabled",
            config.id
        );
        warn!(
            "Discord channel '{}' is configured in webhook mode; inbound plain messages/DMs will not use the gateway stream",
            config.id
        );
    }
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
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    let telegram_config =
        savfox_channels::telegram::TelegramChannelConfig::from_channel_config(config)
            .ok_or_else(|| anyhow::anyhow!("Telegram channel config must be an object"))?;
    let has_token = config
        .config
        .get("bot_token")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty());
    info!(
        "telegram: Channel '{}' initialized ({} mode), bot_token={}",
        config.id,
        if telegram_config.polling {
            "polling"
        } else {
            "webhook"
        },
        if has_token { "configured" } else { "NOT SET" }
    );
    if telegram_config.polling {
        let sink = crate::channels::telegram::telegram_sink(
            Arc::clone(channel),
            Arc::clone(session_store),
        );
        savfox_channels::telegram::start_telegram_polling(&config.id, &telegram_config, sink)
            .await?;
    }
    if let Some(dm) = &config.dm_policy {
        info!("telegram: DM policy: {:?}", dm.mode);
    }
    if let Some(grp) = &config.group_policy {
        info!("telegram: Group policy: {:?}", grp.mode);
    }
    Ok(())
}

async fn start_slack_channel(
    _config: &savfox_core::config::channel_store::ChannelConfig,
    _registry: &ChannelRegistry,
    _channel: &Arc<GatewayChannel>,
) -> anyhow::Result<()> {
    info!("Slack channel persistent connection not yet implemented - using webhook mode");
    Ok(())
}

async fn start_webhook_only_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
) -> anyhow::Result<()> {
    info!(
        "Channel '{}' of type '{}' is enabled in webhook mode",
        config.id, config.kind
    );
    Ok(())
}

async fn start_feishu_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    registry: &ChannelRegistry,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    use savfox_channels::feishu::start_feishu_stream;

    use crate::channels::feishu::{FeishuChannel, FeishuChannelConfig, feishu_sink};

    let feishu_config = FeishuChannelConfig::from_channel_config(config)
        .ok_or_else(|| anyhow::anyhow!("Feishu channel config must be an object"))?;

    info!("Starting Feishu channel with config: {:#?}", feishu_config);
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
    info!("Loading channel configurations from {:?}", savfox_home);

    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home).await?;

    info!("Found {} total channel configuration(s)", all_configs.len());

    if all_configs.is_empty() {
        warn!("No channel configurations found in {:?}", savfox_home);
        return Ok(());
    }

    let mut enabled_count = 0;
    let mut disabled_count = 0;
    let mut by_kind: std::collections::HashMap<String, Vec<(String, bool)>> =
        std::collections::HashMap::new();

    for config in &all_configs {
        let configs = by_kind.entry(config.kind.clone()).or_default();
        configs.push((config.id.clone(), config.enabled));

        if config.enabled {
            enabled_count += 1;
        } else {
            disabled_count += 1;
        }
    }

    info!(
        "Channel summary: {} enabled, {} disabled",
        enabled_count, disabled_count
    );

    for (kind, configs) in &by_kind {
        info!("{} channel(s) of type '{}'", configs.len(), kind);

        for (id, enabled) in configs {
            let status = if *enabled { "ENABLED" } else { "DISABLED" };
            info!("  - {} [{}]", id, status);

            if *enabled
                && kind.eq_ignore_ascii_case("matrix")
                && let Err(err) = log_matrix_channel_details(savfox_home, id).await
            {
                warn!("Failed to log Matrix channel details for {}: {}", id, err);
            }
        }
    }

    Ok(())
}

async fn log_matrix_channel_details(savfox_home: &PathBuf, channel_id: &str) -> anyhow::Result<()> {
    let matrix_configs = savfox_channels::matrix::load_matrix_channel_configs(savfox_home).await?;

    for config in matrix_configs {
        if config.id != channel_id {
            continue;
        }

        let has_token = config
            .access_token
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let rooms_str = if config.rooms.is_empty() {
            "none".to_owned()
        } else {
            config.rooms.join(", ")
        };
        let mode = match config.mode {
            savfox_channels::matrix::MatrixMode::User => "user",
            savfox_channels::matrix::MatrixMode::Appservice => "appservice",
        };

        info!("Matrix channel details:");
        info!("  Mode: {}", mode);
        info!("  Homeserver: {}", config.homeserver);
        if matches!(config.mode, savfox_channels::matrix::MatrixMode::User) {
            info!(
                "  Access token: {}",
                if has_token { "configured" } else { "NOT SET" }
            );
        }
        if matches!(config.mode, savfox_channels::matrix::MatrixMode::Appservice) {
            info!(
                "  Appservice URL: {}",
                config.public_url.as_deref().unwrap_or("NOT SET")
            );
            info!(
                "  Sender localpart: {}",
                config.sender_localpart.as_deref().unwrap_or("NOT SET")
            );
        }
        info!("  Channel ID: {}", channel_id);
        info!("  Configured rooms: {}", rooms_str);
        break;
    }

    Ok(())
}
