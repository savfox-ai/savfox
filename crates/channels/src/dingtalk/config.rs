use std::path::PathBuf;

use anyhow::Context;
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DingtalkInboundMode {
    Webhook,
    #[default]
    Stream,
}

#[derive(Debug, Clone)]
pub struct DingtalkChannelConfig {
    pub openapi_host: String,
    pub webhook_url: Option<String>,
    pub access_token: Option<String>,
    pub secret: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub inbound_mode: DingtalkInboundMode,
    pub stream_auto_reconnect: bool,
    pub stream_reconnect_interval_secs: u64,
    pub stream_keep_alive_idle_secs: u64,
}

#[derive(Debug, Clone)]
pub struct DingtalkOutboundConfig {
    pub webhook_url: Option<String>,
    pub access_token: Option<String>,
    pub secret: Option<String>,
}

impl DingtalkChannelConfig {
    #[must_use]
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("dingtalk") {
            return None;
        }

        let raw = config.config.as_object()?;
        let webhook_url = first_non_empty_config_string(
            raw,
            &["webhook", "webhook_url", "robot_webhook", "webhookUrl"],
        );
        let access_token = first_non_empty_config_string(
            raw,
            &["access_token", "accessToken", "token", "robot_token"],
        );
        let secret =
            first_non_empty_config_string(raw, &["secret", "sign_secret", "webhook_secret"]);
        let client_id = first_non_empty_config_string(
            raw,
            &[
                "clientId",
                "client_id",
                "appKey",
                "app_key",
                "appId",
                "app_id",
            ],
        );
        let client_secret = first_non_empty_config_string(
            raw,
            &[
                "clientSecret",
                "client_secret",
                "appSecret",
                "app_secret",
                "appKeySecret",
                "app_key_secret",
            ],
        );
        let inbound_mode = dingtalk_inbound_mode(raw);
        let stream_auto_reconnect =
            first_config_bool(raw, &["streamAutoReconnect", "stream_auto_reconnect"])
                .unwrap_or(true);
        let stream_reconnect_interval_secs = first_config_u64(
            raw,
            &[
                "streamReconnectIntervalSecs",
                "stream_reconnect_interval_secs",
                "streamReconnectIntervalSeconds",
            ],
        )
        .unwrap_or(3);
        let stream_keep_alive_idle_secs = first_config_u64(
            raw,
            &[
                "streamKeepAliveIdleSecs",
                "stream_keep_alive_idle_secs",
                "streamKeepAliveIdleSeconds",
            ],
        )
        .unwrap_or(120);

        if webhook_url.is_none()
            && access_token.is_none()
            && (client_id.is_none() || client_secret.is_none())
        {
            return None;
        }

        Some(Self {
            openapi_host: first_non_empty_config_string(
                raw,
                &["openapiHost", "openapi_host", "baseUrl", "base_url"],
            )
            .unwrap_or_else(|| "https://api.dingtalk.com".to_owned()),
            webhook_url,
            access_token,
            secret,
            client_id,
            client_secret,
            inbound_mode,
            stream_auto_reconnect,
            stream_reconnect_interval_secs,
            stream_keep_alive_idle_secs,
        })
    }

    #[must_use]
    pub fn stream_enabled(&self) -> bool {
        self.inbound_mode == DingtalkInboundMode::Stream
            && self
                .client_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            && self
                .client_secret
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
    }
}

impl DingtalkOutboundConfig {
    fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        let channel = DingtalkChannelConfig::from_channel_config(config)?;
        if channel.webhook_url.is_none() && channel.access_token.is_none() {
            return None;
        }

        Some(Self {
            webhook_url: channel.webhook_url,
            access_token: channel.access_token,
            secret: channel.secret,
        })
    }
}

fn dingtalk_inbound_mode(raw: &serde_json::Map<String, Value>) -> DingtalkInboundMode {
    if first_config_bool(raw, &["streamMode", "stream_mode", "stream"]).unwrap_or(false) {
        return DingtalkInboundMode::Stream;
    }

    let mode = first_non_empty_config_string(
        raw,
        &[
            "eventMode",
            "event_mode",
            "connectionMode",
            "connection_mode",
            "receiveMode",
            "receive_mode",
        ],
    );
    match mode
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("webhook" | "callback") => DingtalkInboundMode::Webhook,
        Some("stream" | "long_connection" | "long-connection" | "longconnection") => {
            DingtalkInboundMode::Stream
        }
        _ => DingtalkInboundMode::default(),
    }
}

fn first_non_empty_config_string(
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

fn first_config_bool(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(Value::as_bool))
}

fn first_config_u64(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
                .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
        })
    })
}

pub async fn resolve_dingtalk_outbound_config(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<DingtalkOutboundConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    Ok(all_configs
        .iter()
        .find_map(DingtalkOutboundConfig::from_channel_config))
}

pub async fn load_dingtalk_channel_config(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<DingtalkChannelConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    Ok(all_configs
        .iter()
        .find_map(DingtalkChannelConfig::from_channel_config))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DingtalkChannelConfig, DingtalkInboundMode};

    fn channel_config(
        config: serde_json::Value,
    ) -> savfox_core::config::channel_store::ChannelConfig {
        savfox_core::config::channel_store::ChannelConfig {
            id: "dingtalk-test".to_owned(),
            kind: "dingtalk".to_owned(),
            slug: "dingtalk-test".to_owned(),
            name: "dingtalk-test".to_owned(),
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
    fn stream_mode_defaults_when_client_credentials_exist() {
        let config = DingtalkChannelConfig::from_channel_config(&channel_config(json!({
            "clientId": "ding-cli",
            "clientSecret": "secret"
        })))
        .expect("config should parse");

        assert_eq!(config.inbound_mode, DingtalkInboundMode::Stream);
        assert!(config.stream_auto_reconnect);
        assert_eq!(config.stream_reconnect_interval_secs, 3);
        assert_eq!(config.stream_keep_alive_idle_secs, 120);
        assert!(config.stream_enabled());
    }

    #[test]
    fn webhook_mode_overrides_stream_default() {
        let config = DingtalkChannelConfig::from_channel_config(&channel_config(json!({
            "clientId": "ding-cli",
            "clientSecret": "secret",
            "eventMode": "webhook"
        })))
        .expect("config should parse");

        assert_eq!(config.inbound_mode, DingtalkInboundMode::Webhook);
        assert!(!config.stream_enabled());
    }

    #[test]
    fn same_type_instances_keep_their_own_credentials() {
        let first = DingtalkChannelConfig::from_channel_config(&channel_config(json!({
            "clientId": "first-client",
            "clientSecret": "first-secret"
        })))
        .expect("first config should parse");
        let second = DingtalkChannelConfig::from_channel_config(&channel_config(json!({
            "clientId": "second-client",
            "clientSecret": "second-secret"
        })))
        .expect("second config should parse");

        assert_eq!(first.client_id.as_deref(), Some("first-client"));
        assert_eq!(first.client_secret.as_deref(), Some("first-secret"));
        assert_eq!(second.client_id.as_deref(), Some("second-client"));
        assert_eq!(second.client_secret.as_deref(), Some("second-secret"));
    }
}
