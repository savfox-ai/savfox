use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use savfox_core::config::channel_store::ChannelConfig;

pub(crate) type ChannelRecoveryRegistry = Arc<RwLock<HashMap<String, ChannelRecoveryReport>>>;
pub(crate) type ChannelRecoverySupervisors =
    Arc<tokio::sync::Mutex<HashMap<String, JoinHandle<()>>>>;

pub(crate) type ChannelStartFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChannelRuntimeCapability {
    Persistent,
    Webhook,
    UnsupportedRuntime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChannelRecoveryPhase {
    Disabled,
    Starting,
    Ready,
    Retrying,
    Failed,
    Stopped,
    UnsupportedRuntime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChannelHealthState {
    Configured,
    Listening,
    Connected,
    Degraded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ChannelRecoveryReport {
    pub(crate) id: String,
    pub(crate) platform: String,
    pub(crate) capability: ChannelRuntimeCapability,
    pub(crate) phase: ChannelRecoveryPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) health_state: Option<ChannelHealthState>,
    pub(crate) attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_error: Option<String>,
    pub(crate) updated_at: String,
}

impl ChannelRecoveryReport {
    fn new(
        config: &ChannelConfig,
        capability: ChannelRuntimeCapability,
        phase: ChannelRecoveryPhase,
        health_state: Option<ChannelHealthState>,
    ) -> Self {
        Self {
            id: config.id.clone(),
            platform: canonical_platform(&config.kind),
            capability,
            phase,
            health_state,
            attempts: 0,
            last_error: None,
            updated_at: now_rfc3339(),
        }
    }
}

pub(crate) fn create_channel_recovery_registry() -> ChannelRecoveryRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

pub(crate) fn create_channel_recovery_supervisors() -> ChannelRecoverySupervisors {
    Arc::new(tokio::sync::Mutex::new(HashMap::new()))
}

pub(crate) fn canonical_platform(kind: &str) -> String {
    match kind.trim().to_ascii_lowercase().as_str() {
        "lark" => "feishu".to_owned(),
        other => other.to_owned(),
    }
}

pub(crate) fn runtime_capability(config: &ChannelConfig) -> ChannelRuntimeCapability {
    match canonical_platform(&config.kind).as_str() {
        "matrix" => ChannelRuntimeCapability::Persistent,
        "arkret" => {
            #[cfg(feature = "arkret")]
            {
                ChannelRuntimeCapability::Persistent
            }
            #[cfg(not(feature = "arkret"))]
            {
                ChannelRuntimeCapability::UnsupportedRuntime
            }
        }
        "discord" => {
            if savfox_channels::discord::DiscordChannelConfig::from_channel_config(config)
                .is_some_and(|parsed| parsed.stream_enabled())
            {
                ChannelRuntimeCapability::Persistent
            } else {
                ChannelRuntimeCapability::Webhook
            }
        }
        "telegram" => {
            if savfox_channels::telegram::TelegramChannelConfig::from_channel_config(config)
                .is_some_and(|parsed| parsed.polling)
            {
                ChannelRuntimeCapability::Persistent
            } else {
                ChannelRuntimeCapability::Webhook
            }
        }
        "dingtalk" => {
            if savfox_channels::dingtalk::DingtalkChannelConfig::from_channel_config(config)
                .is_some_and(|parsed| parsed.stream_enabled())
            {
                ChannelRuntimeCapability::Persistent
            } else {
                ChannelRuntimeCapability::Webhook
            }
        }
        "feishu" => {
            if savfox_channels::feishu::FeishuChannelConfig::from_channel_config(config)
                .is_some_and(|parsed| parsed.stream_enabled())
            {
                ChannelRuntimeCapability::Persistent
            } else {
                ChannelRuntimeCapability::Webhook
            }
        }
        "slack" | "mattermost" | "googlechat" | "line" | "whatsapp" | "qq" | "wechat"
        | "msteams" | "signal" | "irc" | "zalo" | "webhook" => ChannelRuntimeCapability::Webhook,
        _ => ChannelRuntimeCapability::UnsupportedRuntime,
    }
}

pub(crate) async fn recover_channel_configs<F>(
    configs: &[ChannelConfig],
    reports: &ChannelRecoveryRegistry,
    mut start: F,
) where
    F: FnMut(ChannelConfig) -> ChannelStartFuture,
{
    for config in configs {
        let capability = runtime_capability(config);
        if !config.enabled {
            reports.write().await.insert(
                config.id.clone(),
                ChannelRecoveryReport::new(
                    config,
                    capability,
                    ChannelRecoveryPhase::Disabled,
                    None,
                ),
            );
            continue;
        }

        if capability == ChannelRuntimeCapability::UnsupportedRuntime {
            let mut report = ChannelRecoveryReport::new(
                config,
                capability,
                ChannelRecoveryPhase::UnsupportedRuntime,
                Some(ChannelHealthState::Degraded),
            );
            report.last_error = Some(format!(
                "channel type '{}' has no supported gateway runtime",
                canonical_platform(&config.kind)
            ));
            reports.write().await.insert(config.id.clone(), report);
            continue;
        }

        {
            let mut reports = reports.write().await;
            let report = reports.entry(config.id.clone()).or_insert_with(|| {
                ChannelRecoveryReport::new(config, capability, ChannelRecoveryPhase::Starting, None)
            });
            report.platform = canonical_platform(&config.kind);
            report.capability = capability;
            report.phase = ChannelRecoveryPhase::Starting;
            report.health_state = None;
            report.attempts = report.attempts.saturating_add(1);
            report.last_error = None;
            report.updated_at = now_rfc3339();
        }

        match start(config.clone()).await {
            Ok(()) => {
                let health_state = match capability {
                    ChannelRuntimeCapability::Persistent => ChannelHealthState::Listening,
                    ChannelRuntimeCapability::Webhook => ChannelHealthState::Configured,
                    ChannelRuntimeCapability::UnsupportedRuntime => unreachable!(),
                };
                if let Some(report) = reports.write().await.get_mut(&config.id) {
                    report.phase = ChannelRecoveryPhase::Ready;
                    report.health_state = Some(health_state);
                    report.last_error = None;
                    report.updated_at = now_rfc3339();
                }
            }
            Err(err) => {
                if let Some(report) = reports.write().await.get_mut(&config.id) {
                    report.phase = ChannelRecoveryPhase::Failed;
                    report.health_state = Some(ChannelHealthState::Degraded);
                    report.last_error = Some(err.to_string());
                    report.updated_at = now_rfc3339();
                }
            }
        }
    }
}

pub(crate) async fn mark_channel_stopped(
    reports: &ChannelRecoveryRegistry,
    config: &ChannelConfig,
    disabled: bool,
) {
    let capability = runtime_capability(config);
    let phase = if disabled {
        ChannelRecoveryPhase::Disabled
    } else {
        ChannelRecoveryPhase::Stopped
    };
    let health_state = (!disabled).then_some(ChannelHealthState::Degraded);
    let mut report = ChannelRecoveryReport::new(config, capability, phase, health_state);
    if !disabled {
        report.last_error = Some("channel runtime was stopped".to_owned());
    }
    reports.write().await.insert(config.id.clone(), report);
}

pub(crate) async fn mark_channel_retrying(
    reports: &ChannelRecoveryRegistry,
    config: &ChannelConfig,
    last_error: Option<String>,
) {
    let capability = runtime_capability(config);
    let mut reports = reports.write().await;
    let report = reports.entry(config.id.clone()).or_insert_with(|| {
        ChannelRecoveryReport::new(
            config,
            capability,
            ChannelRecoveryPhase::Retrying,
            Some(ChannelHealthState::Degraded),
        )
    });
    report.platform = canonical_platform(&config.kind);
    report.capability = capability;
    report.phase = ChannelRecoveryPhase::Retrying;
    report.health_state = Some(ChannelHealthState::Degraded);
    if let Some(last_error) = last_error {
        report.last_error = Some(last_error);
    }
    report.updated_at = now_rfc3339();
}

pub(crate) async fn mark_channel_alive(reports: &ChannelRecoveryRegistry, config: &ChannelConfig) {
    if let Some(report) = reports.write().await.get_mut(&config.id) {
        report.phase = ChannelRecoveryPhase::Ready;
        report.health_state = Some(ChannelHealthState::Listening);
        report.last_error = None;
        report.updated_at = now_rfc3339();
    }
}

pub(crate) async fn remove_channel_report(reports: &ChannelRecoveryRegistry, channel_id: &str) {
    reports.write().await.remove(channel_id);
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use savfox_core::config::channel_store::ChannelConfig;
    use serde_json::json;

    use super::{
        ChannelHealthState, ChannelRecoveryPhase, ChannelRuntimeCapability,
        create_channel_recovery_registry, mark_channel_stopped, recover_channel_configs,
        remove_channel_report, runtime_capability,
    };

    fn config(id: &str, kind: &str, enabled: bool, value: serde_json::Value) -> ChannelConfig {
        ChannelConfig {
            id: id.to_owned(),
            kind: kind.to_owned(),
            slug: id.to_owned(),
            name: id.to_owned(),
            enabled,
            config: value,
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn recovery_enumerates_same_type_instances_and_isolates_failure() {
        let configs = vec![
            config(
                "telegram-a",
                "telegram",
                true,
                json!({"bot_token": "a", "polling": true}),
            ),
            config(
                "telegram-b",
                "telegram",
                true,
                json!({"bot_token": "b", "polling": true}),
            ),
        ];
        let attempted = Arc::new(Mutex::new(Vec::new()));
        let attempted_for_start = Arc::clone(&attempted);
        let reports = create_channel_recovery_registry();

        recover_channel_configs(&configs, &reports, move |config| {
            let attempted = Arc::clone(&attempted_for_start);
            Box::pin(async move {
                attempted
                    .lock()
                    .expect("attempt list lock")
                    .push(config.id.clone());
                if config.id == "telegram-a" {
                    anyhow::bail!("bad credentials");
                }
                Ok(())
            })
        })
        .await;

        assert_eq!(
            attempted
                .lock()
                .expect("attempt list lock")
                .iter()
                .cloned()
                .collect::<HashSet<_>>(),
            HashSet::from(["telegram-a".to_owned(), "telegram-b".to_owned()])
        );
        let reports = reports.read().await;
        assert_eq!(
            reports.get("telegram-a").map(|report| report.phase),
            Some(ChannelRecoveryPhase::Failed)
        );
        assert_eq!(
            reports.get("telegram-b").map(|report| report.phase),
            Some(ChannelRecoveryPhase::Ready)
        );
        assert_eq!(
            reports
                .get("telegram-b")
                .and_then(|report| report.health_state),
            Some(ChannelHealthState::Listening)
        );
    }

    #[tokio::test]
    async fn recovery_reports_disabled_and_unsupported_without_starting_them() {
        let configs = vec![
            config("disabled", "telegram", false, json!({"bot_token": "a"})),
            config("unsupported", "nextcloud", true, json!({"token": "b"})),
        ];
        let attempted = Arc::new(Mutex::new(Vec::new()));
        let attempted_for_start = Arc::clone(&attempted);
        let reports = create_channel_recovery_registry();

        recover_channel_configs(&configs, &reports, move |config| {
            let attempted = Arc::clone(&attempted_for_start);
            Box::pin(async move {
                attempted
                    .lock()
                    .expect("attempt list lock")
                    .push(config.id.clone());
                Ok(())
            })
        })
        .await;

        assert!(attempted.lock().expect("attempt list lock").is_empty());
        let reports = reports.read().await;
        assert_eq!(
            reports.get("disabled").map(|report| report.phase),
            Some(ChannelRecoveryPhase::Disabled)
        );
        assert_eq!(
            reports.get("unsupported").map(|report| report.phase),
            Some(ChannelRecoveryPhase::UnsupportedRuntime)
        );
    }

    #[test]
    fn capability_matrix_distinguishes_webhook_persistent_and_unsupported_modes() {
        assert_eq!(
            runtime_capability(&config(
                "discord-stream",
                "discord",
                true,
                json!({"bot_token": "token", "event_mode": "gateway"})
            )),
            ChannelRuntimeCapability::Persistent
        );
        assert_eq!(
            runtime_capability(&config(
                "discord-webhook",
                "discord",
                true,
                json!({"bot_token": "token", "event_mode": "webhook"})
            )),
            ChannelRuntimeCapability::Webhook
        );
        assert_eq!(
            runtime_capability(&config(
                "future",
                "future-channel",
                true,
                json!({"token": "token"})
            )),
            ChannelRuntimeCapability::UnsupportedRuntime
        );
    }

    #[tokio::test]
    async fn disable_and_delete_reports_only_touch_target_instance() {
        let configs = vec![
            config("telegram-a", "telegram", true, json!({"bot_token": "a"})),
            config("telegram-b", "telegram", true, json!({"bot_token": "b"})),
        ];
        let reports = create_channel_recovery_registry();
        recover_channel_configs(&configs, &reports, |_| Box::pin(async { Ok(()) })).await;

        mark_channel_stopped(&reports, &configs[0], true).await;
        {
            let snapshot = reports.read().await;
            assert_eq!(
                snapshot.get("telegram-a").map(|report| report.phase),
                Some(ChannelRecoveryPhase::Disabled)
            );
            assert_eq!(
                snapshot.get("telegram-b").map(|report| report.phase),
                Some(ChannelRecoveryPhase::Ready)
            );
        }

        remove_channel_report(&reports, "telegram-a").await;
        let snapshot = reports.read().await;
        assert!(!snapshot.contains_key("telegram-a"));
        assert_eq!(
            snapshot.get("telegram-b").map(|report| report.phase),
            Some(ChannelRecoveryPhase::Ready)
        );
    }
}
