use std::path::PathBuf;

use anyhow::Context;
use feishu_sdk::core::{Config as FeishuSdkConfig, FEISHU_BASE_URL, LARK_BASE_URL, LogLevel};
use feishu_sdk::ws::StreamConfig;
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FeishuInboundMode {
    Webhook,
    #[default]
    Stream,
}

#[derive(Debug, Clone)]
pub struct FeishuChannelConfig {
    pub kind: String,
    pub base_url: String,
    pub app_access_token: Option<String>,
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub verification_token: Option<String>,
    pub encrypt_key: Option<String>,
    pub receive_id_type: String,
    pub inbound_mode: FeishuInboundMode,
    pub stream_locale: Option<String>,
    pub stream_auto_reconnect: bool,
    pub stream_reconnect_count: i32,
    pub stream_reconnect_interval_secs: u64,
    pub stream_ping_interval_secs: u64,
}

impl FeishuChannelConfig {
    #[must_use]
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled
            || (!config.kind.eq_ignore_ascii_case("feishu")
                && !config.kind.eq_ignore_ascii_case("lark"))
        {
            return None;
        }

        let raw = config.config.as_object()?;
        let app_access_token = first_non_empty_config_string(
            raw,
            &[
                "appAccessToken",
                "app_access_token",
                "tenantAccessToken",
                "tenant_access_token",
                "access_token",
                "token",
            ],
        );
        let app_id = first_non_empty_config_string(raw, &["appId", "app_id"]);
        let app_secret = first_non_empty_config_string(raw, &["appSecret", "app_secret"]);
        let verification_token = first_non_empty_config_string(
            raw,
            &["verificationToken", "verification_token", "verify_token"],
        );
        let encrypt_key =
            first_non_empty_config_string(raw, &["encryptKey", "encrypt_key", "event_encrypt_key"]);
        let receive_id_type =
            first_non_empty_config_string(raw, &["receiveIdType", "receive_id_type", "id_type"])
                .unwrap_or_else(|| "chat_id".to_owned());
        let inbound_mode = feishu_inbound_mode(raw);
        let stream_locale =
            first_non_empty_config_string(raw, &["streamLocale", "stream_locale", "locale"]);
        let stream_defaults = default_stream_config();
        let stream_auto_reconnect =
            first_config_bool(raw, &["streamAutoReconnect", "stream_auto_reconnect"])
                .unwrap_or(stream_defaults.auto_reconnect);
        let stream_reconnect_count =
            first_config_i32(raw, &["streamReconnectCount", "stream_reconnect_count"])
                .unwrap_or(stream_defaults.reconnect_count);
        let stream_reconnect_interval_secs = first_config_u64(
            raw,
            &[
                "streamReconnectIntervalSecs",
                "stream_reconnect_interval_secs",
                "streamReconnectIntervalSeconds",
            ],
        )
        .unwrap_or(stream_defaults.reconnect_interval.as_secs());
        let stream_ping_interval_secs = first_config_u64(
            raw,
            &[
                "streamPingIntervalSecs",
                "stream_ping_interval_secs",
                "streamPingIntervalSeconds",
            ],
        )
        .unwrap_or(stream_defaults.ping_interval.as_secs());

        Some(Self {
            kind: config.kind.clone(),
            base_url: configured_base_url(&config.kind, raw),
            app_access_token,
            app_id,
            app_secret,
            verification_token,
            encrypt_key,
            receive_id_type,
            inbound_mode,
            stream_locale,
            stream_auto_reconnect,
            stream_reconnect_count,
            stream_reconnect_interval_secs,
            stream_ping_interval_secs,
        })
    }

    fn has_outbound_auth(&self) -> bool {
        self.app_access_token
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || (self
                .app_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                && self
                    .app_secret
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()))
    }

    #[must_use]
    pub fn stream_enabled(&self) -> bool {
        self.inbound_mode == FeishuInboundMode::Stream
    }
}

#[derive(Debug, Clone)]
pub struct FeishuOutboundConfig {
    pub base_url: String,
    pub app_access_token: Option<String>,
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub receive_id_type: String,
}

fn default_base_url(kind: &str) -> &'static str {
    if kind.eq_ignore_ascii_case("lark") {
        LARK_BASE_URL
    } else {
        FEISHU_BASE_URL
    }
}

pub(crate) fn default_stream_locale(kind: &str) -> &'static str {
    if kind.eq_ignore_ascii_case("lark") {
        "en"
    } else {
        "zh"
    }
}

fn default_stream_config() -> StreamConfig {
    StreamConfig::default()
}

fn configured_base_url(kind: &str, raw: &serde_json::Map<String, Value>) -> String {
    first_non_empty_config_string(raw, &["baseUrl", "base_url", "apiBaseUrl", "api_base_url"])
        .unwrap_or_else(|| default_base_url(kind).to_owned())
}

fn feishu_inbound_mode(raw: &serde_json::Map<String, Value>) -> FeishuInboundMode {
    if first_config_bool(raw, &["streamMode", "stream_mode", "stream"]).unwrap_or(false) {
        return FeishuInboundMode::Stream;
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
        Some("stream" | "long_connection" | "long-connection" | "longconnection") => {
            FeishuInboundMode::Stream
        }
        Some("webhook" | "callback" | "callback_url") => FeishuInboundMode::Webhook,
        _ => FeishuInboundMode::default(),
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

fn first_config_i32(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<i32> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            value
                .as_i64()
                .and_then(|number| i32::try_from(number).ok())
                .or_else(|| value.as_str()?.trim().parse::<i32>().ok())
        })
    })
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

