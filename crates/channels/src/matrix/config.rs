use std::path::PathBuf;

use anyhow::Context;
use serde_json::Value;
use tracing::warn;

fn default_matrix_homeserver() -> String {
    "https://matrix.org".to_owned()
}

fn default_appservice_user_prefix() -> String {
    "_savfox_".to_owned()
}

fn default_appservice_alias_prefix() -> String {
    "_savfox_".to_owned()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MatrixMode {
    #[default]
    User,
    Appservice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixChannelConfig {
    pub id: String,
    pub mode: MatrixMode,
    pub homeserver: String,
    pub access_token: Option<String>,
    pub password: Option<String>,
    pub device_name: Option<String>,
    pub user_id: Option<String>,
    pub rooms: Vec<String>,
    pub server_name: Option<String>,
    pub public_url: Option<String>,
    pub appservice_id: Option<String>,
    pub appservice_token: Option<String>,
    pub homeserver_token: Option<String>,
    pub sender_localpart: Option<String>,
    pub user_prefix: String,
    pub alias_prefix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatrixConfigSelection {
    RoomMatch,
    SingleConfig,
    DefaultConfig,
    FirstConfig,
}

impl MatrixChannelConfig {
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("matrix") {
            return None;
        }

        let raw = config.config.as_object()?;
        let mode = parse_mode(raw);
        let access_token =
            first_non_empty_config_string(raw, &["accessToken", "access_token", "token"]);
        let password = first_non_empty_config_string(raw, &["password"]);
        let homeserver =
            first_non_empty_config_string(raw, &["homeserver", "homeserver_url", "server_url"])
                .unwrap_or_else(default_matrix_homeserver);
        let user_id = first_non_empty_config_string(raw, &["userId", "user_id"]);
        let device_name = first_non_empty_config_string(raw, &["deviceName", "device_name"]);
        let server_name = first_non_empty_config_string(raw, &["serverName", "server_name"]);
        let public_url = first_non_empty_config_string(raw, &["publicUrl", "public_url"]);
        let appservice_id =
            first_non_empty_config_string(raw, &["appserviceId", "appservice_id", "bridgeId"]);
        let appservice_token = first_non_empty_config_string(
            raw,
            &["appserviceToken", "appservice_token", "asToken", "as_token"],
        );
        let homeserver_token = first_non_empty_config_string(
            raw,
            &["homeserverToken", "homeserver_token", "hsToken", "hs_token"],
        );
        let sender_localpart =
            first_non_empty_config_string(raw, &["senderLocalpart", "sender_localpart"]);
        let user_prefix = first_non_empty_config_string(raw, &["userPrefix", "user_prefix"])
            .unwrap_or_else(default_appservice_user_prefix);
        let alias_prefix = first_non_empty_config_string(raw, &["aliasPrefix", "alias_prefix"])
            .unwrap_or_else(default_appservice_alias_prefix);

        let mut rooms = Vec::new();
        for key in ["groups", "rooms", "roomIds", "room_ids", "room_id"] {
            if let Some(value) = raw.get(key) {
                rooms.extend(parse_string_list(value));
            }
        }
        rooms.sort_unstable();
        rooms.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        Some(Self {
            id: config.id.clone(),
            mode,
            homeserver,
            access_token,
            password,
            device_name,
            user_id,
            rooms,
            server_name,
            public_url,
            appservice_id,
            appservice_token,
            homeserver_token,
            sender_localpart,
            user_prefix,
            alias_prefix,
        })
    }

    #[must_use]
    pub fn has_auth(&self) -> bool {
        match self.mode {
            MatrixMode::User => {
                non_empty_trimmed(self.access_token.as_deref()).is_some()
                    || non_empty_trimmed(self.password.as_deref()).is_some()
            }
            MatrixMode::Appservice => {
                non_empty_trimmed(self.appservice_token.as_deref()).is_some()
                    && non_empty_trimmed(self.homeserver_token.as_deref()).is_some()
            }
        }
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.validate_auth().is_ok()
    }

    pub fn validate_auth(&self) -> anyhow::Result<()> {
        match self.mode {
            MatrixMode::User => {
                if !self.has_auth() {
                    anyhow::bail!("Matrix channel missing access token or password");
                }
                if self.access_token.is_none()
                    && non_empty_trimmed(self.user_id.as_deref()).is_none()
                {
                    anyhow::bail!("Matrix password login requires userId");
                }
            }
            MatrixMode::Appservice => {
                if non_empty_trimmed(self.server_name.as_deref()).is_none() {
                    anyhow::bail!("Matrix appservice mode requires serverName");
                }
                if non_empty_trimmed(self.public_url.as_deref()).is_none() {
                    anyhow::bail!("Matrix appservice mode requires publicUrl");
                }
                if non_empty_trimmed(self.appservice_id.as_deref()).is_none() {
                    anyhow::bail!("Matrix appservice mode requires appserviceId");
                }
                if non_empty_trimmed(self.appservice_token.as_deref()).is_none() {
                    anyhow::bail!("Matrix appservice mode requires appserviceToken");
                }
                if non_empty_trimmed(self.homeserver_token.as_deref()).is_none() {
                    anyhow::bail!("Matrix appservice mode requires homeserverToken");
                }
                if non_empty_trimmed(self.sender_localpart.as_deref()).is_none() {
                    anyhow::bail!("Matrix appservice mode requires senderLocalpart");
                }
            }
        }

        Ok(())
    }

    #[must_use]
    pub fn matches_room(&self, room_id: &str) -> bool {
        let room_id = room_id.trim();
        !room_id.is_empty()
            && self
                .rooms
                .iter()
                .any(|room| room.eq_ignore_ascii_case(room_id))
    }

    #[must_use]
    pub fn bot_user_id(&self) -> Option<String> {
        match self.mode {
            MatrixMode::User => self.user_id.clone(),
            MatrixMode::Appservice => Some(format!(
                "@{}:{}",
                non_empty_trimmed(self.sender_localpart.as_deref())?,
                non_empty_trimmed(self.server_name.as_deref())?
            )),
        }
    }
}

pub async fn load_matrix_channel_configs(
    savfox_home: &PathBuf,
) -> anyhow::Result<Vec<MatrixChannelConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;

    Ok(all_configs
        .iter()
        .filter_map(MatrixChannelConfig::from_channel_config)
        .collect())
}

pub async fn resolve_matrix_outbound_config(
    savfox_home: &PathBuf,
    room_id: &str,
) -> anyhow::Result<Option<MatrixChannelConfig>> {
    let configs: Vec<MatrixChannelConfig> = load_matrix_channel_configs(savfox_home)
        .await?
        .into_iter()
        .filter(MatrixChannelConfig::is_ready)
        .collect();

    let Some((config, selection)) = select_matrix_outbound_config(&configs, room_id) else {
        return Ok(None);
    };

    match selection {
        MatrixConfigSelection::DefaultConfig => {
            warn!(
                room_id,
                config_id = %config.id,
                "Matrix room does not match any configured room allowlist; using default Matrix channel"
            );
        }
        MatrixConfigSelection::FirstConfig => {
            warn!(
                room_id,
                config_id = %config.id,
                "Matrix room does not match configured room allowlists; using first Matrix channel"
            );
        }
        MatrixConfigSelection::RoomMatch | MatrixConfigSelection::SingleConfig => {}
    }

    Ok(Some(config))
}

fn select_matrix_outbound_config(
    configs: &[MatrixChannelConfig],
    room_id: &str,
) -> Option<(MatrixChannelConfig, MatrixConfigSelection)> {
    if configs.is_empty() {
        return None;
    }

    if let Some(config) = configs.iter().find(|config| config.matches_room(room_id)) {
        return Some((config.clone(), MatrixConfigSelection::RoomMatch));
    }

    if configs.len() == 1 {
        return configs
            .first()
            .cloned()
            .map(|config| (config, MatrixConfigSelection::SingleConfig));
    }

    if let Some(config) = configs.iter().find(|config| config.rooms.is_empty()) {
        return Some((config.clone(), MatrixConfigSelection::DefaultConfig));
    }

    configs
        .first()
        .cloned()
        .map(|config| (config, MatrixConfigSelection::FirstConfig))
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

fn parse_mode(map: &serde_json::Map<String, Value>) -> MatrixMode {
    match first_non_empty_config_string(map, &["mode"]) {
        Some(mode) if mode.eq_ignore_ascii_case("appservice") => MatrixMode::Appservice,
        _ => MatrixMode::User,
    }
}

fn non_empty_trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn parse_string_list(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => text
            .split(['\n', ','])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn channel_config(
        id: &str,
        kind: &str,
        enabled: bool,
        config: Value,
    ) -> savfox_core::config::channel_store::ChannelConfig {
        savfox_core::config::channel_store::ChannelConfig {
            id: id.to_owned(),
            kind: kind.to_owned(),
            slug: String::new(),
            name: id.to_owned(),
            enabled,
            config,
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn matrix_config_parses_aliases_and_deduplicates_rooms() {
        let parsed = MatrixChannelConfig::from_channel_config(&channel_config(
            "matrix-main",
            "matrix",
            true,
            json!({
                "server_url": "https://matrix.example",
                "token": "secret",
                "user_id": "@savfox:example.org",
                "device_name": "Savfox Bot",
                "rooms": ["!room-a:example.org", "!room-a:example.org"],
                "groups": "!room-b:example.org,\n!room-c:example.org"
            }),
        ))
        .expect("matrix config should parse");

        assert_eq!(parsed.mode, MatrixMode::User);
        assert_eq!(parsed.homeserver, "https://matrix.example");
        assert_eq!(parsed.access_token.as_deref(), Some("secret"));
        assert_eq!(parsed.user_id.as_deref(), Some("@savfox:example.org"));
        assert_eq!(parsed.device_name.as_deref(), Some("Savfox Bot"));
        assert_eq!(
            parsed.rooms,
            vec![
                "!room-a:example.org".to_owned(),
                "!room-b:example.org".to_owned(),
                "!room-c:example.org".to_owned()
            ]
        );
    }

    #[test]
    fn matrix_config_requires_credentials_for_authentication() {
        let parsed = MatrixChannelConfig::from_channel_config(&channel_config(
            "matrix-main",
            "matrix",
            true,
            json!({
                "homeserver": "https://matrix.example"
            }),
        ))
        .expect("matrix config should parse");

        assert!(!parsed.has_auth());
        assert!(parsed.validate_auth().is_err());
    }

    #[test]
    fn matrix_appservice_mode_requires_registration_fields() {
        let parsed = MatrixChannelConfig::from_channel_config(&channel_config(
            "matrix-appservice",
            "matrix",
            true,
            json!({
                "mode": "appservice",
                "homeserver": "https://matrix.example",
                "serverName": "matrix.example",
                "publicUrl": "https://gateway.example",
                "appserviceId": "savfox-matrix",
                "appserviceToken": "as-token",
                "homeserverToken": "hs-token",
                "senderLocalpart": "_savfox_bot"
            }),
        ))
        .expect("matrix appservice config should parse");

        assert_eq!(parsed.mode, MatrixMode::Appservice);
        assert!(parsed.is_ready());
        assert_eq!(
            parsed.bot_user_id().as_deref(),
            Some("@_savfox_bot:matrix.example")
        );
    }

    #[test]
    fn outbound_selection_prefers_room_match_then_default() {
        let configs = vec![
            MatrixChannelConfig {
                id: "default".to_owned(),
                mode: MatrixMode::User,
                homeserver: "https://matrix.example".to_owned(),
                access_token: Some("token-a".to_owned()),
                password: None,
                device_name: None,
                user_id: Some("@bot:example.org".to_owned()),
                rooms: Vec::new(),
                server_name: None,
                public_url: None,
                appservice_id: None,
                appservice_token: None,
                homeserver_token: None,
                sender_localpart: None,
                user_prefix: default_appservice_user_prefix(),
                alias_prefix: default_appservice_alias_prefix(),
            },
            MatrixChannelConfig {
                id: "room-specific".to_owned(),
                mode: MatrixMode::User,
                homeserver: "https://matrix.example".to_owned(),
                access_token: Some("token-b".to_owned()),
                password: None,
                device_name: None,
                user_id: Some("@bot:example.org".to_owned()),
                rooms: vec!["!ops:example.org".to_owned()],
                server_name: None,
                public_url: None,
                appservice_id: None,
                appservice_token: None,
                homeserver_token: None,
                sender_localpart: None,
                user_prefix: default_appservice_user_prefix(),
                alias_prefix: default_appservice_alias_prefix(),
            },
        ];

        let selected = select_matrix_outbound_config(&configs, "!ops:example.org")
            .expect("room-specific config should match");
        assert_eq!(selected.0.id, "room-specific");
        assert_eq!(selected.1, MatrixConfigSelection::RoomMatch);

        let fallback = select_matrix_outbound_config(&configs, "!other:example.org")
            .expect("default config should be used");
        assert_eq!(fallback.0.id, "default");
        assert_eq!(fallback.1, MatrixConfigSelection::DefaultConfig);
    }
}
