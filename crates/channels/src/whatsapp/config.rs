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
pub struct WhatsAppSavedConfig {
    pub id: String,
    pub phone_number_id: Option<String>,
    pub access_token: Option<String>,
    pub app_secret: Option<String>,
    pub verify_token: Option<String>,
}

impl WhatsAppSavedConfig {
    #[must_use]
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.kind.eq_ignore_ascii_case("whatsapp") {
            return None;
        }
        let raw = config.config.as_object()?;
        Some(Self {
            id: config.id.clone(),
            phone_number_id: non_empty(raw, &["phone_number_id", "phoneNumberId", "phone_id"]),
            access_token: non_empty(
                raw,
                &[
                    "access_token",
                    "accessToken",
                    "token",
                    "bot_token",
                    "botToken",
                ],
            ),
            app_secret: non_empty(raw, &["app_secret", "appSecret", "secret"]),
            verify_token: non_empty(raw, &["verify_token", "verifyToken"]),
        })
    }

    fn has_outbound_auth(&self) -> bool {
        self.phone_number_id
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
            && self
                .access_token
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty())
    }
}

/// Resolve the WhatsApp phone_number_id and access_token from saved channel configs.
pub async fn resolve_whatsapp_outbound_config(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<(String, String)>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(WhatsAppSavedConfig::from_channel_config)
        .filter(WhatsAppSavedConfig::has_outbound_auth)
        .find_map(|cfg| {
            let phone_id = cfg.phone_number_id?;
            let token = cfg.access_token?;
            Some((phone_id, token))
        });
    Ok(result)
}

/// Resolve the WhatsApp app secret from saved channel configs.
pub async fn resolve_whatsapp_app_secret(savfox_home: &PathBuf) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(WhatsAppSavedConfig::from_channel_config)
        .find_map(|cfg| cfg.app_secret);
    Ok(result)
}

/// Resolve the WhatsApp webhook verify token from saved channel configs.
pub async fn resolve_whatsapp_verify_token(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(WhatsAppSavedConfig::from_channel_config)
        .find_map(|cfg| cfg.verify_token);
    Ok(result)
}
