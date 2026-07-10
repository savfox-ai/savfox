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
    pub channels: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChannelAccountSnapshot {
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
    pub service_did: Option<String>,
    pub account_id: Option<String>,
    pub principal_id: Option<String>,
    pub applet_id: Option<String>,
    pub bot_actor_id: Option<String>,
    pub protocol_count: Option<u32>,
    pub namespace_count: Option<u32>,
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
