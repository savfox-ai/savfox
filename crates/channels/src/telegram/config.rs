use std::path::PathBuf;

use anyhow::Context;

use crate::base::non_empty;

#[derive(Debug, Clone)]
pub struct TelegramChannelConfig {
    pub id: String,
    pub bot_token: Option<String>,
    pub webhook_url: Option<String>,
    pub polling: bool,
}

impl TelegramChannelConfig {
    #[must_use]
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.kind.eq_ignore_ascii_case("telegram") {
            return None;
        }
        let raw = config.config.as_object()?;
        let polling = raw
            .get("polling")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Some(Self {
            id: config.id.clone(),
            bot_token: non_empty(raw, &["bot_token", "botToken", "token"]),
            webhook_url: non_empty(raw, &["webhook_url", "webhookUrl"]),
            polling,
        })
    }

    fn has_outbound_auth(&self) -> bool {
        self.bot_token
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
    }
}

pub async fn resolve_telegram_outbound_token(
    savfox_home: &PathBuf,
) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let token = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(TelegramChannelConfig::from_channel_config)
        .filter(TelegramChannelConfig::has_outbound_auth)
        .find_map(|cfg| cfg.bot_token);
    Ok(token)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::TelegramChannelConfig;

    #[test]
    fn reads_polling_flag_from_channel_config() {
        let config = savfox_core::config::channel_store::ChannelConfig {
            id: "telegram-polling".to_owned(),
            kind: "telegram".to_owned(),
            slug: "telegram".to_owned(),
            name: "Telegram".to_owned(),
            enabled: true,
            config: json!({
                "bot_token": "123:ABC",
                "polling": true
            }),
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        };

        let parsed = TelegramChannelConfig::from_channel_config(&config).expect("parsed");
        assert!(parsed.polling);
        assert_eq!(parsed.bot_token.as_deref(), Some("123:ABC"));
    }
}
