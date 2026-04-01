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
pub struct MattermostChannelConfig {
    pub id: String,
    pub server_url: Option<String>,
    pub access_token: Option<String>,
    pub team_name: Option<String>,
    pub verification_token: Option<String>,
}

impl MattermostChannelConfig {
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.kind.eq_ignore_ascii_case("mattermost") {
            return None;
        }
        let raw = config.config.as_object()?;
        Some(Self {
            id: config.id.clone(),
            server_url: non_empty(raw, &["server_url", "serverUrl"]),
            access_token: non_empty(raw, &["bot_token", "botToken", "token", "access_token"]),
            team_name: non_empty(raw, &["team_name", "teamName"]),
            verification_token: non_empty(
                raw,
                &[
                    "verification_token",
                    "verificationToken",
                    "verify_token",
                    "verifyToken",
                    "webhook_token",
                    "webhookToken",
                ],
            ),
        })
    }

    fn has_outbound_auth(&self) -> bool {
        self.server_url.is_some()
            && self
                .access_token
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty())
    }
}

/// Resolve the Mattermost server URL and access token from saved channel configs.
pub async fn resolve_mattermost_outbound_config(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<(String, String)>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(MattermostChannelConfig::from_channel_config)
        .filter(MattermostChannelConfig::has_outbound_auth)
        .find_map(|cfg| {
            let url = cfg.server_url?;
            let token = cfg.access_token?;
            Some((url, token))
        });
    Ok(result)
}

/// Resolve the Mattermost verification token from saved channel configs.
pub async fn resolve_mattermost_verification_token(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(MattermostChannelConfig::from_channel_config)
        .find_map(|cfg| cfg.verification_token);
    Ok(result)
}
