use std::path::PathBuf;

use anyhow::Context;
use serde_json::Map;

fn non_empty(map: &Map<String, serde_json::Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
    })
}

#[derive(Debug, Clone)]
pub struct SlackChannelConfig {
    pub id: String,
    pub bot_token: Option<String>,
    pub signing_secret: Option<String>,
    pub app_id: Option<String>,
}

impl SlackChannelConfig {
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.kind.eq_ignore_ascii_case("slack") {
            return None;
        }
        let raw = config.config.as_object()?;
        Some(Self {
            id: config.id.clone(),
            bot_token: non_empty(raw, &["bot_token", "botToken", "token"]),
            signing_secret: non_empty(raw, &["signing_secret", "signingSecret"]),
            app_id: non_empty(raw, &["app_id", "appId"]),
        })
    }

    fn has_outbound_auth(&self) -> bool {
        self.bot_token
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
    }
}

pub async fn resolve_slack_outbound_token(savfox_home: &PathBuf) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let token = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(SlackChannelConfig::from_channel_config)
        .filter(SlackChannelConfig::has_outbound_auth)
        .find_map(|cfg| cfg.bot_token);
    Ok(token)
}

pub async fn resolve_slack_signing_secret(savfox_home: &PathBuf) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let secret = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(SlackChannelConfig::from_channel_config)
        .find_map(|cfg| cfg.signing_secret);
    Ok(secret)
}
