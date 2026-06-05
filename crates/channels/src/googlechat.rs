use std::path::PathBuf;

use anyhow::Context;

use crate::base::non_empty;

#[derive(Debug, Clone)]
pub struct GoogleChatChannelConfig {
    pub id: String,
    pub webhook_url: Option<String>,
    pub space_id: Option<String>,
    pub verification_token: Option<String>,
}

impl GoogleChatChannelConfig {
    #[must_use]
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.kind.eq_ignore_ascii_case("googlechat") {
            return None;
        }
        let raw = config.config.as_object()?;
        Some(Self {
            id: config.id.clone(),
            webhook_url: non_empty(
                raw,
                &[
                    "webhook_url",
                    "webhookUrl",
                    "incoming_webhook_url",
                    "incomingWebhookUrl",
                    "url",
                ],
            ),
            space_id: non_empty(raw, &["space_id", "spaceId"]),
            verification_token: non_empty(
                raw,
                &[
                    "verification_token",
                    "verificationToken",
                    "verify_token",
                    "verifyToken",
                ],
            ),
        })
    }

    fn has_outbound_auth(&self) -> bool {
        self.webhook_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    }
}

pub async fn resolve_googlechat_webhook_url(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(GoogleChatChannelConfig::from_channel_config)
        .filter(GoogleChatChannelConfig::has_outbound_auth)
        .find_map(|cfg| cfg.webhook_url);
    Ok(result)
}

pub async fn resolve_googlechat_space_id(savfox_home: &PathBuf) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(GoogleChatChannelConfig::from_channel_config)
        .find_map(|cfg| cfg.space_id);
    Ok(result)
}

pub async fn resolve_googlechat_verification_token(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(GoogleChatChannelConfig::from_channel_config)
        .find_map(|cfg| cfg.verification_token);
    Ok(result)
}

/// Send a message via a Google Chat webhook URL.
pub async fn send_webhook_message(
    client: &reqwest::Client,
    webhook_url: &str,
    text: &str,
) -> anyhow::Result<()> {
    let body = serde_json::json!({ "text": text });

    let response = client
        .post(webhook_url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    crate::http::warn_on_error(response, "Google Chat API error").await;
    Ok(())
}
