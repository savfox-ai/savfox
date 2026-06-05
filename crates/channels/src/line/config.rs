use std::path::PathBuf;

use anyhow::Context;

use crate::base::non_empty;

#[derive(Debug, Clone)]
pub struct LineChannelConfig {
    pub id: String,
    pub channel_token: Option<String>,
    pub channel_secret: Option<String>,
}

impl LineChannelConfig {
    #[must_use]
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.kind.eq_ignore_ascii_case("line") {
            return None;
        }
        let raw = config.config.as_object()?;
        Some(Self {
            id: config.id.clone(),
            channel_token: non_empty(
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
            ),
            channel_secret: non_empty(raw, &["channel_secret", "channelSecret", "secret"]),
        })
    }

    fn has_outbound_auth(&self) -> bool {
        self.channel_token
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
    }
}

/// Resolve the LINE channel token from saved channel configs.
pub async fn resolve_line_outbound_token(savfox_home: &PathBuf) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(LineChannelConfig::from_channel_config)
        .filter(LineChannelConfig::has_outbound_auth)
        .find_map(|cfg| cfg.channel_token);
    Ok(result)
}

/// Resolve the LINE channel secret from saved channel configs.
pub async fn resolve_line_channel_secret(savfox_home: &PathBuf) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(LineChannelConfig::from_channel_config)
        .find_map(|cfg| cfg.channel_secret);
    Ok(result)
}
