#[cfg(feature = "arkret")]
pub(crate) mod arkret;
#[cfg(feature = "arkret")]
pub(crate) mod arkret_applet;
pub(crate) mod channel_stream;
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
pub(crate) mod recovery;
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

#[cfg(feature = "arkret")]
use anyhow::Context as _;
use salvo::http::StatusCode;
use salvo::prelude::*;
pub(crate) use savfox_core::channel::{Channel, RichMessage};
#[cfg(feature = "arkret")]
use serde_json::Value;
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
    let gateway_channel = match depot.get_typed::<Arc<GatewayChannel>>() {
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
    let session_store = match depot.get_typed::<Arc<SessionStore>>() {
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
    let gateway_channel = match depot.get_typed::<Arc<GatewayChannel>>() {
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
    #[cfg(feature = "arkret")]
    let all_configs = {
        let mut retained = Vec::with_capacity(all_configs.len());
        for config in all_configs {
            if !config.kind.eq_ignore_ascii_case("arkret") {
                retained.push(config);
                continue;
            }
            let mode = config.config.get("mode").and_then(Value::as_str);
            let validation = match mode {
                Some("agent") => {
                    savfox_channels::arkret::ArkretChannelConfig::from_strict_agent_config(&config)
                        .map(|_| ())
                }
                Some("applet") => {
                    let mut enabled = config.clone();
                    enabled.enabled = true;
                    savfox_channels::arkret::applet::ArkretAppletConfig::from_channel_config(
                        &enabled,
                    )
                    .ok_or_else(|| anyhow::anyhow!("invalid Arkret applet config"))
                    .and_then(|parsed| parsed.validate())
                }
                Some(other) => Err(anyhow::anyhow!(
                    "unsupported Arkret mode '{other}'; expected 'agent' or 'applet'"
                )),
                None => Err(anyhow::anyhow!("missing Arkret mode")),
            };
            if let Err(error) = validation {
                warn!(
                    channel_id = %config.id,
                    "Deleting invalid Arkret config instead of loading legacy data: {error:#}"
                );
                savfox_core::config::channel_store::delete_channel_config(savfox_home, &config.id)
                    .await
                    .with_context(|| {
                        format!("delete invalid Arkret channel config '{}'", config.id)
                    })?;
                continue;
            }
            retained.push(config);
        }
        retained
    };

    let reports = channel.channel_recovery_registry();
    recovery::recover_channel_configs(&all_configs, &reports, |config| {
        let registry = Arc::clone(&registry);
        let channel = Arc::clone(channel);
        let session_store = Arc::clone(session_store);
        Box::pin(
            async move { start_saved_channel(&config, &registry, &channel, &session_store).await },
        )
    })
    .await;

    let reports = reports.read().await;
    let started_count = reports
        .values()
        .filter(|report| report.phase == recovery::ChannelRecoveryPhase::Ready)
        .count();
    let failed_count = reports
        .values()
        .filter(|report| {
            matches!(
                report.phase,
                recovery::ChannelRecoveryPhase::Failed
                    | recovery::ChannelRecoveryPhase::UnsupportedRuntime
            )
        })
        .count();
    info!(
        "Channel startup complete: {} ready, {} failed or unsupported",
        started_count, failed_count
    );
    drop(reports);
    for config in &all_configs {
        schedule_channel_supervisor(config, channel, session_store).await;
    }

    Ok(())
}

pub(crate) async fn start_saved_channel(
    config: &savfox_core::config::channel_store::ChannelConfig,
    registry: &ChannelRegistry,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    let channel_id = &config.id;
    let kind = config.kind.to_ascii_lowercase();
    info!("Starting channel '{}' of type '{}'", channel_id, kind);

    let result = match kind.as_str() {
        "matrix" => start_matrix_channel(config, registry, channel, session_store).await,
        #[cfg(feature = "arkret")]
        "arkret" => {
            let mode = config
                .config
                .get("mode")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Arkret config is missing mode"))?;
            if mode == "applet" {
                crate::channels::arkret_applet::start_arkret_applet_channel(
                    config,
                    channel,
                    session_store,
                )
                .await
            } else if mode == "agent" {
                crate::channels::arkret::start_arkret_channel(
                    config,
                    registry,
                    channel,
                    session_store,
                )
                .await
            } else {
                anyhow::bail!("unsupported Arkret mode '{mode}'")
            }
        }
        "dingtalk" => start_dingtalk_channel(config, channel, session_store).await,
        "discord" => start_discord_channel(config, registry, channel, session_store).await,
        "telegram" => start_telegram_channel(config, registry, channel, session_store).await,
        "slack" => start_slack_channel(config, registry, channel).await,
        "mattermost" | "googlechat" | "line" | "whatsapp" | "qq" | "wechat" | "msteams"
        | "signal" | "irc" | "zalo" | "webhook" => start_webhook_only_channel(config).await,
        "feishu" | "lark" => start_feishu_channel(config, registry, channel, session_store).await,
        _ => {
            anyhow::bail!("channel type '{kind}' has no supported gateway runtime");
        }
    };

    match &result {
        Ok(()) => info!("Channel '{}' startup completed", channel_id),
        Err(err) => warn!("Failed to start channel '{}': {}", channel_id, err),
    }
    result
}

pub(crate) async fn stop_channel_instance(
    config: &savfox_core::config::channel_store::ChannelConfig,
    channel: &Arc<GatewayChannel>,
) -> anyhow::Result<bool> {
    abort_channel_supervisor(channel, &config.id).await;
    let stopped = match recovery::canonical_platform(&config.kind).as_str() {
        "discord" => savfox_channels::discord::stop_discord_stream(&config.id).await,
        "telegram" => savfox_channels::telegram::stop_telegram_polling(&config.id).await,
        "dingtalk" => savfox_channels::dingtalk::stop_dingtalk_stream(&config.id).await,
        "feishu" => {
            let stopped = savfox_channels::feishu::stop_feishu_stream(&config.id).await;
            let removed = channel
                .channel_registry()
                .write()
                .await
                .remove(&config.id)
                .is_some();
            stopped || removed
        }
        "matrix" => {
            let removed = channel
                .channel_registry()
                .write()
                .await
                .remove(&config.id)
                .is_some();
            let had_appservice =
                crate::channels::matrix::matrix_appservice_channel_for(&config.id).is_some();
            crate::channels::matrix::remove_matrix_appservice_channel(&config.id);
            removed || had_appservice
        }
        "arkret" => {
            #[cfg(feature = "arkret")]
            {
                let listeners = crate::channels::arkret::stop_arkret_account_listeners(&config.id);
                let removed_applet =
                    crate::channels::arkret_applet::remove_arkret_applet_channel(&config.id)?;
                listeners > 0 || removed_applet
            }
            #[cfg(not(feature = "arkret"))]
            {
                false
            }
        }
        _ => false,
    };
    Ok(stopped)
}

async fn channel_runtime_alive(
    config: &savfox_core::config::channel_store::ChannelConfig,
    channel: &Arc<GatewayChannel>,
) -> bool {
    match recovery::canonical_platform(&config.kind).as_str() {
        "discord" => savfox_channels::discord::is_discord_stream_running(&config.id).await,
        "telegram" => savfox_channels::telegram::is_telegram_polling_running(&config.id).await,
        "dingtalk" => savfox_channels::dingtalk::is_dingtalk_stream_running(&config.id).await,
        "feishu" => savfox_channels::feishu::is_feishu_stream_running(&config.id).await,
        "matrix" => channel
            .channel_registry()
            .read()
            .await
            .contains_key(&config.id),
        "arkret" => {
            #[cfg(feature = "arkret")]
            {
                crate::channels::arkret::arkret_account_listener_task_count(&config.id) > 0
                    || crate::channels::arkret_applet::is_arkret_applet_registered(&config.id)
            }
            #[cfg(not(feature = "arkret"))]
            {
                false
            }
        }
        _ => false,
    }
}

fn channel_runtime_last_error(
    config: &savfox_core::config::channel_store::ChannelConfig,
) -> Option<String> {
    match recovery::canonical_platform(&config.kind).as_str() {
        "dingtalk" => savfox_channels::dingtalk::dingtalk_stream_state_snapshot()
            .get(&config.id)
            .and_then(|state| state.last_error.clone()),
        "discord" => savfox_channels::discord::discord_stream_state_snapshot()
            .get(&config.id)
            .and_then(|state| state.last_error.clone()),
        #[cfg(feature = "arkret")]
        "arkret" => crate::channels::arkret::arkret_account_runtime_diagnostics(&config.id)
            .iter()
            .find_map(|diagnostic| {
                diagnostic
                    .get("last_error")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            }),
        _ => None,
    }
}

async fn abort_channel_supervisor(channel: &Arc<GatewayChannel>, channel_id: &str) {
    if let Some(handle) = channel
        .channel_recovery_supervisors()
        .lock()
        .await
        .remove(channel_id)
    {
        handle.abort();
    }
}

async fn schedule_channel_supervisor(
    config: &savfox_core::config::channel_store::ChannelConfig,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) {
    if !config.enabled
        || recovery::runtime_capability(config) != recovery::ChannelRuntimeCapability::Persistent
        || recovery::canonical_platform(&config.kind) == "arkret"
    {
        // Arkret owns its own per-account infinite retry loop so an outer
        // supervisor would duplicate listeners while an account is backing off.
        return;
    }

    abort_channel_supervisor(channel, &config.id).await;
    let channel_id = config.id.clone();
    let channel_for_task = Arc::clone(channel);
    let session_store_for_task = Arc::clone(session_store);
    let handle = tokio::spawn(async move {
        let mut consecutive_failures = 0_u64;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let saved = match savfox_core::config::channel_store::get_channel_config(
                &channel_for_task.config().savfox_home,
                &channel_id,
            )
            .await
            {
                Ok(Some(saved)) if saved.enabled => saved,
                Ok(Some(_) | None) => break,
                Err(err) => {
                    warn!(
                        channel_id,
                        error = %err,
                        "channel supervisor could not reload instance config"
                    );
                    continue;
                }
            };
            if recovery::runtime_capability(&saved)
                != recovery::ChannelRuntimeCapability::Persistent
            {
                break;
            }
            if channel_runtime_alive(&saved, &channel_for_task).await {
                consecutive_failures = 0;
                recovery::mark_channel_alive(&channel_for_task.channel_recovery_registry(), &saved)
                    .await;
                continue;
            }

            consecutive_failures = consecutive_failures.saturating_add(1);
            let error = channel_runtime_last_error(&saved).or_else(|| {
                Some("persistent runtime exited; automatic restart scheduled".to_owned())
            });
            recovery::mark_channel_retrying(
                &channel_for_task.channel_recovery_registry(),
                &saved,
                error,
            )
            .await;
            let retry_delay = supervisor_retry_delay(consecutive_failures);
            warn!(
                channel_id,
                attempt = consecutive_failures,
                retry_delay_ms = retry_delay.as_millis(),
                "channel runtime stopped; supervisor will restart the exact instance"
            );
            tokio::time::sleep(retry_delay).await;

            let saved = match savfox_core::config::channel_store::get_channel_config(
                &channel_for_task.config().savfox_home,
                &channel_id,
            )
            .await
            {
                Ok(Some(saved)) if saved.enabled => saved,
                _ => break,
            };
            let registry = channel_for_task.channel_registry();
            let reports = channel_for_task.channel_recovery_registry();
            recovery::recover_channel_configs(std::slice::from_ref(&saved), &reports, |config| {
                let registry = Arc::clone(&registry);
                let channel = Arc::clone(&channel_for_task);
                let session_store = Arc::clone(&session_store_for_task);
                Box::pin(async move {
                    start_saved_channel(&config, &registry, &channel, &session_store).await
                })
            })
            .await;
            if channel_runtime_alive(&saved, &channel_for_task).await {
                consecutive_failures = 0;
            }
        }
    });
    channel
        .channel_recovery_supervisors()
        .lock()
        .await
        .insert(config.id.clone(), handle);
}

pub(crate) async fn note_channel_started(
    config: &savfox_core::config::channel_store::ChannelConfig,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) {
    recovery::mark_channel_alive(&channel.channel_recovery_registry(), config).await;
    schedule_channel_supervisor(config, channel, session_store).await;
}

fn supervisor_retry_delay(attempt: u64) -> std::time::Duration {
    std::time::Duration::from_secs(attempt.max(1).min(6).pow(2))
}

pub(crate) async fn reconcile_channel_instance(
    config: &savfox_core::config::channel_store::ChannelConfig,
    channel: &Arc<GatewayChannel>,
    session_store: &Arc<SessionStore>,
) -> serde_json::Value {
    let _ = stop_channel_instance(config, channel).await;
    let reports = channel.channel_recovery_registry();
    if !config.enabled {
        recovery::mark_channel_stopped(&reports, config, true).await;
    } else {
        let registry = channel.channel_registry();
        recovery::recover_channel_configs(std::slice::from_ref(config), &reports, |saved| {
            let registry = Arc::clone(&registry);
            let channel = Arc::clone(channel);
            let session_store = Arc::clone(session_store);
            Box::pin(async move {
                start_saved_channel(&saved, &registry, &channel, &session_store).await
            })
        })
        .await;
        schedule_channel_supervisor(config, channel, session_store).await;
    }

    reports
        .read()
        .await
        .get(&config.id)
        .and_then(|report| serde_json::to_value(report).ok())
        .unwrap_or(serde_json::Value::Null)
}

pub(crate) async fn shutdown_all_channel_instances(
    savfox_home: &PathBuf,
    channel: &Arc<GatewayChannel>,
) {
    let configs = match savfox_core::config::channel_store::list_channel_configs(savfox_home).await
    {
        Ok(configs) => configs,
        Err(err) => {
            warn!(error = %err, "gateway shutdown: failed to load channel configs");
            return;
        }
    };
    for config in &configs {
        if let Err(err) = stop_channel_instance(config, channel).await {
            warn!(
                channel_id = %config.id,
                error = %err,
                "gateway shutdown: failed to stop channel instance"
            );
        }
    }
    let supervisors = channel.channel_recovery_supervisors();
    for (_, handle) in supervisors.lock().await.drain() {
        handle.abort();
    }
    channel.channel_registry().write().await.clear();
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
    use crate::channels::dingtalk::{dingtalk_sink, start_dingtalk_stream};

    let dingtalk_config =
        crate::channels::dingtalk::DingtalkChannelConfig::from_channel_config(config)
            .ok_or_else(|| anyhow::anyhow!("Dingtalk channel config must be an object"))?;

    if dingtalk_config.stream_enabled() {
        let sink = dingtalk_sink(
            Arc::clone(channel),
            Arc::clone(session_store),
            config.id.clone(),
        );
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
            config.id.clone(),
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
        let sink = feishu_sink(
            Arc::clone(channel),
            Arc::clone(session_store),
            Some(config.id.clone()),
        );
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

#[cfg(test)]
mod tests {
    use super::supervisor_retry_delay;

    #[test]
    fn supervisor_retry_backoff_is_bounded_and_repeats_forever() {
        assert_eq!(supervisor_retry_delay(1).as_secs(), 1);
        assert_eq!(supervisor_retry_delay(2).as_secs(), 4);
        assert_eq!(supervisor_retry_delay(3).as_secs(), 9);
        assert_eq!(supervisor_retry_delay(6).as_secs(), 36);
        assert_eq!(supervisor_retry_delay(100).as_secs(), 36);
    }
}
