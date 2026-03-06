use std::path::PathBuf;

use anyhow::Context;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct DingtalkOutboundConfig {
    pub webhook_url: Option<String>,
    pub access_token: Option<String>,
    pub secret: Option<String>,
}

impl DingtalkOutboundConfig {
    fn from_channel_config(
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

        if webhook_url.is_none() && access_token.is_none() {
            return None;
        }

        Some(Self {
            webhook_url,
            access_token,
            secret,
        })
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
                Some(text.to_string())
            }
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
        .filter_map(DingtalkOutboundConfig::from_channel_config)
        .next())
}
