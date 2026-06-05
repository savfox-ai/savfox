use std::path::PathBuf;

use anyhow::Context;
use serde_json::Map;

use crate::base::non_empty;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiscordInboundMode {
    Webhook,
    #[default]
    Stream,
}

#[derive(Debug, Clone)]
pub struct DiscordChannelConfig {
    pub id: String,
    pub bot_token: Option<String>,
    pub application_id: Option<String>,
    pub application_public_key: Option<String>,
    pub guild_id: Option<String>,
    pub inbound_mode: DiscordInboundMode,
}

impl DiscordChannelConfig {
    #[must_use]
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.kind.eq_ignore_ascii_case("discord") {
            return None;
        }
        let raw = config.config.as_object()?;
        Some(Self {
            id: config.id.clone(),
            bot_token: non_empty(raw, &["bot_token", "botToken", "token"]),
            application_id: non_empty(raw, &["application_id", "applicationId", "app_id"]),
            application_public_key: non_empty(
                raw,
                &[
                    "application_public_key",
                    "applicationPublicKey",
                    "public_key",
                ],
            ),
            guild_id: non_empty(raw, &["guild_id", "guildId"]),
            inbound_mode: discord_inbound_mode(raw),
        })
    }

    fn has_outbound_auth(&self) -> bool {
        self.bot_token
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
    }

    #[must_use]
    pub fn stream_enabled(&self) -> bool {
        self.inbound_mode == DiscordInboundMode::Stream
    }
}

fn discord_inbound_mode(raw: &Map<String, serde_json::Value>) -> DiscordInboundMode {
    if raw
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
        || raw
            .get("stream_mode")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        || raw
            .get("streamMode")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    {
        return DiscordInboundMode::Stream;
    }

    let mode = non_empty(
        raw,
        &[
            "mode",
            "event_mode",
            "eventMode",
            "inbound_mode",
            "inboundMode",
            "connection_mode",
            "connectionMode",
        ],
    );
    match mode
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("webhook" | "interaction" | "interactions" | "callback") => {
            DiscordInboundMode::Webhook
        }
        Some("stream" | "gateway" | "socket" | "websocket") => DiscordInboundMode::Stream,
        _ => DiscordInboundMode::default(),
    }
}

pub async fn resolve_discord_outbound_token(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let token = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(DiscordChannelConfig::from_channel_config)
        .filter(DiscordChannelConfig::has_outbound_auth)
        .find_map(|cfg| cfg.bot_token);
    Ok(token)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DiscordChannelConfig, DiscordInboundMode};

    fn channel_config(
        config: serde_json::Value,
    ) -> savfox_core::config::channel_store::ChannelConfig {
        savfox_core::config::channel_store::ChannelConfig {
            id: "discord-stream".to_owned(),
            kind: "discord".to_owned(),
            slug: "discord".to_owned(),
            name: "Discord".to_owned(),
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
    fn defaults_to_stream_mode() {
        let parsed = DiscordChannelConfig::from_channel_config(&channel_config(json!({
            "bot_token": "discord-token"
        })))
        .expect("config should parse");

        assert_eq!(parsed.inbound_mode, DiscordInboundMode::Stream);
        assert!(parsed.stream_enabled());
    }

    #[test]
    fn explicit_webhook_mode_disables_stream() {
        let parsed = DiscordChannelConfig::from_channel_config(&channel_config(json!({
            "bot_token": "discord-token",
            "mode": "webhook"
        })))
        .expect("config should parse");

        assert_eq!(parsed.inbound_mode, DiscordInboundMode::Webhook);
        assert!(!parsed.stream_enabled());
    }
}
