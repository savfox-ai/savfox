use std::collections::HashMap;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct ChannelEntry {
    pub platform: String,
    pub name: String,
    pub status: Option<String>,
    pub connected_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChannelsResponse {
    pub channels: Vec<ChannelEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChannelsStatusSnapshot {
    pub ts: Option<i64>,
    pub channels: Option<HashMap<String, ChannelStatusSnapshot>>,
    pub instances: Option<HashMap<String, ChannelInstanceStatusSnapshot>>,
}

/// Common status fields returned for each platform by `channels.status`.
///
/// Platform-specific fields are intentionally ignored during deserialization;
/// consumers of the aggregate status response only need this common subset.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ChannelStatusSnapshot {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub saved: Option<bool>,
    pub enabled: Option<bool>,
    pub id: Option<String>,
    pub channel_name: Option<String>,
    pub slug: Option<String>,
    pub bot_username: Option<String>,
    pub user_id: Option<String>,
    #[serde(alias = "lastError")]
    pub last_error: Option<String>,
    pub accounts: Option<Vec<ChannelAccountSnapshot>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ChannelInstanceStatusSnapshot {
    pub platform: String,
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub enabled: Option<bool>,
    pub id: Option<String>,
    pub channel_name: Option<String>,
    pub slug: Option<String>,
    pub runtime_capability: Option<String>,
    pub recovery_phase: Option<String>,
    pub health_state: Option<String>,
    pub startup_attempts: Option<u32>,
    pub startup_updated_at: Option<String>,
    pub runtime_phase: Option<String>,
    pub runtime_attempts: Option<u32>,
    pub runtime_ready: Option<bool>,
    pub last_reason_code: Option<String>,
    pub authority_status: Option<String>,
    pub local_requested_scope: Option<Vec<String>>,
    pub verified_authorization_scope: Option<Vec<String>>,
    pub missing_required_actions: Option<Vec<String>>,
    #[serde(alias = "lastError")]
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ChannelAccountSnapshot {
    #[serde(default, alias = "accountId")]
    pub account_id: String,
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub last_inbound_at: Option<i64>,
    pub last_outbound_at: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DiscordStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub guild_count: Option<u32>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
    pub bot_username: Option<String>,
    pub bot_avatar_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TelegramStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub mode: Option<String>,
    pub bot_username: Option<String>,
    pub last_probe_at: Option<i64>,
    pub last_error: Option<String>,
    pub probe: Option<TelegramProbeResult>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TelegramProbeResult {
    pub ok: Option<bool>,
    pub status: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct WhatsAppStatus {
    pub configured: Option<bool>,
    pub linked: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub auth_age_ms: Option<i64>,
    pub last_connected_at: Option<i64>,
    pub last_message_at: Option<i64>,
    pub last_error: Option<String>,
    pub qr_data_url: Option<String>,
    pub self_user: Option<WhatsAppSelfUser>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct WhatsAppSelfUser {
    pub push_name: Option<String>,
    pub id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SlackStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub workspace_name: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SignalStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub phone_number: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct IMessageStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NostrStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub public_key: Option<String>,
    pub relay_count: Option<u32>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct GoogleChatStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct MatrixStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub mode: Option<String>,
    pub homeserver: Option<String>,
    pub user_id: Option<String>,
    pub room_count: Option<u32>,
    pub pending_invites: Option<u32>,
    pub auto_join: Option<String>,
    pub appservice_url: Option<String>,
    pub sender_localpart: Option<String>,
    pub user_prefix: Option<String>,
    pub server_name: Option<String>,
    pub config_id: Option<String>,
    pub registration: Option<serde_json::Value>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ArkretStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub mode: Option<String>,
    pub base_url: Option<String>,
    pub service_id: Option<String>,
    pub account_id: Option<String>,
    pub principal_id: Option<String>,
    pub applet_id: Option<String>,
    pub bot_actor_id: Option<String>,
    pub protocol_count: Option<u32>,
    pub namespace_count: Option<u32>,
    pub instance_count: Option<u32>,
    pub ready_count: Option<u32>,
    pub retrying_count: Option<u32>,
    pub migration_required_count: Option<u32>,
    pub failed_count: Option<u32>,
    pub health_state: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct MattermostStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub server_url: Option<String>,
    pub team_name: Option<String>,
    pub bot_username: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct LineStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub bot_name: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct FeishuStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub app_id: Option<String>,
    pub bot_name: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct IrcStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub server: Option<String>,
    pub nickname: Option<String>,
    pub channel_count: Option<u32>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct MsTeamsStatus {
    pub configured: Option<bool>,
    pub running: Option<bool>,
    pub connected: Option<bool>,
    pub tenant_id: Option<String>,
    pub bot_name: Option<String>,
    pub last_activity: Option<i64>,
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ChannelsStatusSnapshot;

    #[test]
    fn aggregate_channel_status_deserializes_common_fields() {
        let snapshot: ChannelsStatusSnapshot = serde_json::from_value(json!({
            "channels": {
                "discord": {
                    "configured": true,
                    "running": true,
                    "connected": false,
                    "id": "discord-main",
                    "channel_name": "Main Discord",
                    "last_error": "connection lost",
                    "guild_count": 3
                },
                "legacy": {
                    "configured": true,
                    "lastError": "legacy error"
                }
            },
            "instances": {
                "discord-main": {
                    "platform": "discord",
                    "configured": true,
                    "running": true,
                    "connected": false,
                    "runtime_capability": "persistent",
                    "recovery_phase": "ready",
                    "health_state": "listening",
                    "startup_attempts": 1
                }
            }
        }))
        .expect("channel status should deserialize");

        let channels = snapshot
            .channels
            .as_ref()
            .expect("channels should be present");
        let discord = channels.get("discord").expect("discord should be present");
        assert_eq!(discord.connected, Some(false));
        assert_eq!(discord.id.as_deref(), Some("discord-main"));
        assert_eq!(discord.last_error.as_deref(), Some("connection lost"));
        assert_eq!(
            channels
                .get("legacy")
                .and_then(|status| status.last_error.as_deref()),
            Some("legacy error")
        );
        let instance = snapshot
            .instances
            .as_ref()
            .and_then(|instances| instances.get("discord-main"))
            .expect("instance status should be present");
        assert_eq!(instance.recovery_phase.as_deref(), Some("ready"));
        assert_eq!(instance.health_state.as_deref(), Some("listening"));
    }
}