pub async fn resolve_feishu_outbound_config(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<FeishuOutboundConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let feishu_configs: Vec<FeishuChannelConfig> = all_configs
        .iter()
        .filter_map(FeishuChannelConfig::from_channel_config)
        .filter(FeishuChannelConfig::has_outbound_auth)
        .collect();
    if feishu_configs.is_empty() {
        return Ok(None);
    }
    let pick = feishu_configs
        .iter()
        .find(|cfg| {
            cfg.app_access_token
                .as_deref()
                .is_some_and(|token| !token.trim().is_empty())
        })
        .or_else(|| {
            feishu_configs.iter().find(|cfg| {
                cfg.app_id
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                    && cfg
                        .app_secret
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
            })
        })
        .or_else(|| feishu_configs.first())
        .cloned();

    Ok(pick.map(|cfg| FeishuOutboundConfig {
        base_url: cfg.base_url,
        app_access_token: cfg.app_access_token,
        app_id: cfg.app_id,
        app_secret: cfg.app_secret,
        receive_id_type: cfg.receive_id_type,
    }))
}

pub async fn load_feishu_channel_config(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<FeishuChannelConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    Ok(all_configs
        .iter()
        .find_map(FeishuChannelConfig::from_channel_config))
}

pub async fn fetch_feishu_tenant_access_token(
    http_client: &reqwest::Client,
    base_url: &str,
    app_id: &str,
    app_secret: &str,
) -> anyhow::Result<String> {
    let response = http_client
        .post(format!(
            "{}/open-apis/auth/v3/tenant_access_token/internal",
            base_url.trim_end_matches('/')
        ))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "app_id": app_id,
            "app_secret": app_secret,
        }))
        .send()
        .await
        .context("failed to call Feishu tenant_access_token API")?;

    let status = response.status();
    let body = response.bytes().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "Feishu tenant access token API error: HTTP {}: {}",
            status,
            String::from_utf8_lossy(&body)
        );
    }

    let parsed: Value = serde_json::from_slice(&body)
        .context("failed to parse Feishu tenant access token response")?;
    let code = parsed.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        let msg = parsed
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        anyhow::bail!("Feishu tenant access token API returned code {code}: {msg}");
    }
    let token = parsed
        .get("tenant_access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Feishu tenant access token response missing token"))?;
    Ok(token.to_owned())
}

pub(crate) fn build_feishu_sdk_config(
    config: &FeishuChannelConfig,
) -> anyhow::Result<FeishuSdkConfig> {
    let app_id = config
        .app_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Feishu stream mode requires app_id"))?;
    let app_secret = config
        .app_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Feishu stream mode requires app_secret"))?;

    Ok(FeishuSdkConfig::builder(app_id, app_secret)
        .base_url(config.base_url.clone())
        .log_level(LogLevel::Info)
        .build())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{FeishuChannelConfig, FeishuInboundMode};

    fn channel_config(
        config: serde_json::Value,
    ) -> savfox_core::config::channel_store::ChannelConfig {
        savfox_core::config::channel_store::ChannelConfig {
            id: "feishu-test".to_owned(),
            kind: "feishu".to_owned(),
            slug: "feishu-test".to_owned(),
            name: "feishu-test".to_owned(),
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
    fn stream_fields_fall_back_to_sdk_defaults() {
        let config = FeishuChannelConfig::from_channel_config(&channel_config(json!({
            "appId": "cli_a",
            "appSecret": "secret_a"
        })))
        .expect("config should parse");

        assert_eq!(config.inbound_mode, FeishuInboundMode::Stream);
        assert!(config.stream_auto_reconnect);
        assert_eq!(config.stream_reconnect_count, -1);
        assert_eq!(config.stream_reconnect_interval_secs, 120);
        assert_eq!(config.stream_ping_interval_secs, 120);
    }

    #[test]
    fn stream_fields_accept_explicit_values() {
        let config = FeishuChannelConfig::from_channel_config(&channel_config(json!({
            "appId": "cli_a",
            "appSecret": "secret_a",
            "eventMode": "stream",
            "streamAutoReconnect": false,
            "streamReconnectCount": 3,
            "streamReconnectIntervalSecs": 15,
            "streamPingIntervalSecs": 45
        })))
        .expect("config should parse");

        assert!(!config.stream_auto_reconnect);
        assert_eq!(config.stream_reconnect_count, 3);
        assert_eq!(config.stream_reconnect_interval_secs, 15);
        assert_eq!(config.stream_ping_interval_secs, 45);
    }

    #[test]
    fn explicit_webhook_mode_overrides_stream_default() {
        let config = FeishuChannelConfig::from_channel_config(&channel_config(json!({
            "appId": "cli_a",
            "appSecret": "secret_a",
            "eventMode": "webhook"
        })))
        .expect("config should parse");

        assert_eq!(config.inbound_mode, FeishuInboundMode::Webhook);
    }
}
