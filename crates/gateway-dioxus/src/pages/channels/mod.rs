#![allow(clippy::items_after_test_module)]

pub mod arkret;
pub mod common;
pub mod dingtalk;
pub mod discord;
pub mod feishu;
pub mod generic;
pub mod google_chat;
pub mod imessage;
pub mod irc;
pub mod line;
pub mod matrix;
pub mod mattermost;
pub mod msteams;
pub mod nextcloud;
pub mod nostr;
pub mod signal;
pub mod slack;
pub mod telegram;
pub mod tlon;
pub mod twitch;
pub mod webhook;
pub mod whatsapp;
pub mod zalo;

use arkret_models_collaboration::agent_operations::AgentPairingBootstrap;
use dioxus::prelude::*;
use lucide_dioxus::{
    Activity, Braces, CircleCheck, Copy, Link2, LoaderCircle, Power, Settings, SlidersHorizontal,
    Trash2, TriangleAlert, Unplug,
};
use savfox_utils::string::normalize_slug;
use serde_json::{Value, json};

use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};
use crate::components::skeleton::*;
use crate::components::tooltip::HelpTip;
use crate::utils::deep_link::replace_url;

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ChannelTypeInfo {
    id: String,
    name: String,
    icon: String,
    description: String,
    config_fields: Vec<ConfigField>,
}

#[derive(Clone, Debug, PartialEq)]
struct ConfigField {
    key: String,
    label: String,
    field_type: FieldType,
    placeholder: String,
    secret: bool,
    required: bool,
    help: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SavedChannelSummary {
    kind: String,
    id: String,
    name: String,
    enabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum FieldType {
    Text,
    Password,
    Number,
    Textarea,
    Toggle,
    Select(Vec<String>),
}

fn saved_channel_summaries(configs: Option<&Value>) -> Vec<SavedChannelSummary> {
    configs
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|config| {
            let kind = config
                .get("kind")
                .and_then(Value::as_str)
                .or_else(|| {
                    config
                        .get("id")
                        .and_then(Value::as_str)
                        .and_then(|raw| raw.split('-').next())
                })?
                .trim()
                .to_ascii_lowercase();
            let id = config
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_owned();
            let name = config
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(&id)
                .to_owned();
            let enabled = config
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            Some(SavedChannelSummary {
                kind,
                id,
                name,
                enabled,
            })
        })
        .collect()
}

fn next_available_channel_name(
    kind: &str,
    base_name: &str,
    configs: &[SavedChannelSummary],
) -> String {
    let id_is_available = |name: &str| {
        let candidate = compute_channel_id(name, kind);
        configs
            .iter()
            .all(|config| !config.id.eq_ignore_ascii_case(&candidate))
    };
    if id_is_available(base_name) {
        return base_name.to_owned();
    }
    for suffix in 2.. {
        let candidate = format!("{base_name} {suffix}");
        if id_is_available(&candidate) {
            return candidate;
        }
    }
    unreachable!("an unused channel name suffix must exist")
}

fn new_channel_form_values(
    channel_id: &str,
    fields: &[ConfigField],
    channel_name: String,
) -> std::collections::HashMap<String, String> {
    let mut values = default_channel_values(channel_id, fields);
    values.insert(channel_name_key(channel_id), channel_name);
    values
}

/// Cached channel metadata. The full set of channel descriptors is fully
/// `'static` (only `String` / `&'static str` fields), so we build it once via
/// `LazyLock` instead of rebuilding+cloning it on every render.
static CHANNEL_TYPES: std::sync::LazyLock<Vec<ChannelTypeInfo>> =
    std::sync::LazyLock::new(build_channel_types);

fn get_channel_types() -> &'static [ChannelTypeInfo] {
    &CHANNEL_TYPES
}

fn build_channel_types() -> Vec<ChannelTypeInfo> {
    vec![
        ChannelTypeInfo {
            id: "discord".into(),
            name: "Discord".into(),
            icon: "D".into(),
            description: "Connect to Discord servers via bot token".into(),
            config_fields: vec![
                ConfigField {
                    key: "bot_token".into(),
                    label: "Bot Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "Enter Discord bot token".into(),
                    secret: true,
                    required: true,
                    help: "Your bot's authentication token from the Discord Developer Portal",
                },
                ConfigField {
                    key: "application_id".into(),
                    label: "Application ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "Discord application ID".into(),
                    secret: false,
                    required: false,
                    help: "The application ID from your Discord Developer Portal app settings",
                },
                ConfigField {
                    key: "application_public_key".into(),
                    label: "Application Public Key".into(),
                    field_type: FieldType::Text,
                    placeholder: "Discord application public key".into(),
                    secret: false,
                    required: false,
                    help: "Used to verify incoming interaction webhooks from Discord",
                },
                ConfigField {
                    key: "guild_id".into(),
                    label: "Guild ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "Discord guild (server) ID".into(),
                    secret: false,
                    required: false,
                    help: "The server (guild) ID to restrict the bot to a single server",
                },
                ConfigField {
                    key: "mode".into(),
                    label: "Inbound Mode".into(),
                    field_type: FieldType::Select(vec!["stream".into(), "webhook".into()]),
                    placeholder: "stream".into(),
                    secret: false,
                    required: false,
                    help: "Stream opens a Discord Gateway connection for DMs and plain messages; webhook keeps slash-command interactions only",
                },
            ],
        },
        ChannelTypeInfo {
            id: "telegram".into(),
            name: "Telegram".into(),
            icon: "T".into(),
            description: "Connect via Telegram bot".into(),
            config_fields: vec![
                ConfigField {
                    key: "bot_token".into(),
                    label: "Bot Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "Enter Telegram bot token from @BotFather".into(),
                    secret: true,
                    required: true,
                    help: "Your bot's authentication token from BotFather",
                },
                ConfigField {
                    key: "webhook_url".into(),
                    label: "Webhook URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://your-domain.com/webhooks/telegram".into(),
                    secret: false,
                    required: false,
                    help: "The URL where Telegram will send updates; leave empty to use polling",
                },
                ConfigField {
                    key: "polling".into(),
                    label: "Use Polling".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Use long-polling instead of webhooks to receive updates",
                },
            ],
        },
        ChannelTypeInfo {
            id: "slack".into(),
            name: "Slack".into(),
            icon: "S".into(),
            description: "Connect to Slack workspaces via Socket Mode".into(),
            config_fields: vec![
                ConfigField {
                    key: "bot_token".into(),
                    label: "Bot Token (xoxb-)".into(),
                    field_type: FieldType::Password,
                    placeholder: "xoxb-...".into(),
                    secret: true,
                    required: true,
                    help: "Bot user OAuth token starting with xoxb- from your Slack app",
                },
                ConfigField {
                    key: "signing_secret".into(),
                    label: "Signing Secret".into(),
                    field_type: FieldType::Password,
                    placeholder: "Slack signing secret".into(),
                    secret: true,
                    required: true,
                    help: "Used to verify that requests come from Slack",
                },
                ConfigField {
                    key: "app_id".into(),
                    label: "App ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "Slack app ID".into(),
                    secret: false,
                    required: false,
                    help: "Your Slack application ID from the app settings page",
                },
            ],
        },
        ChannelTypeInfo {
            id: "whatsapp".into(),
            name: "WhatsApp".into(),
            icon: "W".into(),
            description: "Connect via WhatsApp Cloud API".into(),
            config_fields: vec![
                ConfigField {
                    key: "phone_number_id".into(),
                    label: "Phone Number ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "WhatsApp phone number ID".into(),
                    secret: false,
                    required: true,
                    help: "The phone number ID from Meta Business Suite for your WhatsApp number",
                },
                ConfigField {
                    key: "access_token".into(),
                    label: "Access Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "WhatsApp access token".into(),
                    secret: true,
                    required: true,
                    help: "Permanent access token from Meta for Developers dashboard",
                },
                ConfigField {
                    key: "verify_token".into(),
                    label: "Verify Token".into(),
                    field_type: FieldType::Text,
                    placeholder: "Webhook verification token".into(),
                    secret: false,
                    required: false,
                    help: "Custom string used to verify webhook setup with Meta",
                },
            ],
        },
        ChannelTypeInfo {
            id: "nostr".into(),
            name: "Nostr".into(),
            icon: "N".into(),
            description: "Decentralized relay-based messaging".into(),
            config_fields: vec![
                ConfigField {
                    key: "public_key".into(),
                    label: "Public Key".into(),
                    field_type: FieldType::Text,
                    placeholder: "npub... or hex".into(),
                    secret: false,
                    required: false,
                    help: "Your Nostr public key in npub or hex format",
                },
                ConfigField {
                    key: "private_key".into(),
                    label: "Private Key".into(),
                    field_type: FieldType::Password,
                    placeholder: "nsec... or hex".into(),
                    secret: true,
                    required: true,
                    help: "Your Nostr private key in nsec or hex format for signing events",
                },
                ConfigField {
                    key: "relay_urls".into(),
                    label: "Relay URLs".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "wss://relay.damus.io\nwss://nos.lol".into(),
                    secret: false,
                    required: true,
                    help: "WebSocket relay URLs to connect to, one per line",
                },
            ],
        },
        ChannelTypeInfo {
            id: "signal".into(),
            name: "Signal".into(),
            icon: "Si".into(),
            description: "Connect via Signal CLI".into(),
            config_fields: vec![
                ConfigField {
                    key: "phone_number".into(),
                    label: "Phone Number".into(),
                    field_type: FieldType::Text,
                    placeholder: "+1234567890".into(),
                    secret: false,
                    required: true,
                    help: "The phone number registered with Signal in E.164 format",
                },
                ConfigField {
                    key: "device_name".into(),
                    label: "Device Name".into(),
                    field_type: FieldType::Text,
                    placeholder: "savfox-signal".into(),
                    secret: false,
                    required: false,
                    help: "Friendly name for this linked device shown in Signal settings",
                },
            ],
        },
        ChannelTypeInfo {
            id: "imessage".into(),
            name: "iMessage".into(),
            icon: "iM".into(),
            description: "Connect via BlueBubbles server".into(),
            config_fields: vec![
                ConfigField {
                    key: "server_url".into(),
                    label: "BlueBubbles Server URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "http://localhost:1234".into(),
                    secret: false,
                    required: true,
                    help: "The URL of your running BlueBubbles server instance",
                },
                ConfigField {
                    key: "password".into(),
                    label: "Password".into(),
                    field_type: FieldType::Password,
                    placeholder: "BlueBubbles server password".into(),
                    secret: true,
                    required: true,
                    help: "BlueBubbles server password for API authentication",
                },
            ],
        },
        ChannelTypeInfo {
            id: "matrix".into(),
            name: "Matrix".into(),
            icon: "M".into(),
            description: "Connect a Matrix user or appservice bridge".into(),
            config_fields: vec![
                ConfigField {
                    key: "mode".into(),
                    label: "Mode".into(),
                    field_type: FieldType::Select(vec!["user".into(), "appservice".into()]),
                    placeholder: "user".into(),
                    secret: false,
                    required: true,
                    help: "User mode logs in as a bot account; appservice mode registers a bridge",
                },
                ConfigField {
                    key: "homeserver".into(),
                    label: "Homeserver URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://matrix.org".into(),
                    secret: false,
                    required: true,
                    help: "The base URL of your Matrix homeserver",
                },
                ConfigField {
                    key: "userId".into(),
                    label: "User ID (optional with token)".into(),
                    field_type: FieldType::Text,
                    placeholder: "@bot:matrix.org".into(),
                    secret: false,
                    required: false,
                    help: "Matrix user ID; auto-detected when using an access token",
                },
                ConfigField {
                    key: "accessToken".into(),
                    label: "Access Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "syt_... (user ID fetched automatically)".into(),
                    secret: true,
                    required: false,
                    help: "Matrix access token for authentication (alternative to password)",
                },
                ConfigField {
                    key: "password".into(),
                    label: "Password (alternative to token)".into(),
                    field_type: FieldType::Password,
                    placeholder: "Bot account password".into(),
                    secret: true,
                    required: false,
                    help: "Bot account password used when no access token is provided",
                },
                ConfigField {
                    key: "deviceName".into(),
                    label: "Device Name".into(),
                    field_type: FieldType::Text,
                    placeholder: "Savfox Gateway".into(),
                    secret: false,
                    required: false,
                    help: "Friendly device name shown in Matrix session list",
                },
                ConfigField {
                    key: "encryption".into(),
                    label: "Enable E2EE".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Enable end-to-end encryption for messages",
                },
                ConfigField {
                    key: "dmPolicy".into(),
                    label: "DM Policy".into(),
                    field_type: FieldType::Select(vec![
                        "pairing".into(),
                        "allowlist".into(),
                        "open".into(),
                        "disabled".into(),
                    ]),
                    placeholder: "pairing".into(),
                    secret: false,
                    required: false,
                    help: "Controls who can initiate direct messages with the bot",
                },
                ConfigField {
                    key: "dmAllowFrom".into(),
                    label: "DM Allow From (comma-separated)".into(),
                    field_type: FieldType::Text,
                    placeholder: "@user:server.org, @admin:example.com".into(),
                    secret: false,
                    required: false,
                    help: "Matrix user IDs allowed to DM the bot when using allowlist policy",
                },
                ConfigField {
                    key: "groupPolicy".into(),
                    label: "Group Policy".into(),
                    field_type: FieldType::Select(vec![
                        "allowlist".into(),
                        "open".into(),
                        "disabled".into(),
                    ]),
                    placeholder: "allowlist".into(),
                    secret: false,
                    required: false,
                    help: "Controls which rooms the bot responds in",
                },
                ConfigField {
                    key: "groups".into(),
                    label: "Allowed Rooms (comma-separated)".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "!roomId:server.org\n#alias:server.org".into(),
                    secret: false,
                    required: false,
                    help: "Room IDs or aliases the bot is allowed to join and respond in",
                },
                ConfigField {
                    key: "autoJoin".into(),
                    label: "Invite Join Strategy".into(),
                    field_type: FieldType::Select(vec![
                        "off".into(),
                        "allowlist".into(),
                        "always".into(),
                    ]),
                    placeholder: "off".into(),
                    secret: false,
                    required: false,
                    help: "Controls whether Matrix room invites are queued for review, joined by allowlist, or always joined",
                },
                ConfigField {
                    key: "autoJoinAllowlist".into(),
                    label: "Invite Allowlist".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "!roomId:server.org\n#alias:server.org\n*".into(),
                    secret: false,
                    required: false,
                    help: "Room IDs, aliases, or * allowed for automatic invite joining when strategy is allowlist",
                },
                ConfigField {
                    key: "allowedSenders".into(),
                    label: "Allowed Senders".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "@user:server.org\n@admin:example.com".into(),
                    secret: false,
                    required: false,
                    help: "Optional Matrix user IDs allowed to trigger the bot; leave empty to allow all senders",
                },
                ConfigField {
                    key: "serverName".into(),
                    label: "Server Name".into(),
                    field_type: FieldType::Text,
                    placeholder: "matrix.example.com".into(),
                    secret: false,
                    required: false,
                    help: "Server name used in appservice registration for namespace regex",
                },
                ConfigField {
                    key: "publicUrl".into(),
                    label: "Public Base URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://gateway.example.com".into(),
                    secret: false,
                    required: false,
                    help: "Public URL the homeserver uses to reach this appservice",
                },
                ConfigField {
                    key: "appserviceId".into(),
                    label: "Appservice ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "savfox-matrix".into(),
                    secret: false,
                    required: false,
                    help: "Unique identifier for this appservice in the homeserver config",
                },
                ConfigField {
                    key: "appserviceToken".into(),
                    label: "Appservice Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "Matrix appservice AS token".into(),
                    secret: true,
                    required: false,
                    help: "Token the appservice uses to authenticate to the homeserver",
                },
                ConfigField {
                    key: "homeserverToken".into(),
                    label: "Homeserver Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "Matrix appservice HS token".into(),
                    secret: true,
                    required: false,
                    help: "Token the homeserver uses to authenticate to the appservice",
                },
                ConfigField {
                    key: "senderLocalpart".into(),
                    label: "Sender Localpart".into(),
                    field_type: FieldType::Text,
                    placeholder: "savfox".into(),
                    secret: false,
                    required: false,
                    help: "Local part of the appservice bot user (e.g. savfox in @savfox:server)",
                },
                ConfigField {
                    key: "userPrefix".into(),
                    label: "Virtual User Prefix".into(),
                    field_type: FieldType::Text,
                    placeholder: "_savfox_".into(),
                    secret: false,
                    required: false,
                    help: "Prefix for virtual user IDs managed by the appservice",
                },
                ConfigField {
                    key: "aliasPrefix".into(),
                    label: "Room Alias Prefix".into(),
                    field_type: FieldType::Text,
                    placeholder: "_savfox_".into(),
                    secret: false,
                    required: false,
                    help: "Prefix for room aliases managed by the appservice",
                },
            ],
        },
        ChannelTypeInfo {
            id: "arkret".into(),
            name: "Arkret".into(),
            icon: "Ak".into(),
            description: "Connect an Arkret AI agent or registered applet".into(),
            config_fields: vec![
                ConfigField {
                    key: "mode".into(),
                    label: "Connection Type".into(),
                    field_type: FieldType::Select(vec!["agent".into(), "applet".into()]),
                    placeholder: "agent".into(),
                    secret: false,
                    required: true,
                    help: "Agent consumes a Inkson bootstrap and uses a local runtime key. Applet exposes Savfox as a registered Arkret Applet endpoint.",
                },
                ConfigField {
                    key: "inksonBootstrap".into(),
                    label: "Inkson pairing link".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://arkret.example.org/_arkret/open/agent-pairing/resolve#token=...".into(),
                    secret: false,
                    required: false,
                    help: "Paste the short pairing link from Inkson. Savfox resolves and stores the protocol bootstrap without showing its internal JSON.",
                },
                ConfigField {
                    key: "baseUrl".into(),
                    label: "Arkret Base URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://arkret.example.org".into(),
                    secret: false,
                    required: false,
                    help: "Arkret server URL. Agent mode normally derives this from the Inkson bootstrap.",
                },
                ConfigField {
                    key: "serviceId".into(),
                    label: "Arkret Service DID".into(),
                    field_type: FieldType::Text,
                    placeholder: "did:webvh:arkret.example.org".into(),
                    secret: false,
                    required: false,
                    help: "Arkret service DID used as the agent_key_proof audience and DPoP-bound self endpoint service identity.",
                },
                ConfigField {
                    key: "accessToken".into(),
                    label: "Applet Bearer Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "applet bearer token".into(),
                    secret: true,
                    required: false,
                    help: "Inbound applet bearer token configured in Arkret. Agent mode does not store a static session grant here.",
                },
                ConfigField {
                    key: "deviceId".into(),
                    label: "Device ID (internal)".into(),
                    field_type: FieldType::Text,
                    placeholder: "ak:device:...".into(),
                    secret: false,
                    required: false,
                    help: "Internal Arkret runtime device id. Savfox derives this automatically.",
                },
                ConfigField {
                    key: "advanced".into(),
                    label: "Advanced settings".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Show low-level Arkret scope, signing, and applet runtime fields.",
                },
                ConfigField {
                    key: "appletId".into(),
                    label: "Applet ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "ak:applet:...".into(),
                    secret: false,
                    required: true,
                    help: "Registered Arkret applet identifier.",
                },
                ConfigField {
                    key: "controllerId".into(),
                    label: "Controller DID".into(),
                    field_type: FieldType::Text,
                    placeholder: "did:webvh:arkret.example.org".into(),
                    secret: false,
                    required: true,
                    help: "Controller DID that owns or signs the applet registration.",
                },
                ConfigField {
                    key: "botActorId".into(),
                    label: "Bot Actor DID".into(),
                    field_type: FieldType::Text,
                    placeholder: "did:web:savfox.example:bot".into(),
                    secret: false,
                    required: false,
                    help: "Visible applet bot actor DID. Defaults to serviceId:bot.",
                },
                ConfigField {
                    key: "arkretServerUrl".into(),
                    label: "Arkret Server URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://arkret.example.org".into(),
                    secret: false,
                    required: false,
                    help: "Arkret server used by the applet for outbound event submission.",
                },
                ConfigField {
                    key: "arkretServerDid".into(),
                    label: "Arkret Server DID".into(),
                    field_type: FieldType::Text,
                    placeholder: "did:webvh:arkret.example.org".into(),
                    secret: false,
                    required: false,
                    help: "Trusted Arkret server DID for applet outbound authentication and HTTP Message Signature verification.",
                },
                ConfigField {
                    key: "protocols".into(),
                    label: "Applet Protocols".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "slack\ndiscord".into(),
                    secret: false,
                    required: false,
                    help: "External protocols bridged by this applet, one per line or comma-separated.",
                },
                ConfigField {
                    key: "namespaceActors".into(),
                    label: "Actor Namespaces".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "did:web:savfox.example:ghost:*".into(),
                    secret: false,
                    required: false,
                    help: "Applet actor namespace patterns. Saved as Arkret namespaces.actors[].",
                },
                ConfigField {
                    key: "namespaceRealms".into(),
                    label: "Realm Namespaces".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "slack:team:*:channel:*".into(),
                    secret: false,
                    required: false,
                    help: "Applet realm namespace patterns. Saved as Arkret namespaces.realms[].",
                },
                ConfigField {
                    key: "namespaceHandles".into(),
                    label: "Handle Namespaces".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "slack.example/*".into(),
                    secret: false,
                    required: false,
                    help: "Applet third-party handle namespace patterns. Saved as Arkret namespaces.handles[].",
                },
                ConfigField {
                    key: "ghostDidPrefix".into(),
                    label: "Ghost DID Prefix".into(),
                    field_type: FieldType::Text,
                    placeholder: "ghost:".into(),
                    secret: false,
                    required: false,
                    help: "Prefix used when minting ghost actor DIDs for external users",
                },
                ConfigField {
                    key: "requestedScopes".into(),
                    label: "Requested Scopes".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "ak.message.create\nck.flow.create".into(),
                    secret: false,
                    required: false,
                    help: "Requested applet scopes, one per line or comma-separated",
                },
                ConfigField {
                    key: "receiveEvents".into(),
                    label: "Receive Events".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Allow Arkret to push event transactions to this applet",
                },
                ConfigField {
                    key: "receiveEphemeral".into(),
                    label: "Receive Ephemeral".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Allow ephemeral Arkret events such as typing/presence",
                },
                ConfigField {
                    key: "rateLimited".into(),
                    label: "Allow Rate Limiting".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Permit the Arkret server to rate-limit applet transaction pushes",
                },
                ConfigField {
                    key: "authorizationGrantId".into(),
                    label: "Authorization Grant ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "ak:event:...".into(),
                    secret: false,
                    required: false,
                    help: "Optional capability grant event id attached to outbound applet events",
                },
                ConfigField {
                    key: "registrationEpoch".into(),
                    label: "Registration Epoch".into(),
                    field_type: FieldType::Text,
                    placeholder: "sha256:<hex>".into(),
                    secret: false,
                    required: false,
                    help: "Operator supplied registration evidence hash",
                },
                ConfigField {
                    key: "trustedVerificationMethods".into(),
                    label: "Trusted Verification Methods JSON".into(),
                    field_type: FieldType::Textarea,
                    placeholder: r#"[{"verificationMethod":"did:webvh:arkret.example.org#key-1","publicKeyMultibase":"z..."}]"#.into(),
                    secret: false,
                    required: false,
                    help: "Optional array of Arkret server HTTP Message Signature public keys",
                },
                ConfigField {
                    key: "loginChallenge".into(),
                    label: "Applet DID-proof Challenge".into(),
                    field_type: FieldType::Text,
                    placeholder: "challenge-from-arkret".into(),
                    secret: false,
                    required: false,
                    help: "Applet outbound DID-proof challenge. Personal agent runtime uses agent_key_proof instead.",
                },
                ConfigField {
                    key: "verificationMethod".into(),
                    label: "Runtime key DID URL (internal)".into(),
                    field_type: FieldType::Text,
                    placeholder: "did:web:agent.example#key-1".into(),
                    secret: false,
                    required: false,
                    help: "Internal DID URL for the local runtime key. Savfox derives this from the Inkson bootstrap.",
                },
                ConfigField {
                    key: "authorizedEventRef".into(),
                    label: "Authorization event (internal)".into(),
                    field_type: FieldType::Text,
                    placeholder: "ak:event:...".into(),
                    secret: false,
                    required: false,
                    help: "ak.agent.key.authorize event reference produced after controller approval.",
                },
                ConfigField {
                    key: "runtimeKeyRequest".into(),
                    label: "Request approval".into(),
                    field_type: FieldType::Textarea,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Savfox generates the local runtime key when needed and builds the internal approval payload for Inkson.",
                },
                ConfigField {
                    key: "authorizationResult".into(),
                    label: "Approval result (internal)".into(),
                    field_type: FieldType::Textarea,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Internal compatibility slot for an authorization event returned by Inkson after approval.",
                },
                ConfigField {
                    key: "unbind".into(),
                    label: "Bound agent".into(),
                    field_type: FieldType::Textarea,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "A runtime binds exactly one Agent. Unbind revokes the current Agent's KeyPackage pool, purges its local runtime state, and clears the binding so a different Agent can be paired.",
                },
                ConfigField {
                    key: "grantEventPath".into(),
                    label: "Grant Event Path".into(),
                    field_type: FieldType::Text,
                    placeholder: "C:\\secrets\\arkret-grant.json".into(),
                    secret: false,
                    required: false,
                    help: "Path to a pre-signed ak.capability.grant Event JSON",
                },
                ConfigField {
                    key: "keyRef".into(),
                    label: "Local Runtime Key".into(),
                    field_type: FieldType::Textarea,
                    placeholder: r#"{"kind":"env","var":"SAVFOX_ARKRET_BOT_KEY"}"#.into(),
                    secret: true,
                    required: false,
                    help: "Advanced local runtime key reference. By default Savfox generates a local file key automatically.",
                },
            ],
        },
        ChannelTypeInfo {
            id: "mattermost".into(),
            name: "Mattermost".into(),
            icon: "MM".into(),
            description: "Connect to Mattermost servers".into(),
            config_fields: vec![
                ConfigField {
                    key: "server_url".into(),
                    label: "Server URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://mattermost.example.com".into(),
                    secret: false,
                    required: true,
                    help: "The base URL of your Mattermost server",
                },
                ConfigField {
                    key: "bot_token".into(),
                    label: "Bot Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "Mattermost bot token".into(),
                    secret: true,
                    required: true,
                    help: "Bot account token from Mattermost integrations settings",
                },
                ConfigField {
                    key: "team_name".into(),
                    label: "Team Name".into(),
                    field_type: FieldType::Text,
                    placeholder: "my-team".into(),
                    secret: false,
                    required: false,
                    help: "The team slug to restrict the bot to a specific team",
                },
            ],
        },
        ChannelTypeInfo {
            id: "googlechat".into(),
            name: "Google Chat".into(),
            icon: "G".into(),
            description: "Connect to Google Chat via incoming webhook".into(),
            config_fields: vec![
                ConfigField {
                    key: "webhook_url".into(),
                    label: "Webhook URL".into(),
                    field_type: FieldType::Password,
                    placeholder: "https://chat.googleapis.com/v1/spaces/...".into(),
                    secret: true,
                    required: true,
                    help: "Incoming webhook URL used to post Savfox replies back to Google Chat",
                },
                ConfigField {
                    key: "space_id".into(),
                    label: "Space ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "spaces/AAAA...".into(),
                    secret: false,
                    required: false,
                    help: "Optional inbound space filter; if set, only this space is accepted",
                },
                ConfigField {
                    key: "verification_token".into(),
                    label: "Verification Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "Optional shared token".into(),
                    secret: true,
                    required: false,
                    help: "Optional token checked against the webhook request for extra protection",
                },
            ],
        },
        ChannelTypeInfo {
            id: "irc".into(),
            name: "IRC".into(),
            icon: "IR".into(),
            description: "Connect to IRC networks".into(),
            config_fields: vec![
                ConfigField {
                    key: "server".into(),
                    label: "Server".into(),
                    field_type: FieldType::Text,
                    placeholder: "irc.libera.chat".into(),
                    secret: false,
                    required: true,
                    help: "IRC server hostname to connect to",
                },
                ConfigField {
                    key: "port".into(),
                    label: "Port".into(),
                    field_type: FieldType::Number,
                    placeholder: "6667".into(),
                    secret: false,
                    required: false,
                    help: "Server port (6667 for plain, 6697 for TLS)",
                },
                ConfigField {
                    key: "nick".into(),
                    label: "Nickname".into(),
                    field_type: FieldType::Text,
                    placeholder: "MyBot".into(),
                    secret: false,
                    required: true,
                    help: "The IRC nickname the bot will use",
                },
                ConfigField {
                    key: "channel".into(),
                    label: "Channel".into(),
                    field_type: FieldType::Text,
                    placeholder: "#channel".into(),
                    secret: false,
                    required: true,
                    help: "The IRC channel to join, including the # prefix",
                },
                ConfigField {
                    key: "use_tls".into(),
                    label: "Use TLS".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Enable TLS/SSL encryption for the IRC connection",
                },
            ],
        },
        ChannelTypeInfo {
            id: "line".into(),
            name: "LINE".into(),
            icon: "Li".into(),
            description: "Connect to LINE messaging".into(),
            config_fields: vec![
                ConfigField {
                    key: "channel_access_token".into(),
                    label: "Channel Access Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "LINE channel access token".into(),
                    secret: true,
                    required: true,
                    help: "Long-lived channel access token from the LINE Developers console",
                },
                ConfigField {
                    key: "channel_secret".into(),
                    label: "Channel Secret".into(),
                    field_type: FieldType::Password,
                    placeholder: "LINE channel secret".into(),
                    secret: true,
                    required: true,
                    help: "Channel secret used to verify webhook signatures",
                },
            ],
        },
        ChannelTypeInfo {
            id: "qq".into(),
            name: "QQ".into(),
            icon: "Q".into(),
            description: "Connect QQ through a webhook bridge".into(),
            config_fields: vec![
                ConfigField {
                    key: "webhook_url".into(),
                    label: "Bridge Webhook URL".into(),
                    field_type: FieldType::Password,
                    placeholder: "https://bridge.example.com/qq/send".into(),
                    secret: true,
                    required: true,
                    help: "Bridge endpoint Savfox uses to send QQ replies",
                },
                ConfigField {
                    key: "verify_token".into(),
                    label: "Verify Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "Optional shared token".into(),
                    secret: true,
                    required: false,
                    help: "Optional token required on inbound webhook requests from your QQ bridge",
                },
            ],
        },
        ChannelTypeInfo {
            id: "wechat".into(),
            name: "WeChat".into(),
            icon: "WX".into(),
            description: "Connect WeChat through a webhook bridge".into(),
            config_fields: vec![
                ConfigField {
                    key: "webhook_url".into(),
                    label: "Bridge Webhook URL".into(),
                    field_type: FieldType::Password,
                    placeholder: "https://bridge.example.com/wechat/send".into(),
                    secret: true,
                    required: true,
                    help: "Bridge endpoint Savfox uses to send WeChat replies",
                },
                ConfigField {
                    key: "verify_token".into(),
                    label: "Verify Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "Optional shared token".into(),
                    secret: true,
                    required: false,
                    help: "Optional token required on inbound webhook requests from your WeChat bridge",
                },
            ],
        },
        ChannelTypeInfo {
            id: "webhook".into(),
            name: "Generic Webhook".into(),
            icon: "WH".into(),
            description: "Send/receive messages via HTTP webhook".into(),
            config_fields: vec![
                ConfigField {
                    key: "url".into(),
                    label: "URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://example.com/webhook".into(),
                    secret: false,
                    required: true,
                    help: "The URL where the platform will send events",
                },
                ConfigField {
                    key: "secret".into(),
                    label: "Secret".into(),
                    field_type: FieldType::Password,
                    placeholder: "Optional secret for verification".into(),
                    secret: true,
                    required: false,
                    help: "Shared secret used to sign and verify webhook payloads",
                },
                ConfigField {
                    key: "method".into(),
                    label: "Method".into(),
                    field_type: FieldType::Select(vec!["POST".into(), "GET".into()]),
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "HTTP method used when sending outbound webhook requests",
                },
            ],
        },
        ChannelTypeInfo {
            id: "feishu".into(),
            name: "Feishu/Lark".into(),
            icon: "F".into(),
            description: "Connect to Feishu or Lark".into(),
            config_fields: vec![
                ConfigField {
                    key: "app_id".into(),
                    label: "App ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "Feishu app ID".into(),
                    secret: false,
                    required: true,
                    help: "Application ID from the Feishu/Lark developer console",
                },
                ConfigField {
                    key: "app_secret".into(),
                    label: "App Secret".into(),
                    field_type: FieldType::Password,
                    placeholder: "Feishu app secret".into(),
                    secret: true,
                    required: true,
                    help: "Application secret for authenticating API requests",
                },
            ],
        },
        ChannelTypeInfo {
            id: "zalo".into(),
            name: "Zalo".into(),
            icon: "Z".into(),
            description: "Connect to Zalo OA via webhook and customer service API".into(),
            config_fields: vec![
                ConfigField {
                    key: "app_id".into(),
                    label: "App ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "Zalo OA app ID".into(),
                    secret: false,
                    required: true,
                    help: "Application ID from your Zalo OA developer account",
                },
                ConfigField {
                    key: "app_secret".into(),
                    label: "App Secret".into(),
                    field_type: FieldType::Password,
                    placeholder: "Zalo OA app secret".into(),
                    secret: true,
                    required: true,
                    help: "Application secret for authenticating Zalo API requests",
                },
                ConfigField {
                    key: "access_token".into(),
                    label: "Access Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "Zalo OA access token".into(),
                    secret: true,
                    required: true,
                    help: "OA access token for sending messages via the Zalo API",
                },
                ConfigField {
                    key: "webhook_verify_token".into(),
                    label: "Verify Token".into(),
                    field_type: FieldType::Text,
                    placeholder: "Optional webhook verify token".into(),
                    secret: false,
                    required: false,
                    help: "Custom token used to verify incoming webhook requests from Zalo",
                },
            ],
        },
        ChannelTypeInfo {
            id: "dingtalk".into(),
            name: "DingTalk".into(),
            icon: "DT".into(),
            description: "Connect to DingTalk custom robot/webhook".into(),
            config_fields: vec![
                ConfigField {
                    key: "webhook_url".into(),
                    label: "Webhook URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://oapi.dingtalk.com/robot/send?access_token=...".into(),
                    secret: false,
                    required: true,
                    help: "The DingTalk custom robot webhook URL including access token",
                },
                ConfigField {
                    key: "access_token".into(),
                    label: "Access Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "DingTalk robot access token".into(),
                    secret: true,
                    required: true,
                    help: "Robot access token from the DingTalk custom robot settings",
                },
                ConfigField {
                    key: "secret".into(),
                    label: "Sign Secret".into(),
                    field_type: FieldType::Password,
                    placeholder: "Optional DingTalk webhook sign secret".into(),
                    secret: true,
                    required: false,
                    help: "Signing secret for additional webhook security verification",
                },
            ],
        },
        ChannelTypeInfo {
            id: "nextcloud".into(),
            name: "NextCloud Talk".into(),
            icon: "NC".into(),
            description: "Connect to NextCloud Talk rooms via the OCS API".into(),
            config_fields: vec![
                ConfigField {
                    key: "server_url".into(),
                    label: "Server URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://cloud.example.com".into(),
                    secret: false,
                    required: true,
                    help: "The base URL of your NextCloud instance",
                },
                ConfigField {
                    key: "username".into(),
                    label: "Username".into(),
                    field_type: FieldType::Text,
                    placeholder: "NextCloud username".into(),
                    secret: false,
                    required: true,
                    help: "NextCloud user account used to access Talk rooms",
                },
                ConfigField {
                    key: "password".into(),
                    label: "App Password".into(),
                    field_type: FieldType::Password,
                    placeholder: "NextCloud app password".into(),
                    secret: true,
                    required: true,
                    help: "App password generated in NextCloud security settings",
                },
                ConfigField {
                    key: "rooms".into(),
                    label: "Rooms".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "room-token-1\nroom-token-2".into(),
                    secret: false,
                    required: false,
                    help: "Talk room tokens to monitor, one per line",
                },
                ConfigField {
                    key: "poll_interval_secs".into(),
                    label: "Poll Interval (s)".into(),
                    field_type: FieldType::Number,
                    placeholder: "15".into(),
                    secret: false,
                    required: false,
                    help: "How often to poll for new messages in seconds",
                },
            ],
        },
        ChannelTypeInfo {
            id: "twitch".into(),
            name: "Twitch".into(),
            icon: "Tw".into(),
            description: "Connect to Twitch chat via IRC/TMI".into(),
            config_fields: vec![
                ConfigField {
                    key: "bot_username".into(),
                    label: "Bot Username".into(),
                    field_type: FieldType::Text,
                    placeholder: "twitch_bot_name".into(),
                    secret: false,
                    required: true,
                    help: "Twitch username the bot will use to join chat",
                },
                ConfigField {
                    key: "oauth_token".into(),
                    label: "OAuth Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "oauth:...".into(),
                    secret: true,
                    required: true,
                    help: "OAuth token starting with oauth: for Twitch IRC authentication",
                },
                ConfigField {
                    key: "channels".into(),
                    label: "Channels".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "channel_one\nchannel_two".into(),
                    secret: false,
                    required: true,
                    help: "Twitch channel names to join, one per line (without #)",
                },
                ConfigField {
                    key: "command_prefix".into(),
                    label: "Command Prefix".into(),
                    field_type: FieldType::Text,
                    placeholder: "!savfox".into(),
                    secret: false,
                    required: false,
                    help: "Prefix that triggers bot commands in chat messages",
                },
            ],
        },
        ChannelTypeInfo {
            id: "tlon".into(),
            name: "Tlon".into(),
            icon: "TL".into(),
            description: "Connect to an Urbit ship via Tlon / graph-store".into(),
            config_fields: vec![
                ConfigField {
                    key: "ship_url".into(),
                    label: "Ship URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "http://localhost:8080".into(),
                    secret: false,
                    required: true,
                    help: "The HTTP URL of your running Urbit ship",
                },
                ConfigField {
                    key: "access_code".into(),
                    label: "Access Code".into(),
                    field_type: FieldType::Password,
                    placeholder: "Urbit +code output".into(),
                    secret: true,
                    required: true,
                    help: "Access code from running +code in your Urbit dojo",
                },
                ConfigField {
                    key: "ship_name".into(),
                    label: "Ship Name".into(),
                    field_type: FieldType::Text,
                    placeholder: "~zod".into(),
                    secret: false,
                    required: true,
                    help: "Your Urbit ship name including the ~ prefix",
                },
                ConfigField {
                    key: "channels".into(),
                    label: "Channels".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "chat/~ship/group\nchat/~ship/another-group".into(),
                    secret: false,
                    required: false,
                    help: "Graph-store channel paths to monitor, one per line",
                },
            ],
        },
    ]
}

/// Extract health counts from channel status data.
fn compute_health_counts(
    channels_status: Option<&serde_json::Value>,
) -> (usize, usize, usize, usize) {
    let map = channels_status.and_then(|status| {
        status
            .get("instances")
            .and_then(Value::as_object)
            .filter(|instances| !instances.is_empty())
            .or_else(|| status.get("channels").and_then(Value::as_object))
    });

    let Some(m) = map else {
        return (0, 0, 0, 0);
    };

    let mut connected = 0usize;
    let mut running = 0usize;
    let mut disconnected = 0usize;
    let mut errored = 0usize;

    for v in m.values() {
        let is_configured = v
            .get("configured")
            .and_then(|r| r.as_bool())
            .unwrap_or(false);
        let is_running = v.get("running").and_then(|r| r.as_bool()).unwrap_or(false);
        let is_connected = v
            .get("connected")
            .and_then(|r| r.as_bool())
            .unwrap_or(false);
        let has_error = v.get("lastError").and_then(|r| r.as_str()).is_some();

        if is_running && is_connected {
            connected += 1;
        } else if is_running {
            running += 1;
        } else if is_configured {
            disconnected += 1;
        }

        if has_error {
            errored += 1;
        }
    }

    (connected, running, disconnected, errored)
}

fn format_uptime_compact(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let days = total_seconds / 86_400;
    let hours = (total_seconds % 86_400) / 3600;
    let minutes = (total_seconds % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

fn arkret_pairing_state_label(state: &str) -> String {
    match state {
        "paired" | "active" => "Paired".to_owned(),
        "pending_authorization" => "Pending authorization".to_owned(),
        "pending_runtime_key" => "Pending runtime key".to_owned(),
        other => other.to_owned(),
    }
}

fn arkret_runtime_phase_label(phase: &str) -> String {
    match phase {
        "scheduled" | "starting" => "Starting".to_owned(),
        "subscribing" => "Listening".to_owned(),
        "dispatching" => "Dispatching".to_owned(),
        "retry_wait" => "Retrying".to_owned(),
        "stopped" => "Stopped".to_owned(),
        other => other.to_owned(),
    }
}

fn value_to_field_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        Value::Array(_) | Value::Object(_) => Some(value.to_string()),
    }
}

fn split_config_list(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

fn namespace_patterns_to_text(value: Option<&Value>) -> String {
    let Some(Value::Array(items)) = value else {
        return String::new();
    };
    items
        .iter()
        .filter_map(|item| {
            if let Some(pattern) = item.as_str() {
                return Some(pattern.trim().to_owned());
            }
            item.as_object()
                .and_then(|obj| obj.get("pattern"))
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_owned)
        })
        .filter(|pattern| !pattern.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn namespace_patterns_from_text(value: &str, exclusive: bool) -> Vec<Value> {
    split_config_list(value)
        .into_iter()
        .map(|pattern| {
            json!({
                "pattern": pattern,
                "exclusive": exclusive,
            })
        })
        .collect()
}

fn parse_json_config_field(label: &str, value: &str) -> Result<Value, String> {
    serde_json::from_str::<Value>(value).map_err(|err| format!("{label} must be valid JSON: {err}"))
}

fn parse_arkret_agent_pairing_bootstrap(value: Value) -> Result<AgentPairingBootstrap, String> {
    let bootstrap: AgentPairingBootstrap = serde_json::from_value(value).map_err(|err| {
        format!("Inkson Bootstrap JSON must match CKP-0008 AgentPairingBootstrap: {err}")
    })?;
    for (name, value) in [
        ("arkret_base_url", bootstrap.arkret_base_url.as_str()),
        ("service_id", bootstrap.service_id.as_str()),
        ("agent_id", bootstrap.agent_id.as_str()),
        ("pairing_request_id", bootstrap.pairing_request_id.as_str()),
        ("pairing_code", bootstrap.pairing_code.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(format!(
                "Inkson Bootstrap JSON field {name} must not be empty."
            ));
        }
    }
    Ok(bootstrap)
}

fn field_value_key(channel_id: &str, field_key: &str) -> String {
    format!("{channel_id}.{field_key}")
}

fn channel_name_key(channel_id: &str) -> String {
    format!("{channel_id}.__name")
}

fn arkret_pairing_link_input_key(channel_id: &str) -> String {
    format!("{channel_id}.__pairing_link_input")
}

fn arkret_pairing_state_key(channel_id: &str) -> String {
    format!("{channel_id}.__pairing_state")
}

fn saved_channel_id_key(channel_id: &str) -> String {
    format!("{channel_id}.__saved_channel_id")
}

fn arkret_unbind_confirm_key(channel_id: &str) -> String {
    format!("{channel_id}.__unbind_confirm")
}

fn compute_channel_id(name: &str, kind: &str) -> String {
    let slug = normalize_slug(name).unwrap_or_else(|| "default".to_string());
    format!("{kind}-{slug}")
}

fn channel_form_id(
    channel_id: &str,
    channel_name: &str,
    values: &std::collections::HashMap<String, String>,
) -> String {
    values
        .get(&saved_channel_id_key(channel_id))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| compute_channel_id(channel_name, channel_id))
}

fn router_mode_key(channel_id: &str) -> String {
    format!("{channel_id}.__router_mode")
}

fn router_agent_id_key(channel_id: &str) -> String {
    format!("{channel_id}.__router_agent_id")
}

fn router_default_agent_id_key(channel_id: &str) -> String {
    format!("{channel_id}.__router_default_agent_id")
}

fn router_rules_key(channel_id: &str) -> String {
    format!("{channel_id}.__router_rules_json")
}

fn auto_channel_id(kind: &str, name: &str) -> String {
    let kind_slug = normalize_slug(kind).unwrap_or_else(|| "channel".to_string());
    let name_slug = normalize_slug(name).unwrap_or_else(|| "default".to_string());
    format!("{kind_slug}-{name_slug}")
}

fn restore_router_values(
    channel_id: &str,
    restored: &mut std::collections::HashMap<String, String>,
    router_value: Option<&Value>,
) {
    let Some(router) = router_value.and_then(Value::as_object) else {
        return;
    };
    let Some(router_type) = router.get("type").and_then(Value::as_str) else {
        return;
    };

    restored.insert(router_mode_key(channel_id), router_type.to_string());

    match router_type {
        "agent_id" => {
            if let Some(agent_id) = router.get("agent_id").and_then(Value::as_str) {
                restored.insert(router_agent_id_key(channel_id), agent_id.to_string());
            }
        }
        "route_rules" => {
            if let Some(default_agent_id) = router
                .get("default_agent_id")
                .or_else(|| router.get("default_agent"))
                .and_then(Value::as_str)
            {
                restored.insert(
                    router_default_agent_id_key(channel_id),
                    default_agent_id.to_string(),
                );
            }
            if let Some(rules) = router.get("rules") {
                let rules_text =
                    serde_json::to_string_pretty(rules).unwrap_or_else(|_| rules.to_string());
                restored.insert(router_rules_key(channel_id), rules_text);
            }
        }
        _ => {}
    }
}

fn restore_channel_values(
    channel_id: &str,
    fields: &[ConfigField],
    saved: &Value,
) -> std::collections::HashMap<String, String> {
    let mut restored = std::collections::HashMap::<String, String>::new();

    // Restore channel name
    if let Some(name) = saved.get("name").and_then(|v| v.as_str()) {
        restored.insert(channel_name_key(channel_id), name.to_string());
    }

    if let Some(config_obj) = saved.get("config").and_then(Value::as_object) {
        for field in fields {
            if let Some(raw) = config_obj.get(&field.key)
                && let Some(text) = value_to_field_text(raw)
            {
                restored.insert(field_value_key(channel_id, &field.key), text);
            }
        }

        if is_arkret_channel(fields) {
            restore_arkret_derived_values(channel_id, &mut restored, config_obj);
        }
    }

    restore_router_values(channel_id, &mut restored, saved.get("router"));
    restore_policy_values(
        channel_id,
        &mut restored,
        saved.get("dm_policy"),
        saved.get("group_policy"),
    );
    restored
}

fn restore_arkret_derived_values(
    channel_id: &str,
    restored: &mut std::collections::HashMap<String, String>,
    config_obj: &serde_json::Map<String, Value>,
) {
    let namespaces = config_obj.get("namespaces").and_then(Value::as_object);
    if let Some(namespaces) = namespaces {
        let actors = namespace_patterns_to_text(namespaces.get("actors"));
        if !actors.is_empty() {
            restored.insert(field_value_key(channel_id, "namespaceActors"), actors);
        }
        let realms = namespace_patterns_to_text(namespaces.get("realms"));
        if !realms.is_empty() {
            restored.insert(field_value_key(channel_id, "namespaceRealms"), realms);
        }
        let handles = namespace_patterns_to_text(namespaces.get("handles"));
        if !handles.is_empty() {
            restored.insert(field_value_key(channel_id, "namespaceHandles"), handles);
        }
    }

    for key in ["inksonBootstrap", "keyRef", "trustedVerificationMethods"] {
        if let Some(raw) = config_obj.get(key) {
            let rendered = serde_json::to_string_pretty(raw).unwrap_or_else(|_| raw.to_string());
            restored.insert(field_value_key(channel_id, key), rendered);
        }
    }
    if arkret_config_has_advanced_values(config_obj) {
        restored.insert(field_value_key(channel_id, "advanced"), "true".to_string());
    }
}

fn arkret_config_has_advanced_values(config_obj: &serde_json::Map<String, Value>) -> bool {
    let mode = config_obj
        .get("mode")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "agent".to_string());
    if mode != "applet" {
        return false;
    }
    let applet_advanced = [
        "botActorId",
        "arkretServerDid",
        "ghostDidPrefix",
        "requestedScopes",
        "receiveEvents",
        "receiveEphemeral",
        "rateLimited",
        "authorizationGrantId",
        "registrationEpoch",
        "trustedVerificationMethods",
        "loginChallenge",
        "verificationMethod",
        "grantEventPath",
        "keyRef",
    ];
    applet_advanced
        .iter()
        .any(|key| config_obj.get(*key).is_some_and(arkret_value_is_present))
}

fn arkret_value_is_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

fn current_router_mode(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> String {
    values
        .get(&router_mode_key(channel_id))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| matches!(value.as_str(), "agent_id" | "route_rules"))
        .unwrap_or_default()
}

fn build_router_value(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> Result<Value, String> {
    let router_mode = current_router_mode(channel_id, values);
    if router_mode.is_empty() {
        return Ok(Value::Null);
    }

    if router_mode == "agent_id" {
        let agent_id = values
            .get(&router_agent_id_key(channel_id))
            .map(|value| value.trim())
            .unwrap_or("");
        if agent_id.is_empty() {
            return Err("Routing mode 'Single agent' requires an agent ID.".to_string());
        }
        return Ok(json!({
            "type": "agent_id",
            "agent_id": agent_id,
        }));
    }

    let rules_text = values
        .get(&router_rules_key(channel_id))
        .map(|value| value.trim())
        .unwrap_or("");
    let parsed_rules = if rules_text.is_empty() {
        None
    } else {
        Some(
            serde_json::from_str::<Value>(rules_text)
                .map_err(|err| format!("Router rules JSON is invalid: {err}"))?,
        )
    };

    let mut default_agent_id = values
        .get(&router_default_agent_id_key(channel_id))
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    if default_agent_id.is_empty()
        && let Some(Value::Object(obj)) = parsed_rules.as_ref()
    {
        default_agent_id = obj
            .get("default_agent_id")
            .or_else(|| obj.get("default_agent"))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
    }
    if default_agent_id.is_empty() {
        default_agent_id = "default".to_string();
    }

    let rules = match parsed_rules {
        None => Value::Array(Vec::new()),
        Some(Value::Array(rules)) => Value::Array(rules),
        Some(Value::Object(obj)) => match obj.get("rules") {
            Some(rules) if rules.is_array() => rules.clone(),
            Some(_) => {
                return Err("Router rules JSON object must contain an array `rules`.".to_string());
            }
            None => Value::Array(Vec::new()),
        },
        Some(_) => {
            return Err(
                "Router rules JSON must be an array or an object containing `rules`.".to_string(),
            );
        }
    };

    Ok(json!({
        "type": "route_rules",
        "default_agent_id": default_agent_id,
        "rules": rules,
    }))
}

// ---- Policy key helpers ----

fn dm_policy_mode_key(channel_id: &str) -> String {
    format!("{channel_id}.__dm_policy_mode")
}

fn dm_policy_list_key(channel_id: &str) -> String {
    format!("{channel_id}.__dm_policy_list")
}

fn group_policy_mode_key(channel_id: &str) -> String {
    format!("{channel_id}.__group_policy_mode")
}

fn group_policy_list_key(channel_id: &str) -> String {
    format!("{channel_id}.__group_policy_list")
}

fn restore_policy_values(
    channel_id: &str,
    restored: &mut std::collections::HashMap<String, String>,
    dm_policy: Option<&Value>,
    group_policy: Option<&Value>,
) {
    if let Some(dm) = dm_policy.and_then(Value::as_object) {
        if let Some(mode) = dm.get("mode").and_then(Value::as_str) {
            restored.insert(dm_policy_mode_key(channel_id), mode.to_string());
        }
        let list = dm
            .get("allowlist")
            .or_else(|| dm.get("denylist"))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if !list.is_empty() {
            restored.insert(dm_policy_list_key(channel_id), list);
        }
    }
    if let Some(grp) = group_policy.and_then(Value::as_object) {
        if let Some(mode) = grp.get("mode").and_then(Value::as_str) {
            restored.insert(group_policy_mode_key(channel_id), mode.to_string());
        }
        let list = grp
            .get("allowlist")
            .or_else(|| grp.get("denylist"))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if !list.is_empty() {
            restored.insert(group_policy_list_key(channel_id), list);
        }
    }
}

fn build_policy_value(mode: &str, list_text: &str) -> Value {
    let mode = mode.trim().to_ascii_lowercase();
    if mode.is_empty() || mode == "open" {
        return Value::Null;
    }
    let entries: Vec<String> = list_text
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let list_key = if mode == "denylist" {
        "denylist"
    } else {
        "allowlist"
    };
    json!({
        "mode": mode,
        list_key: entries,
    })
}

fn build_dm_policy(channel_id: &str, values: &std::collections::HashMap<String, String>) -> Value {
    let mode = values
        .get(&dm_policy_mode_key(channel_id))
        .map(|v| v.as_str())
        .unwrap_or("");
    let list = values
        .get(&dm_policy_list_key(channel_id))
        .map(|v| v.as_str())
        .unwrap_or("");
    build_policy_value(mode, list)
}

fn build_group_policy(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> Value {
    let mode = values
        .get(&group_policy_mode_key(channel_id))
        .map(|v| v.as_str())
        .unwrap_or("");
    let list = values
        .get(&group_policy_list_key(channel_id))
        .map(|v| v.as_str())
        .unwrap_or("");
    build_policy_value(mode, list)
}

async fn persist_channel_form(
    ws: &WsRpc,
    channel_id: &str,
    fields: &[ConfigField],
    values: &std::collections::HashMap<String, String>,
) -> Result<Value, String> {
    let channel_name = values
        .get(&channel_name_key(channel_id))
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "default".to_string());
    let channel_computed_id = channel_form_id(channel_id, &channel_name, values);
    let patch = build_channel_patch(channel_id, fields, values)?;
    let router = build_router_value(channel_id, values)?;
    let dm_policy = build_dm_policy(channel_id, values);
    let group_policy = build_group_policy(channel_id, values);

    ws.call::<Value>(
        "channels.config.save",
        Some(json!({
            "channel": channel_id,
            "id": channel_computed_id,
            "name": channel_name,
            "config": patch,
            "router": router,
            "dm_policy": dm_policy,
            "group_policy": group_policy,
        })),
    )
    .await
    .map_err(|error| error.to_string())
}

fn build_appservice_registration_from_values(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> serde_json::Value {
    let get = |key: &str| -> String {
        values
            .get(&field_value_key(channel_id, key))
            .cloned()
            .unwrap_or_default()
    };

    let server_name = get("serverName");
    let public_url = get("publicUrl");
    let appservice_id = get("appserviceId");
    let appservice_token = get("appserviceToken");
    let homeserver_token = get("homeserverToken");
    let sender_localpart = get("senderLocalpart");
    let user_prefix = {
        let v = get("userPrefix");
        if v.is_empty() {
            "_savfox_".to_string()
        } else {
            v
        }
    };
    let alias_prefix = {
        let v = get("aliasPrefix");
        if v.is_empty() {
            "_savfox_".to_string()
        } else {
            v
        }
    };

    json!({
        "id": appservice_id,
        "url": public_url,
        "as_token": appservice_token,
        "hs_token": homeserver_token,
        "sender_localpart": sender_localpart,
        "rate_limited": false,
        "namespaces": {
            "users": [
                {
                    "exclusive": true,
                    "regex": format!("@{}:{}$", sender_localpart, server_name)
                },
                {
                    "exclusive": true,
                    "regex": format!("@{}.*:{}$", user_prefix, server_name)
                }
            ],
            "aliases": [
                {
                    "exclusive": true,
                    "regex": format!("#{}.*:{}$", alias_prefix, server_name)
                }
            ],
            "rooms": []
        }
    })
}

fn is_matrix_channel(fields: &[ConfigField]) -> bool {
    fields.iter().any(|field| field.key == "mode")
        && fields.iter().any(|field| field.key == "homeserver")
}

fn is_arkret_channel(fields: &[ConfigField]) -> bool {
    fields.iter().any(|field| field.key == "mode")
        && fields.iter().any(|field| field.key == "appletId")
        && fields.iter().any(|field| field.key == "inksonBootstrap")
        && fields.iter().any(|field| field.key == "keyRef")
}

fn current_matrix_mode(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> String {
    values
        .get(&field_value_key(channel_id, "mode"))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| value == "appservice")
        .unwrap_or_else(|| "user".to_string())
}

fn is_matrix_user_only_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "userId"
            | "accessToken"
            | "password"
            | "deviceName"
            | "encryption"
            | "dmPolicy"
            | "dmAllowFrom"
            | "groupPolicy"
            | "autoJoin"
            | "autoJoinAllowlist"
            | "allowedSenders"
    )
}

fn is_matrix_appservice_only_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "serverName"
            | "publicUrl"
            | "appserviceId"
            | "appserviceToken"
            | "homeserverToken"
            | "senderLocalpart"
            | "userPrefix"
            | "aliasPrefix"
    )
}

fn current_arkret_mode(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> String {
    values
        .get(&field_value_key(channel_id, "mode"))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| value == "applet")
        .unwrap_or_else(|| "agent".to_string())
}

fn arkret_advanced_enabled(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> bool {
    values
        .get(&field_value_key(channel_id, "advanced"))
        .map(|value| value == "true")
        .unwrap_or(false)
}

fn is_arkret_internal_field(field_key: &str) -> bool {
    matches!(field_key, "deviceId")
}

fn field_display_label(
    ch_id: &str,
    field: &ConfigField,
    values: &std::collections::HashMap<String, String>,
) -> String {
    if ch_id == "arkret" {
        let mode = current_arkret_mode(ch_id, values);
        match (field.key.as_str(), mode.as_str()) {
            ("baseUrl", "applet") => return "Applet URL".to_string(),
            ("baseUrl", _) => return "Arkret Base URL".to_string(),
            ("serviceId", "applet") => return "Applet Service DID".to_string(),
            ("serviceId", _) => return "Arkret Service DID".to_string(),
            ("accessToken", "applet") => return "Bearer Token".to_string(),
            _ => {}
        }
    }
    field.label.clone()
}

fn field_display_placeholder(
    ch_id: &str,
    field: &ConfigField,
    values: &std::collections::HashMap<String, String>,
) -> String {
    if ch_id == "arkret" {
        let mode = current_arkret_mode(ch_id, values);
        match (field.key.as_str(), mode.as_str()) {
            ("baseUrl", "applet") => {
                return "https://savfox.example/appservices/arkret/arkret-default".to_string();
            }
            ("baseUrl", _) => return "https://arkret.example.org".to_string(),
            ("serviceId", "applet") => return "did:web:savfox.example".to_string(),
            ("serviceId", _) => return "did:webvh:arkret.example.org".to_string(),
            ("accessToken", "applet") => return "applet bearer token".to_string(),
            _ => {}
        }
    }
    field.placeholder.clone()
}

fn field_display_help(
    ch_id: &str,
    field: &ConfigField,
    values: &std::collections::HashMap<String, String>,
) -> String {
    if ch_id == "arkret" {
        let mode = current_arkret_mode(ch_id, values);
        match (field.key.as_str(), mode.as_str()) {
            ("baseUrl", "applet") => {
                return "Public Savfox callback URL registered as the Arkret Applet endpoint."
                    .to_string();
            }
            ("baseUrl", _) => {
                return "Arkret server URL, normally parsed from the Inkson bootstrap.".to_string();
            }
            ("serviceId", "applet") => {
                return "Applet service DID registered with Arkret.".to_string();
            }
            ("serviceId", _) => {
                return "Arkret service DID used as the agent_key_proof audience.".to_string();
            }
            ("accessToken", "applet") => {
                return "Inbound applet bearer token configured in Arkret.".to_string();
            }
            _ => {}
        }
    }
    field.help.to_string()
}

fn field_display_required(
    ch_id: &str,
    field: &ConfigField,
    values: &std::collections::HashMap<String, String>,
) -> bool {
    if ch_id == "arkret" {
        let mode = current_arkret_mode(ch_id, values);
        if field.key == "serviceId" {
            return mode == "applet";
        }
        if mode != "applet" && field.key == "inksonBootstrap" {
            return true;
        }
    }
    field.required
}

fn is_arkret_account_only_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "inksonBootstrap"
            | "principalId"
            | "defaultRealmId"
            | "agentId"
            | "externalAiEndpointConfig"
            | "listen"
            | "send"
            | "requestedScope"
            | "authorizedEventRef"
            | "authorizationResult"
            | "runtimeKeyRequest"
            | "unbind"
    )
}

fn is_arkret_agent_hidden_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "advanced"
            | "baseUrl"
            | "serviceId"
            | "arkretServerDid"
            | "principalId"
            | "defaultRealmId"
            | "agentId"
            | "externalAiEndpointConfig"
            | "listen"
            | "send"
            | "requestedScope"
            | "verificationMethod"
            | "authorizedEventRef"
            | "keyRef"
            | "grantEventPath"
    )
}

fn is_arkret_applet_only_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "appletId"
            | "controllerId"
            | "botActorId"
            | "accessToken"
            | "loginChallenge"
            | "arkretServerUrl"
            | "protocols"
            | "requestedScopes"
            | "namespaceActors"
            | "namespaceRealms"
            | "namespaceHandles"
            | "ghostDidPrefix"
            | "receiveEvents"
            | "receiveEphemeral"
            | "rateLimited"
            | "authorizationGrantId"
            | "registrationEpoch"
            | "trustedVerificationMethods"
    )
}

fn is_arkret_helper_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "advanced"
            | "namespaceActors"
            | "namespaceRealms"
            | "namespaceHandles"
            | "authorizationResult"
            | "runtimeKeyRequest"
            | "unbind"
    )
}

fn is_arkret_advanced_field(field_key: &str, mode: &str) -> bool {
    matches!(field_key, "arkretServerDid")
        || (mode == "applet"
            && matches!(
                field_key,
                "botActorId"
                    | "ghostDidPrefix"
                    | "requestedScopes"
                    | "receiveEvents"
                    | "receiveEphemeral"
                    | "rateLimited"
                    | "authorizationGrantId"
                    | "registrationEpoch"
                    | "trustedVerificationMethods"
                    | "loginChallenge"
                    | "verificationMethod"
                    | "grantEventPath"
                    | "keyRef"
            ))
}

fn should_skip_hidden_arkret_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "deviceId"
            | "receiveEvents"
            | "receiveEphemeral"
            | "rateLimited"
            | "ghostDidPrefix"
            | "keyRef"
            | "verificationMethod"
            | "authorizedEventRef"
    )
}

fn field_is_visible(
    channel_id: &str,
    field: &ConfigField,
    values: &std::collections::HashMap<String, String>,
) -> bool {
    if channel_id == "matrix" {
        let matrix_mode = current_matrix_mode(channel_id, values);
        if is_matrix_user_only_field(&field.key) {
            return matrix_mode != "appservice";
        }
        if is_matrix_appservice_only_field(&field.key) {
            return matrix_mode == "appservice";
        }
    }
    if channel_id == "arkret" {
        let arkret_mode = current_arkret_mode(channel_id, values);
        if is_arkret_internal_field(&field.key) {
            return false;
        }
        if arkret_mode != "applet" && is_arkret_agent_hidden_field(&field.key) {
            return false;
        }
        if is_arkret_account_only_field(&field.key) && arkret_mode == "applet" {
            return false;
        }
        if is_arkret_applet_only_field(&field.key) && arkret_mode != "applet" {
            return false;
        }
        if field.key == "authorizationResult" && arkret_mode != "applet" {
            return false;
        }
        if is_arkret_advanced_field(&field.key, &arkret_mode) {
            return arkret_advanced_enabled(channel_id, values);
        }
    }
    true
}

fn build_channel_patch(
    channel_id: &str,
    fields: &[ConfigField],
    values: &std::collections::HashMap<String, String>,
) -> Result<Value, String> {
    if is_arkret_channel(fields) {
        return build_arkret_channel_patch(channel_id, fields, values);
    }

    let mut patch = json!({});
    let matrix_channel = is_matrix_channel(fields);
    for field in fields {
        if matrix_channel && !field_is_visible(channel_id, field, values) {
            patch[&field.key] = Value::Null;
            continue;
        }

        let key = field_value_key(channel_id, &field.key);
        if let Some(val) = values.get(&key) {
            if val.trim().is_empty() {
                if matrix_channel {
                    patch[&field.key] = Value::Null;
                }
                continue;
            }
            if matches!(field.field_type, FieldType::Toggle) {
                patch[&field.key] = json!(val == "true");
            } else {
                patch[&field.key] = json!(val);
            }
        }
    }
    Ok(patch)
}

fn build_arkret_channel_patch(
    channel_id: &str,
    fields: &[ConfigField],
    values: &std::collections::HashMap<String, String>,
) -> Result<Value, String> {
    let mut patch = json!({});
    let mode = current_arkret_mode(channel_id, values);
    patch["mode"] = json!(mode);

    for field in fields {
        if field.key == "mode" {
            continue;
        }

        if is_arkret_internal_field(&field.key) {
            continue;
        }

        if !field_is_visible(channel_id, field, values) {
            if !is_arkret_helper_field(&field.key) && !should_skip_hidden_arkret_field(&field.key) {
                patch[&field.key] = Value::Null;
            }
            continue;
        }

        if is_arkret_helper_field(&field.key) {
            continue;
        }

        let key = field_value_key(channel_id, &field.key);
        let value = values
            .get(&key)
            .map(|value| value.trim())
            .unwrap_or_default();
        if value.is_empty() {
            patch[&field.key] = Value::Null;
            continue;
        }

        match field.key.as_str() {
            "keyRef" => {
                let parsed = parse_json_config_field("Local Runtime Key", value)?;
                if !parsed.is_object() {
                    return Err("Local Runtime Key must be a JSON object.".to_string());
                }
                patch[&field.key] = parsed;
            }
            "inksonBootstrap" => {
                if !value.starts_with('{') {
                    return Err(
                        "Resolve the Inkson pairing link before saving the Arkret agent channel."
                            .to_string(),
                    );
                }
                let parsed = parse_json_config_field("Inkson Bootstrap JSON", value)?;
                parse_arkret_agent_pairing_bootstrap(parsed.clone())?;
                patch[&field.key] = parsed;
            }
            "trustedVerificationMethods" => {
                let parsed = parse_json_config_field("Trusted Verification Methods JSON", value)?;
                if !parsed.is_array() {
                    return Err(
                        "Trusted Verification Methods JSON must be a JSON array.".to_string()
                    );
                }
                patch[&field.key] = parsed;
            }
            "requestedScopes" => {
                patch[&field.key] = json!(split_config_list(value));
            }
            _ if matches!(field.field_type, FieldType::Toggle) => {
                patch[&field.key] = json!(value == "true");
            }
            _ => {
                patch[&field.key] = json!(value);
            }
        }
    }

    if mode == "applet" {
        let actors = values
            .get(&field_value_key(channel_id, "namespaceActors"))
            .map(|value| namespace_patterns_from_text(value, true))
            .unwrap_or_default();
        let realms = values
            .get(&field_value_key(channel_id, "namespaceRealms"))
            .map(|value| namespace_patterns_from_text(value, true))
            .unwrap_or_default();
        let handles = values
            .get(&field_value_key(channel_id, "namespaceHandles"))
            .map(|value| namespace_patterns_from_text(value, false))
            .unwrap_or_default();
        if actors.is_empty() && realms.is_empty() && handles.is_empty() {
            patch["namespaces"] = Value::Null;
        } else {
            patch["namespaces"] = json!({
                "actors": actors,
                "realms": realms,
                "handles": handles,
            });
        }
    } else {
        patch["namespaces"] = Value::Null;
        clear_arkret_agent_obsolete_fields(&mut patch);
        apply_arkret_hidden_agent_runtime_values(channel_id, values, &mut patch)?;
        apply_arkret_bootstrap_defaults(&mut patch);
        validate_arkret_agent_runtime_request_inputs(&patch)?;
    }

    Ok(patch)
}

fn apply_arkret_hidden_agent_runtime_values(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
    patch: &mut Value,
) -> Result<(), String> {
    if let Some(value) = values
        .get(&field_value_key(channel_id, "keyRef"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        let parsed = parse_json_config_field("Local Runtime Key", value)?;
        if !parsed.is_object() {
            return Err("Local Runtime Key must be a JSON object.".to_string());
        }
        patch["keyRef"] = parsed;
    }

    if let Some(value) = values
        .get(&field_value_key(channel_id, "verificationMethod"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        patch["verificationMethod"] = json!(value);
    }

    let authorized_event_ref = values
        .get(&field_value_key(channel_id, "authorizedEventRef"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            values
                .get(&field_value_key(channel_id, "authorizationResult"))
                .and_then(|value| extract_arkret_authorized_event_ref(value))
        });
    if let Some(authorized_event_ref) = authorized_event_ref {
        patch["authorizedEventRef"] = json!(authorized_event_ref);
    }

    Ok(())
}

fn validate_arkret_agent_runtime_request_inputs(patch: &Value) -> Result<(), String> {
    if patch
        .get("inksonBootstrap")
        .and_then(Value::as_object)
        .is_none_or(|object| object.is_empty())
    {
        return Err(
            "Arkret agent mode requires Inkson Bootstrap JSON before runtime key approval."
                .to_string(),
        );
    }
    if patch
        .get("keyRef")
        .and_then(Value::as_object)
        .is_none_or(|object| object.is_empty())
    {
        return Err(
            "Arkret agent mode needs a generated local runtime key. Click Request approval so Savfox can generate it automatically."
                .to_string(),
        );
    }
    if patch
        .get("verificationMethod")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        return Err(
            "Arkret agent mode needs a runtime verification method derived from the resolved Inkson bootstrap."
                .to_string(),
        );
    }
    Ok(())
}

fn apply_arkret_bootstrap_defaults(patch: &mut Value) {
    let Some(bootstrap_value) = patch.get("inksonBootstrap").cloned() else {
        return;
    };
    let Ok(bootstrap) = parse_arkret_agent_pairing_bootstrap(bootstrap_value) else {
        return;
    };

    if patch_value_empty(patch.get("baseUrl")) {
        patch["baseUrl"] = Value::Null;
    }
    if patch_value_empty(patch.get("serviceId")) {
        patch["serviceId"] = Value::Null;
    }
    if patch_value_empty(patch.get("principalId")) {
        patch["principalId"] = Value::Null;
    }
    if patch_value_empty(patch.get("requestedScope")) {
        patch["requestedScope"] = Value::Null;
    }
    let agent_id = bootstrap.agent_id.to_string();
    let existing_verification_method = patch
        .get("verificationMethod")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if existing_verification_method
        .is_none_or(|value| !arkret_verification_method_matches_agent(value, &agent_id))
    {
        if existing_verification_method.is_some() {
            patch["authorizedEventRef"] = Value::Null;
        }
        patch["verificationMethod"] = json!(format!("{agent_id}#runtime-1"));
    }
}

fn arkret_verification_method_matches_agent(verification_method: &str, agent_id: &str) -> bool {
    verification_method
        .split_once('#')
        .is_some_and(|(did, fragment)| did == agent_id && !fragment.is_empty())
}

fn clear_arkret_agent_obsolete_fields(patch: &mut Value) {
    for key in [
        "baseUrl",
        "serviceId",
        "arkretServerDid",
        "deviceId",
        "principalId",
        "defaultRealmId",
        "agentId",
        "externalAiEndpointConfig",
        "listen",
        "send",
        "requestedScope",
        "grantEventPath",
    ] {
        patch[key] = Value::Null;
    }
}

fn patch_value_empty(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.trim().is_empty(),
        Some(Value::Array(items)) => items.is_empty(),
        Some(Value::Object(map)) => map.is_empty(),
        Some(Value::Bool(_)) | Some(Value::Number(_)) => false,
    }
}

fn extract_arkret_authorized_event_ref(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && let Some(event_ref) = find_arkret_authorized_event_ref_in_value(&value)
    {
        return Some(event_ref);
    }
    find_arkret_event_ref_in_text(trimmed)
}

fn find_arkret_authorized_event_ref_in_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => find_arkret_event_ref_in_text(text),
        Value::Array(items) => items
            .iter()
            .find_map(find_arkret_authorized_event_ref_in_value),
        Value::Object(map) => {
            for key in [
                "authorized_event_ref",
                "authorizedEventRef",
                "authorization_event_ref",
                "authorizationEventRef",
                "event_id",
                "eventId",
                "ref",
            ] {
                if let Some(value) = map.get(key)
                    && let Some(event_ref) = find_arkret_authorized_event_ref_in_value(value)
                {
                    return Some(event_ref);
                }
            }
            map.values()
                .find_map(find_arkret_authorized_event_ref_in_value)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

fn find_arkret_event_ref_in_text(text: &str) -> Option<String> {
    for (start, _) in text.match_indices("ak:event:") {
        let rest = &text[start..];
        let end = rest
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, ':' | '-' | '_')))
            .unwrap_or(rest.len());
        let candidate = rest[..end].trim();
        if candidate.len() > "ak:event:".len() {
            return Some(candidate.to_owned());
        }
    }
    None
}

fn default_channel_values(
    channel_id: &str,
    fields: &[ConfigField],
) -> std::collections::HashMap<String, String> {
    let mut values = std::collections::HashMap::new();
    if is_arkret_channel(fields) {
        values.insert(field_value_key(channel_id, "mode"), "agent".to_string());
        values.insert(field_value_key(channel_id, "listen"), "true".to_string());
        values.insert(field_value_key(channel_id, "send"), "true".to_string());
        values.insert(
            field_value_key(channel_id, "receiveEvents"),
            "true".to_string(),
        );
        values.insert(
            field_value_key(channel_id, "receiveEphemeral"),
            "false".to_string(),
        );
        values.insert(
            field_value_key(channel_id, "rateLimited"),
            "true".to_string(),
        );
        values.insert(
            field_value_key(channel_id, "ghostDidPrefix"),
            "ghost:".to_string(),
        );
    }
    values
}

enum ChannelDeepLink {
    None,
    Add,
    Edit(String),
    Health(String),
}

#[component]
pub fn Channels() -> Element {
    channels_inner(ChannelDeepLink::None)
}

#[component]
pub fn ChannelsAdd() -> Element {
    channels_inner(ChannelDeepLink::Add)
}

#[component]
pub fn ChannelsEdit(channel_id: String) -> Element {
    channels_inner(ChannelDeepLink::Edit(channel_id))
}

#[component]
pub fn ChannelsHealth(channel_id: String) -> Element {
    channels_inner(ChannelDeepLink::Health(channel_id))
}

fn channels_inner(deep_link: ChannelDeepLink) -> Element {
    inject_channels_styles_once();
    let is_routed = !matches!(deep_link, ChannelDeepLink::None);
    let nav = use_navigator();

    let initial_add = matches!(&deep_link, ChannelDeepLink::Add);
    let initial_edit = match &deep_link {
        ChannelDeepLink::Edit(id) => Some(id.clone()),
        _ => Option::None,
    };
    let initial_health = match &deep_link {
        ChannelDeepLink::Health(id) => Some(id.clone()),
        _ => Option::None,
    };

    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut show_add_modal = use_signal(move || initial_add);
    let mut selected_channel = use_signal(|| Option::<String>::None);
    let mut config_values: Signal<std::collections::HashMap<String, String>> =
        use_signal(|| std::collections::HashMap::new());
    let mut saving = use_signal(|| false);
    let mut save_msg = use_signal(|| Option::<String>::None);
    let mut auto_refresh = use_signal(|| false);
    let mut show_raw_json = use_signal(move || initial_health);
    let mut testing_channel = use_signal(|| Option::<String>::None);
    let mut test_result = use_signal(|| Option::<(String, bool, String)>::None);
    let mut config_modal_channel = use_signal(move || initial_edit);
    let mut add_channel_search = use_signal(String::new);
    let mut add_channel_name = use_signal(String::new);
    let mut initialized_add_channel = use_signal(|| Option::<String>::None);
    let mut modal_revealed: Signal<std::collections::HashSet<String>> =
        use_signal(|| std::collections::HashSet::new());
    let mut open_channel_menu = use_signal(|| Option::<String>::None);
    let mut confirm_card_delete = use_signal(|| Option::<String>::None);
    let mut deleting_channel = use_signal(|| Option::<String>::None);

    // Sync URL with modal state for deep linking
    use_effect(move || {
        let edit = config_modal_channel();
        let add = show_add_modal();
        let health = show_raw_json();

        if let Some(ref id) = edit {
            replace_url(&format!("/channels/edit/{id}"));
        } else if add {
            replace_url("/channels/add");
        } else if let Some(ref id) = health {
            replace_url(&format!("/channels/health/{id}"));
        } else if is_routed {
            nav.replace(crate::route::Route::Channels {});
        } else {
            replace_url("/channels");
        }
    });

    let channel_types = get_channel_types();

    let ws_list = ws.clone();
    let channels_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_list.clone();
        async move {
            ws.wait_connected().await;
            ws.call::<serde_json::Value>("channels.status", Some(json!({ "probe": false })))
                .await
                .ok()
        }
    });

    let ws_configs = ws.clone();
    let channel_configs_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_configs.clone();
        async move {
            ws.wait_connected().await;
            ws.call::<serde_json::Value>("channels.config.list", None)
                .await
                .ok()
        }
    });

    let modal_channel_types = channel_types;
    use_effect(move || {
        let modal_open = show_add_modal();
        let selected = selected_channel();
        if !modal_open {
            add_channel_name.set(String::new());
            initialized_add_channel.set(None);
            modal_revealed.write().clear();
            return;
        }

        let Some(channel_id) = selected else {
            config_values.write().clear();
            add_channel_name.set(String::new());
            initialized_add_channel.set(None);
            modal_revealed.write().clear();
            return;
        };
        if initialized_add_channel().as_deref() == Some(channel_id.as_str()) {
            return;
        }
        let default_name = modal_channel_types
            .iter()
            .find(|channel_type| channel_type.id == channel_id)
            .map(|channel_type| channel_type.name.clone())
            .unwrap_or_else(|| channel_id.clone());
        let config_summaries = {
            let configs_read = channel_configs_data.read();
            let Some(configs_result) = configs_read.as_ref() else {
                return;
            };
            let configs = configs_result.as_ref();
            saved_channel_summaries(configs)
        };
        let default_name =
            next_available_channel_name(&channel_id, &default_name, &config_summaries);
        let selected_fields = modal_channel_types
            .iter()
            .find(|channel_type| channel_type.id == channel_id)
            .map(|channel_type| channel_type.config_fields.clone())
            .unwrap_or_default();
        add_channel_name.set(default_name.clone());
        config_values.set(new_channel_form_values(
            &channel_id,
            &selected_fields,
            default_name,
        ));
        initialized_add_channel.set(Some(channel_id));
    });

    // Auto-refresh every 30s when enabled (uses spawn to avoid leaked closures)
    use_effect(move || {
        if !auto_refresh() {
            return;
        }
        spawn(async move {
            loop {
                let promise = js_sys::Promise::new(&mut |resolve, _| {
                    if let Some(win) = web_sys::window() {
                        let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(
                            &resolve, 30_000,
                        );
                    }
                });
                let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                if auto_refresh() {
                    refresh_tick += 1;
                } else {
                    break;
                }
            }
        });
    });

    let configs_read = channel_configs_data.read();
    let configs_ref = configs_read.as_ref().and_then(|c| c.as_ref());
    let saved_channel_configs = saved_channel_summaries(configs_ref);

    let is_loading = channels_data.read().is_none() || channel_configs_data.read().is_none();

    let channels_read = channels_data.read();
    let channels_status = channels_read.as_ref().and_then(|c| c.as_ref());

    // Health counts
    let (connected_count, running_count, disconnected_count, error_count) =
        compute_health_counts(channels_status);

    let status_channels = channels_status
        .and_then(|s| s.get("channels"))
        .and_then(|c| c.as_object());
    let mut configured_channel_cards: Vec<(&ChannelTypeInfo, Option<&SavedChannelSummary>)> =
        saved_channel_configs
            .iter()
            .filter_map(|config| {
                channel_types
                    .iter()
                    .find(|channel_type| channel_type.id == config.kind)
                    .map(|channel_type| (channel_type, Some(config)))
            })
            .collect();
    for channel_type in channel_types {
        let already_has_saved_instance = saved_channel_configs
            .iter()
            .any(|config| config.kind == channel_type.id);
        if already_has_saved_instance {
            continue;
        }
        let Some(channel) =
            status_channels.and_then(|channels| channels.get(channel_type.id.as_str()))
        else {
            continue;
        };
        let has_runtime_state = channel
            .get("configured")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || channel
                .get("running")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || channel
                .get("connected")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if has_runtime_state {
            configured_channel_cards.push((channel_type, None));
        }
    }

    let total_configured = connected_count + running_count + disconnected_count;

    rsx! {
        div { class: "channels-page",
            // ---- Health Dashboard ----
            div { class: "channels-health",
                div { class: "channels-health__card channels-health__card--connected",
                    div { class: "channels-health__value", "{connected_count}" }
                    div { class: "channels-health__label", "Connected" }
                    div { class: "channels-health__indicator" }
                }
                div { class: "channels-health__card channels-health__card--running",
                    div { class: "channels-health__value", "{running_count}" }
                    div { class: "channels-health__label", "Running" }
                }
                div { class: "channels-health__card channels-health__card--disconnected",
                    div { class: "channels-health__value", "{disconnected_count}" }
                    div { class: "channels-health__label", "Disconnected" }
                }
                div { class: "channels-health__card channels-health__card--errors",
                    div { class: "channels-health__value", "{error_count}" }
                    div { class: "channels-health__label", "Errors" }
                }
            }

            // ---- Toolbar ----
            div { class: "channels-toolbar",
                div { class: "channels-toolbar__left",
                    h2 { class: "channels-toolbar__title", "Channels" }
                    p { class: "channels-toolbar__subtitle",
                        "Configure messaging platform integrations"
                    }
                }
                div { class: "channels-toolbar__actions",
                    Chip {
                        label: format!("{connected_count} active"),
                        variant: if connected_count > 0 { ChipVariant::Success } else { ChipVariant::Muted },
                    }
                    Chip {
                        label: format!("{total_configured} configured"),
                        variant: ChipVariant::Info,
                    }

                    button {
                        onclick: move |_| auto_refresh.toggle(),
                        class: if auto_refresh() { "channels-btn channels-btn--auto-active" } else { "channels-btn" },
                        if auto_refresh() { "Auto (30s)" } else { "Auto-refresh" }
                    }
                    button {
                        onclick: move |_| refresh_tick += 1,
                        class: "channels-btn",
                        "Refresh"
                    }
                    button {
                        onclick: move |_| {
                            show_add_modal.set(true);
                            selected_channel.set(None);
                            config_values.write().clear();
                            add_channel_search.set(String::new());
                            save_msg.set(None);
                        },
                        class: "channels-btn channels-btn--primary",
                        "+ Add Channel"
                    }
                }
            }

            // ---- Test result toast ----
            if let Some((ref ch_id, success, ref msg)) = test_result() {
                div {
                    class: if success { "channels-toast channels-toast--success" } else { "channels-toast channels-toast--error" },
                    onclick: move |_| test_result.set(None),
                    span { class: "channels-toast__text",
                        "{ch_id}: {msg}"
                    }
                    span { class: "channels-toast__dismiss", "x" }
                }
            }

            // ---- Channel grid ----
            if is_loading {
                div { class: "channels-empty",
                    SkeletonCard {}
                }
            } else if configured_channel_cards.is_empty() {
                div { class: "channels-empty", "No channels configured yet. Click + Add Channel to configure one." }
            } else {
                div { class: "channels-grid",
                    for (ch_type, config_entry) in configured_channel_cards.into_iter() {
                        { render_channel_card(
                            ch_type,
                            config_entry,
                            channels_status,
                            ws.clone(),
                            refresh_tick,
                            show_raw_json,
                            testing_channel,
                            test_result,
                            config_modal_channel,
                            open_channel_menu,
                            confirm_card_delete,
                            deleting_channel,
                        ) }
                    }
                }
            }
        }

        // ---- Raw JSON / Health modal ----
        if let Some(ref ch_id) = show_raw_json() {
            {
                let channel_data = channels_status
                    .and_then(|status| {
                        status
                            .get("instances")
                            .and_then(|instances| instances.get(ch_id.as_str()))
                            .or_else(|| {
                                status
                                    .get("channels")
                                    .and_then(|channels| channels.get(ch_id.as_str()))
                            })
                    });
                let raw_json = channel_data
                    .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                    .unwrap_or_else(|| "No data available".to_string());
                let ch_id_clone = ch_id.clone();

                let health_is_connected = channel_data
                    .and_then(|c| c.get("connected"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let health_is_running = channel_data
                    .and_then(|c| c.get("running"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let health_last_error = channel_data
                    .and_then(|c| c.get("lastError"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let (health_dot_class, health_status_text) = if health_is_running && health_is_connected {
                    ("channels-health-dot channels-health-dot--connected", "Connected")
                } else if health_is_running {
                    ("channels-health-dot channels-health-dot--running", "Running")
                } else if health_last_error.is_some() {
                    ("channels-health-dot channels-health-dot--error", "Error")
                } else {
                    ("channels-health-dot channels-health-dot--disconnected", "Disconnected")
                };

                let reconnect_ch_id = channel_data
                    .and_then(|data| data.get("platform"))
                    .and_then(Value::as_str)
                    .unwrap_or(ch_id)
                    .to_owned();
                let ws_reconnect = ws.clone();

                rsx! {
                    div {
                        class: "channels-modal-backdrop",
                        onmousedown: move |_| show_raw_json.set(None),
                        div {
                            class: "channels-modal channels-modal--wide",
                            onmousedown: |e| e.stop_propagation(),
                            div { class: "channels-modal__header",
                                div { class: "channels-modal__header-left",
                                    h3 { class: "channels-modal__title",
                                        "Health: {ch_id_clone}"
                                        if let Some(config) = saved_channel_configs.iter().find(|config| config.id == *ch_id || config.kind == *ch_id) {
                                            span { style: "font-size:13px;font-weight:400;color:var(--text-muted);margin-left:8px;",
                                                "({config.name}"
                                                span { style: "margin-left:4px;font-size:11px;color:var(--text-muted);opacity:0.7;", "{config.id}" }
                                                ")"
                                            }
                                        }
                                    }
                                    div { class: "channels-health-status",
                                        span { class: "{health_dot_class}" }
                                        span { class: "channels-health-status__text", "{health_status_text}" }
                                    }
                                }
                                button {
                                    onclick: move |_| show_raw_json.set(None),
                                    class: "channels-modal__close",
                                    "x"
                                }
                            }

                            if let Some(ref err) = health_last_error {
                                div { class: "channels-health-error",
                                    div { class: "channels-health-error__header",
                                        span { class: "channels-health-dot channels-health-dot--error" }
                                        span { class: "channels-health-error__label", "Last Error" }
                                    }
                                    div { class: "channels-health-error__message", "{err}" }
                                    button {
                                        onclick: move |_| {
                                            let ws = ws_reconnect.clone();
                                            let platform = reconnect_ch_id.clone();
                                            spawn(async move {
                                                let _ = ws.call::<serde_json::Value>(
                                                    "channels.login",
                                                    Some(json!({ "platform": platform })),
                                                ).await;
                                                refresh_tick += 1;
                                            });
                                        },
                                        class: "channels-action-btn channels-action-btn--test",
                                        "Reconnect"
                                    }
                                }
                            }

                            pre { class: "channels-modal__pre",
                                "{raw_json}"
                            }
                        }
                    }
                }
            }
        }

        // ---- Add Channel Modal ----
        if show_add_modal() {
            { render_add_modal(
                channel_types,
                selected_channel,
                config_values,
                saving,
                save_msg,
                ws.clone(),
                show_add_modal,
                refresh_tick,
                add_channel_search,
                add_channel_name,
                modal_revealed,
            ) }
        }

        // ---- Channel Config Modal ----
        if let Some(ref modal_selector) = config_modal_channel() {
            {
                let selected_config = saved_channel_configs
                    .iter()
                    .find(|config| config.id == *modal_selector);
                let channel_kind = selected_config
                    .map(|config| config.kind.as_str())
                    .unwrap_or(modal_selector.as_str());
                let ch_type = channel_types.iter().find(|ct| ct.id == channel_kind);
                if let Some(ct) = ch_type {
                    rsx! {
                        ChannelConfigModal {
                            channel_id: ct.id.clone(),
                            config_selector: modal_selector.clone(),
                            channel_name: selected_config
                                .map(|config| config.name.clone())
                                .unwrap_or_else(|| ct.name.clone()),
                            fields: ct.config_fields.clone(),
                            ws: ws.clone(),
                            refresh_tick,
                            testing_channel,
                            test_result,
                            config_modal_channel,
                        }
                    }
                } else {
                    rsx! {}
                }
            }
        }
    }
}

fn render_channel_card(
    ch_type: &ChannelTypeInfo,
    config_entry: Option<&SavedChannelSummary>,
    channels_status: Option<&serde_json::Value>,
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    mut show_raw_json: Signal<Option<String>>,
    mut testing_channel: Signal<Option<String>>,
    mut test_result: Signal<Option<(String, bool, String)>>,
    mut config_modal_channel: Signal<Option<String>>,
    mut open_channel_menu: Signal<Option<String>>,
    mut confirm_card_delete: Signal<Option<String>>,
    mut deleting_channel: Signal<Option<String>>,
) -> Element {
    let config_id = config_entry.map(|config| config.id.clone());
    let config_name = config_entry.map(|config| config.name.clone());
    let channel_data = config_id
        .as_deref()
        .and_then(|instance_id| {
            channels_status
                .and_then(|status| status.get("instances"))
                .and_then(|instances| instances.get(instance_id))
        })
        .or_else(|| {
            channels_status
                .and_then(|status| status.get("channels"))
                .and_then(|channels| channels.get(&ch_type.id))
        });

    let status_configured = channel_data
        .and_then(|c| c.get("configured"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let has_saved_config = config_entry.is_some();
    let is_configured = status_configured || has_saved_config;

    let is_running = channel_data
        .and_then(|c| c.get("running"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let is_connected = channel_data
        .and_then(|c| c.get("connected"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let is_enabled = config_entry
        .map(|config| config.enabled)
        .unwrap_or(is_running || is_connected);
    let card_key = config_id
        .clone()
        .unwrap_or_else(|| format!("{}-runtime", ch_type.id));

    let last_error = channel_data
        .and_then(|c| c.get("lastError").or_else(|| c.get("last_error")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let last_activity_str = channel_data
        .and_then(|c| c.get("lastActivity"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let last_message_time_str = channel_data
        .and_then(|c| c.get("lastMessageTime"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let last_event_time_str = channel_data
        .and_then(|c| c.get("lastEventTime"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let uptime_str = channel_data
        .and_then(|c| c.get("uptimeMs"))
        .and_then(|v| v.as_u64())
        .map(format_uptime_compact)
        .unwrap_or_else(|| "-".to_string());
    let error_rate_str = channel_data
        .and_then(|c| c.get("errorRate"))
        .and_then(|v| v.as_f64())
        .map(|v| format!("{:.1}%", v * 100.0))
        .unwrap_or_else(|| "0.0%".to_string());
    let messages_total = channel_data
        .and_then(|c| c.get("messages_total"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let messages_failed = channel_data
        .and_then(|c| c.get("messages_failed"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Platform-specific metadata
    let bot_username = channel_data
        .and_then(|c| c.get("bot_username"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let guild_count = channel_data
        .and_then(|c| c.get("guild_count"))
        .and_then(|v| v.as_u64());
    let matrix_user_id = channel_data
        .and_then(|c| c.get("user_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let room_count = channel_data
        .and_then(|c| c.get("room_count"))
        .and_then(|v| v.as_u64());
    let nostr_public_key = channel_data
        .and_then(|c| c.get("public_key"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let relay_count = channel_data
        .and_then(|c| c.get("relay_count"))
        .and_then(|v| v.as_u64());
    let arkret_pairing_state = channel_data
        .and_then(|c| c.get("runtime_pairing_state"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let arkret_pairing_label = arkret_pairing_state
        .as_deref()
        .map(arkret_pairing_state_label);
    let arkret_runtime_phase = channel_data
        .and_then(|c| c.get("runtime_phase"))
        .and_then(|v| v.as_str())
        .map(|phase| phase.to_owned());
    let arkret_runtime_label = arkret_runtime_phase
        .as_deref()
        .map(arkret_runtime_phase_label);
    let arkret_authorized_event_ref = channel_data
        .and_then(|c| c.get("authorized_event_ref"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let arkret_verification_method = channel_data
        .and_then(|c| c.get("verification_method"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let (status_variant, status_text) = if is_running && is_connected {
        (ChipVariant::Success, "Connected")
    } else if is_running {
        (ChipVariant::Warning, "Running")
    } else if last_error.is_some()
        || (ch_type.id == "arkret" && arkret_runtime_phase.as_deref() == Some("retry_wait"))
    {
        (ChipVariant::Danger, "Needs attention")
    } else if ch_type.id == "arkret"
        && matches!(
            arkret_runtime_phase.as_deref(),
            Some("scheduled" | "starting")
        )
    {
        (ChipVariant::Warning, "Starting")
    } else if is_configured {
        (ChipVariant::Info, "Configured")
    } else {
        (ChipVariant::Muted, "Not configured")
    };

    let border_class = if is_running && is_connected {
        "channels-card channels-card--connected"
    } else if is_running {
        "channels-card channels-card--running"
    } else if last_error.is_some() {
        "channels-card channels-card--error"
    } else if is_configured {
        "channels-card channels-card--configured"
    } else {
        "channels-card channels-card--disabled"
    };

    let platform = ch_type.id.clone();
    let platform_health = config_id.clone().unwrap_or_else(|| ch_type.id.clone());
    let platform_test = ch_type.id.clone();
    let platform_test_name = config_name.clone().unwrap_or_else(|| ch_type.name.clone());
    let platform_test_quick = ch_type.id.clone();
    let platform_test_quick_name = config_name.clone().unwrap_or_else(|| ch_type.name.clone());
    let platform_toggle_menu = ch_type.id.clone();
    let instance_test_menu = config_id.clone();
    let instance_test_quick = config_id.clone();
    let config_id_toggle_menu = config_id.clone();
    let config_name_toggle_menu = config_name.clone();
    let config_id_toggle_quick = config_id.clone();
    let config_name_toggle_quick = config_name.clone();
    let ws_login = ws.clone();
    let ws_login_menu = ws.clone();
    let ws_test = ws.clone();
    let ws_test_quick = ws.clone();
    let ws_delete = ws.clone();

    let is_testing = testing_channel().as_deref() == Some(card_key.as_str());
    let config_selector = config_id.clone().unwrap_or_else(|| ch_type.id.clone());
    let menu_is_open = open_channel_menu().as_deref() == Some(card_key.as_str());
    let testing_key_menu = card_key.clone();
    let testing_key_quick = card_key.clone();
    let delete_is_confirming = config_id
        .as_deref()
        .is_some_and(|id| confirm_card_delete().as_deref() == Some(id));
    let delete_is_running = config_id
        .as_deref()
        .is_some_and(|id| deleting_channel().as_deref() == Some(id));

    rsx! {
        div {
            key: "{card_key}",
            class: "{border_class}",

            // ---- Card header ----
            div { class: "channels-card__header",
                div { class: "channels-card__identity",
                    span { class: "channels-card__icon", "{ch_type.icon}" }
                    div { class: "channels-card__meta",
                        div { class: "channels-card__name", "{ch_type.name}" }
                        div { class: "channels-card__desc", "{ch_type.description}" }
                        if let Some(ref cfg_id) = config_id {
                            div { class: "channels-card__config-id",
                                span { class: "channels-card__config-id-label", "id" }
                                " {cfg_id}"
                                if let Some(ref cfg_name) = config_name {
                                    span { class: "channels-card__config-id-sep", " / " }
                                    span { class: "channels-card__config-name", "{cfg_name}" }
                                }
                            }
                        }
                    }
                }
                div { class: "channels-card__header-actions",
                    Chip { label: status_text.to_string(), variant: status_variant }
                    div { class: "channels-card__menu-wrap",
                        button {
                            class: "channels-card__menu-trigger",
                            title: "Channel actions",
                            aria_label: "Channel actions for {ch_type.name}",
                            onclick: {
                                let menu_id = card_key.clone();
                                move |_| {
                                    if open_channel_menu().as_deref() == Some(menu_id.as_str()) {
                                        open_channel_menu.set(None);
                                        confirm_card_delete.set(None);
                                    } else {
                                        open_channel_menu.set(Some(menu_id.clone()));
                                        confirm_card_delete.set(None);
                                    }
                                }
                            },
                            Settings { size: 16 }
                        }
                        if menu_is_open {
                            div { class: "channels-card__menu",
                                button {
                                    class: "channels-card__menu-item",
                                    onclick: move |_| {
                                        let ws = ws_login_menu.clone();
                                        let platform = platform_toggle_menu.clone();
                                        let config_id = config_id_toggle_menu.clone();
                                        let config_name = config_name_toggle_menu.clone();
                                        let new_enabled = !is_enabled;
                                        open_channel_menu.set(None);
                                        spawn(async move {
                                            let _ = ws.call::<serde_json::Value>(
                                                "channels.config.save",
                                                Some(json!({
                                                    "channel": platform,
                                                    "id": config_id,
                                                    "name": config_name,
                                                    "config": { "enabled": new_enabled },
                                                })),
                                            ).await;
                                            if new_enabled {
                                                let _ = ws.call::<serde_json::Value>(
                                                    "channels.login",
                                                    Some(json!({
                                                        "platform": platform,
                                                        "id": config_id,
                                                    })),
                                                ).await;
                                            } else {
                                                let _ = ws.call::<serde_json::Value>(
                                                    "channels.logout",
                                                    Some(json!({
                                                        "platform": platform,
                                                        "id": config_id,
                                                    })),
                                                ).await;
                                            }
                                            refresh_tick += 1;
                                        });
                                    },
                                    Power { size: 14 }
                                    span { if is_enabled { "Disable" } else { "Enable" } }
                                }
                                button {
                                    class: "channels-card__menu-item",
                                    onclick: move |_| {
                                        open_channel_menu.set(None);
                                        config_modal_channel.set(Some(config_selector.clone()));
                                    },
                                    SlidersHorizontal { size: 14 }
                                    span { "Settings" }
                                }
                                button {
                                    class: "channels-card__menu-item",
                                    disabled: is_testing,
                                    onclick: move |_| {
                                        let ws = ws_test.clone();
                                        let platform = platform_test.clone();
                                        let instance_id = instance_test_menu.clone();
                                        let name = platform_test_name.clone();
                                        open_channel_menu.set(None);
                                        testing_channel.set(Some(testing_key_menu.clone()));
                                        test_result.set(None);
                                        spawn(async move {
                                            let result = ws.call::<serde_json::Value>(
                                                "channels.test",
                                                Some(json!({
                                                    "platform": platform,
                                                    "id": instance_id,
                                                })),
                                            ).await;
                                            testing_channel.set(None);
                                            let (ok, msg) = match result {
                                                Ok(val) => {
                                                    let ok = val.get("ok")
                                                        .and_then(Value::as_bool)
                                                        .unwrap_or(false);
                                                    let msg = val.get("message")
                                                        .and_then(Value::as_str)
                                                        .unwrap_or(if ok { "Connection successful" } else { "Test failed" })
                                                        .to_string();
                                                    (ok, msg)
                                                }
                                                Err(error) => (false, error.to_string()),
                                            };
                                            test_result.set(Some((name, ok, msg)));
                                        });
                                    },
                                    Activity { size: 14 }
                                    span { if is_testing { "Testing..." } else { "Test connection" } }
                                }
                                button {
                                    class: "channels-card__menu-item",
                                    onclick: move |_| {
                                        open_channel_menu.set(None);
                                        show_raw_json.set(Some(platform_health.clone()));
                                    },
                                    Braces { size: 14 }
                                    span { "Health JSON" }
                                }
                                if let Some(ref delete_id) = config_id {
                                    div { class: "channels-card__menu-separator" }
                                    if delete_is_confirming {
                                        div { class: "channels-card__menu-confirm",
                                            span { "Stop and delete {delete_id}?" }
                                            div { class: "channels-card__menu-confirm-actions",
                                                button {
                                                    class: "channels-card__menu-confirm-delete",
                                                    disabled: delete_is_running,
                                                    onclick: {
                                                        let ws = ws_delete.clone();
                                                        let delete_id = delete_id.to_string();
                                                        let channel_name = ch_type.name.clone();
                                                        move |_| {
                                                            let ws = ws.clone();
                                                            let delete_id = delete_id.clone();
                                                            let channel_name = channel_name.clone();
                                                            deleting_channel.set(Some(delete_id.clone()));
                                                            spawn(async move {
                                                                let result = ws.call::<serde_json::Value>(
                                                                    "channels.config.delete",
                                                                    Some(json!({ "channel": delete_id.clone() })),
                                                                ).await;
                                                                deleting_channel.set(None);
                                                                confirm_card_delete.set(None);
                                                                open_channel_menu.set(None);
                                                                match result {
                                                                    Ok(value) if value.get("deleted").and_then(Value::as_bool).unwrap_or(false) => {
                                                                        test_result.set(Some((channel_name, true, "Channel stopped and deleted.".into())));
                                                                        refresh_tick += 1;
                                                                    }
                                                                    Ok(_) => test_result.set(Some((channel_name, false, "Channel configuration was not found.".into()))),
                                                                    Err(error) => test_result.set(Some((channel_name, false, format!("Delete failed: {error}")))),
                                                                }
                                                            });
                                                        }
                                                    },
                                                    if delete_is_running { "Deleting..." } else { "Confirm" }
                                                }
                                                button {
                                                    class: "channels-card__menu-confirm-cancel",
                                                    disabled: delete_is_running,
                                                    onclick: move |_| confirm_card_delete.set(None),
                                                    "Cancel"
                                                }
                                            }
                                        }
                                    } else {
                                        button {
                                            class: "channels-card__menu-item channels-card__menu-item--danger",
                                            onclick: {
                                                let delete_id = delete_id.clone();
                                                move |_| confirm_card_delete.set(Some(delete_id.clone()))
                                            },
                                            Trash2 { size: 14 }
                                            span { "Stop and delete" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ---- Platform-specific info ----
            {
                let has_platform_info = bot_username.is_some() || guild_count.is_some()
                    || matrix_user_id.is_some() || room_count.is_some()
                    || nostr_public_key.is_some() || relay_count.is_some()
                    || arkret_pairing_label.is_some()
                    || arkret_runtime_label.is_some()
                    || arkret_authorized_event_ref.is_some()
                    || arkret_verification_method.is_some();
                rsx! {
                    if has_platform_info {
                        div { class: "channels-card__platform-info",
                            if let Some(ref username) = bot_username {
                                span { class: "channels-card__pinfo-item",
                                    span { class: "channels-card__pinfo-label", "Bot" }
                                    span { class: "channels-card__pinfo-value", "@{username}" }
                                }
                            }
                            if let Some(count) = guild_count {
                                span { class: "channels-card__pinfo-item",
                                    span { class: "channels-card__pinfo-label", "Guilds" }
                                    span { class: "channels-card__pinfo-value", "{count}" }
                                }
                            }
                            if let Some(ref uid) = matrix_user_id {
                                span { class: "channels-card__pinfo-item",
                                    span { class: "channels-card__pinfo-label", "User" }
                                    span { class: "channels-card__pinfo-value", "{uid}" }
                                }
                            }
                            if let Some(count) = room_count {
                                span { class: "channels-card__pinfo-item",
                                    span { class: "channels-card__pinfo-label", "Rooms" }
                                    span { class: "channels-card__pinfo-value", "{count}" }
                                }
                            }
                            if let Some(ref pk) = nostr_public_key {
                                span { class: "channels-card__pinfo-item",
                                    span { class: "channels-card__pinfo-label", "Pubkey" }
                                    span { class: "channels-card__pinfo-value channels-card__pinfo-value--truncate", "{pk}" }
                                }
                            }
                            if let Some(count) = relay_count {
                                span { class: "channels-card__pinfo-item",
                                    span { class: "channels-card__pinfo-label", "Relays" }
                                    span { class: "channels-card__pinfo-value", "{count}" }
                                }
                            }
                            if let Some(label) = arkret_pairing_label {
                                span { class: "channels-card__pinfo-item",
                                    span { class: "channels-card__pinfo-label", "Pairing" }
                                    span { class: "channels-card__pinfo-value", "{label}" }
                                }
                            }
                            if let Some(label) = arkret_runtime_label {
                                span { class: "channels-card__pinfo-item",
                                    span { class: "channels-card__pinfo-label", "Runtime" }
                                    span { class: "channels-card__pinfo-value", "{label}" }
                                }
                            }
                            if let Some(ref event_ref) = arkret_authorized_event_ref {
                                span { class: "channels-card__pinfo-item",
                                    span { class: "channels-card__pinfo-label", "Auth ref" }
                                    span { class: "channels-card__pinfo-value channels-card__pinfo-value--truncate", "{event_ref}" }
                                }
                            }
                            if let Some(ref verification_method) = arkret_verification_method {
                                span { class: "channels-card__pinfo-item",
                                    span { class: "channels-card__pinfo-label", "VM" }
                                    span { class: "channels-card__pinfo-value channels-card__pinfo-value--truncate", "{verification_method}" }
                                }
                            }
                        }
                    }
                }
            }

            // ---- Last activity ----
            if let Some(ref activity) = last_activity_str {
                div { class: "channels-card__activity",
                    "Last activity: {activity}"
                }
            }

            // ---- Health metrics ----
            div { class: "channels-card__metrics",
                div { class: "channels-card__metric",
                    span { class: "channels-card__metric-label", "Last message" }
                    span { class: "channels-card__metric-value",
                        "{last_message_time_str.as_deref().unwrap_or(\"-\")}"
                    }
                }
                div { class: "channels-card__metric",
                    span { class: "channels-card__metric-label", "Last event" }
                    span { class: "channels-card__metric-value",
                        "{last_event_time_str.as_deref().unwrap_or(\"-\")}"
                    }
                }
                div { class: "channels-card__metric",
                    span { class: "channels-card__metric-label", "Uptime" }
                    span { class: "channels-card__metric-value", "{uptime_str}" }
                }
                div { class: "channels-card__metric",
                    span { class: "channels-card__metric-label", "Error rate" }
                    span { class: "channels-card__metric-value", "{error_rate_str}" }
                }
                div { class: "channels-card__metric",
                    span { class: "channels-card__metric-label", "Sent" }
                    span { class: "channels-card__metric-value", "{messages_total}" }
                }
                div { class: "channels-card__metric",
                    span { class: "channels-card__metric-label", "Failed" }
                    span { class: "channels-card__metric-value", "{messages_failed}" }
                }
            }

            // ---- Error banner ----
            if let Some(ref err) = last_error {
                div { class: "channels-card__error",
                    "Error: {err}"
                }
            }

            // ---- Action buttons ----
            div { class: "channels-card__actions",
                // Enabled toggle
                button {
                    onclick: move |_| {
                        let ws = ws_login.clone();
                        let platform = platform.clone();
                        let config_id = config_id_toggle_quick.clone();
                        let config_name = config_name_toggle_quick.clone();
                        let new_enabled = !is_enabled;
                        spawn(async move {
                            let _ = ws.call::<serde_json::Value>(
                                "channels.config.save",
                                Some(json!({
                                    "channel": platform,
                                    "id": config_id,
                                    "name": config_name,
                                    "config": { "enabled": new_enabled },
                                })),
                            ).await;
                            if new_enabled {
                                let _ = ws.call::<serde_json::Value>(
                                    "channels.login",
                                    Some(json!({
                                        "platform": platform,
                                        "id": config_id,
                                    })),
                                ).await;
                            } else {
                                let _ = ws.call::<serde_json::Value>(
                                    "channels.logout",
                                    Some(json!({
                                        "platform": platform,
                                        "id": config_id,
                                    })),
                                ).await;
                            }
                            refresh_tick += 1;
                        });
                    },
                    class: if is_enabled {
                        "channels-action-btn channels-action-btn--configured"
                    } else {
                        "channels-action-btn"
                    },
                    if is_enabled {
                        "\u{2713} Enabled"
                    } else {
                        "Disabled"
                    }
                }

                // Test remains a quick action and is also available in the menu.
                button {
                    onclick: move |_| {
                        let ws = ws_test_quick.clone();
                        let platform = platform_test_quick.clone();
                        let instance_id = instance_test_quick.clone();
                        let name = platform_test_quick_name.clone();
                        testing_channel.set(Some(testing_key_quick.clone()));
                        test_result.set(None);
                        spawn(async move {
                            let result = ws.call::<serde_json::Value>(
                                "channels.test",
                                Some(json!({
                                    "platform": platform,
                                    "id": instance_id,
                                })),
                            ).await;
                            testing_channel.set(None);
                            let (ok, msg) = match result {
                                Ok(val) => {
                                    let ok = val.get("ok")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false);
                                    let msg = val.get("message")
                                        .and_then(Value::as_str)
                                        .unwrap_or(if ok { "Connection successful" } else { "Test failed" })
                                        .to_string();
                                    (ok, msg)
                                }
                                Err(error) => (false, error.to_string()),
                            };
                            test_result.set(Some((name, ok, msg)));
                        });
                    },
                    disabled: is_testing,
                    class: "channels-action-btn channels-action-btn--test",
                    if is_testing {
                        span { style: "display:inline-flex;align-items:center;gap:4px;",
                            span {
                                style: "display:inline-block;width:12px;height:12px;border:2px solid var(--text-muted);border-top-color:transparent;border-radius:50%;animation:spin 0.8s linear infinite;",
                            }
                            "Testing..."
                        }
                    } else {
                        "Test"
                    }
                }

                // Inline test result for this card.
                {
                    let card_ch_id = card_key.clone();
                    let card_config_name = config_name.clone();
                    let card_result = test_result().and_then(|(ref id, ok, ref msg)| {
                        if id == &ch_type.name
                            || id == &card_ch_id
                            || card_config_name.as_ref().is_some_and(|name| id == name)
                        {
                            Some((ok, msg.clone()))
                        } else {
                            None
                        }
                    });
                    if let Some((success, ref msg)) = card_result {
                        rsx! {
                            span {
                                style: if success {
                                    "font-size:11px;color:#22c55e;font-weight:500;"
                                } else {
                                    "font-size:11px;color:#ef4444;font-weight:500;"
                                },
                                if success { "Connection successful" } else { "{msg}" }
                            }
                        }
                    } else {
                        rsx! { span { style: "display:none;" } }
                    }
                }

            }

        }
    }
}

// ---------------------------------------------------------------------------
// Channel configuration modal
// ---------------------------------------------------------------------------

/// Renders channel configuration in a modal dialog instead of inline.
#[component]
fn ChannelConfigModal(
    channel_id: String,
    config_selector: String,
    channel_name: String,
    fields: Vec<ConfigField>,
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    mut testing_channel: Signal<Option<String>>,
    mut test_result: Signal<Option<(String, bool, String)>>,
    mut config_modal_channel: Signal<Option<String>>,
) -> Element {
    let mut inline_values: Signal<std::collections::HashMap<String, String>> =
        use_signal(|| std::collections::HashMap::new());
    let mut inline_saving = use_signal(|| false);
    let mut deleting = use_signal(|| false);
    let mut confirm_delete = use_signal(|| false);
    let mut saved_channel_id = use_signal(|| Option::<String>::None);
    let mut inline_msg = use_signal(|| Option::<(bool, String)>::None);
    let mut revealed: Signal<std::collections::HashSet<String>> =
        use_signal(|| std::collections::HashSet::new());

    let ch_id = channel_id;
    let ch_name = channel_name;
    let fields_vec = fields;
    let ws_save = ws.clone();
    let ws_test = ws.clone();
    let ws_delete = ws.clone();
    let ws_load = ws.clone();
    let load_selector = config_selector;
    let load_ch_id = ch_id.clone();
    let load_fields = fields_vec.clone();

    use_effect(move || {
        let ws = ws_load.clone();
        let ch_id = load_ch_id.clone();
        let selector = load_selector.clone();
        let fields = load_fields.clone();
        spawn(async move {
            let result = ws
                .call::<serde_json::Value>(
                    "channels.config.get",
                    Some(json!({ "channel": selector })),
                )
                .await;
            let Ok(payload) = result else {
                return;
            };
            let Some(saved) = payload.get("config") else {
                return;
            };
            if saved.is_null() {
                saved_channel_id.set(None);
                return;
            }
            let restored_channel_id = saved
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            saved_channel_id.set(restored_channel_id.clone());
            let mut restored = default_channel_values(&ch_id, &fields);
            restored.extend(restore_channel_values(&ch_id, &fields, saved));
            if let Some(restored_channel_id) = restored_channel_id {
                restored.insert(saved_channel_id_key(&ch_id), restored_channel_id);
            }
            inline_values.set(restored);
        });
    });

    rsx! {
        div {
            class: "channels-modal-backdrop",
            onmousedown: move |_| config_modal_channel.set(None),
            div {
                class: "channels-modal channels-modal--wide",
                onmousedown: |e| e.stop_propagation(),
                div { class: "channels-modal__header",
                    h3 { class: "channels-modal__title", "Configure {ch_name}" }
                    button {
                        onclick: move |_| config_modal_channel.set(None),
                        class: "channels-modal__close",
                        "x"
                    }
                }

                // Status message
                if let Some((ok, ref msg)) = inline_msg() {
                    div {
                        class: if ok { "channels-cfg__alert channels-cfg__alert--success" } else { "channels-cfg__alert channels-cfg__alert--error" },
                        "{msg}"
                    }
                }

                // Channel name + auto-computed ID
                {
                    let name_key = channel_name_key(&ch_id);
                    let current_name = inline_values.read().get(&name_key).cloned().unwrap_or_else(|| "default".to_string());
                    let displayed_id = {
                        let values = inline_values.read();
                        channel_form_id(&ch_id, &current_name, &values)
                    };
                    let name_key_input = name_key.clone();
                    rsx! {
                        div { class: "channels-cfg__field",
                            label { class: "channels-field__label",
                                "Name"
                                span { class: "channels-field__required", " *" }
                            }
                            input {
                                r#type: "text",
                                placeholder: "default",
                                value: "{current_name}",
                                oninput: move |e| { inline_values.write().insert(name_key_input.clone(), e.value()); },
                                class: "channels-field__input",
                            }
                        }
                        div { class: "channels-cfg__field",
                            label { class: "channels-field__label",
                                "ID"
                            }
                            input {
                                r#type: "text",
                                value: "{displayed_id}",
                                readonly: true,
                                class: "channels-field__input channels-field__input--readonly",
                            }
                        }
                    }
                }

                // Render fields
                { render_config_fields(
                    &ch_id,
                    &fields_vec,
                    inline_values,
                    revealed,
                    ws.clone(),
                    refresh_tick,
                ) }

                // Discord callback URL section
                if ch_id == "discord" {
                    { render_discord_callback_url() }
                }

                { render_router_fields(&ch_id, inline_values) }
                { render_policy_fields(&ch_id, inline_values) }

                // Action row
                div { class: "channels-cfg__actions",
                    {
                        let show_general_save = {
                            let values = inline_values.read();
                            ch_id != "arkret"
                                || current_arkret_mode(&ch_id, &values) == "applet"
                                || arkret_agent_is_bound(&ch_id, &values)
                        };
                        if show_general_save {
                            rsx! {
                                button {
                                    onclick: {
                                        let ws = ws_save.clone();
                                        let ch_id = ch_id.clone();
                                        let fields = fields_vec.clone();
                                        move |_| {
                                            let ws = ws.clone();
                                            let ch_id = ch_id.clone();
                                            let fields = fields.clone();
                                            let values = inline_values.read().clone();
                                            inline_saving.set(true);
                                            spawn(async move {
                                                match persist_channel_form(
                                                    &ws,
                                                    &ch_id,
                                                    &fields,
                                                    &values,
                                                )
                                                .await
                                                {
                                                    Ok(_) => {
                                                        inline_saving.set(false);
                                                        config_modal_channel.set(None);
                                                        refresh_tick += 1;
                                                    }
                                                    Err(error) => {
                                                        inline_saving.set(false);
                                                        inline_msg.set(Some((
                                                            false,
                                                            format!("Save failed: {error}"),
                                                        )));
                                                    }
                                                }
                                            });
                                        }
                                    },
                                    disabled: inline_saving(),
                                    class: "channels-action-btn channels-action-btn--primary",
                                    if inline_saving() { "Saving..." } else { "Save changes" }
                                }
                            }
                        } else {
                            rsx! {}
                        }
                    }
                    button {
                        onclick: move |_| config_modal_channel.set(None),
                        disabled: inline_saving(),
                        class: "channels-action-btn",
                        "Cancel"
                    }

                    // Test Connection button (TASK-014: enhanced with spinner + inline result)
                    {
                        let modal_is_testing = testing_channel().as_deref() == Some(ch_id.as_str());
                        let modal_ch_id_result = ch_id.clone();
                        let modal_ch_name_result = ch_name.clone();
                        let modal_test_result = test_result().and_then(|(ref id, ok, ref msg)| {
                            if id == &modal_ch_name_result || id == &modal_ch_id_result {
                                Some((ok, msg.clone()))
                            } else {
                                None
                            }
                        });
                        rsx! {
                            div { style: "display:flex;align-items:center;gap:8px;flex-wrap:wrap;",
                                button {
                                    onclick: {
                                        let ws = ws_test.clone();
                                        let ch_id = ch_id.clone();
                                        let ch_name = ch_name.clone();
                                        let fields = fields_vec.clone();
                                        move |_| {
                                            let ws = ws.clone();
                                            let platform = ch_id.clone();
                                            let name = ch_name.clone();
                                            let fields = fields.clone();
                                            let vals = inline_values.read();
                                            let channel_name = vals
                                                .get(&channel_name_key(&platform))
                                                .filter(|v| !v.trim().is_empty())
                                                .cloned()
                                                .unwrap_or_else(|| "default".to_string());
                                            let channel_computed_id =
                                                channel_form_id(&platform, &channel_name, &vals);
                                            let config_patch = match build_channel_patch(&platform, &fields, &vals) {
                                                Ok(patch) => patch,
                                                Err(err) => {
                                                    testing_channel.set(None);
                                                    test_result.set(Some((name, false, err)));
                                                    return;
                                                }
                                            };
                                            testing_channel.set(Some(platform.clone()));
                                            test_result.set(None);
                                            spawn(async move {
                                                let result = ws.call::<serde_json::Value>(
                                                    "channels.test",
                                                    Some(json!({
                                                        "platform": platform,
                                                        "id": channel_computed_id,
                                                        "name": channel_name,
                                                        "config": config_patch,
                                                    })),
                                                ).await;
                                                testing_channel.set(None);
                                                let (ok, msg) = match result {
                                                    Ok(val) => {
                                                        let ok = val.get("ok")
                                                            .and_then(|v| v.as_bool())
                                                            .unwrap_or(false);
                                                        let msg = val.get("message")
                                                            .and_then(|v| v.as_str())
                                                            .unwrap_or(if ok { "Connection successful" } else { "Test failed" })
                                                            .to_string();
                                                        (ok, msg)
                                                    }
                                                    Err(e) => {
                                                        let reason = format!("{e}");
                                                        (false, reason)
                                                    }
                                                };
                                                test_result.set(Some((name, ok, msg)));
                                            });
                                        }
                                    },
                                    disabled: modal_is_testing,
                                    class: "channels-action-btn channels-action-btn--test",
                                    if modal_is_testing {
                                        span { style: "display:inline-flex;align-items:center;gap:4px;",
                                            span {
                                                style: "display:inline-block;width:12px;height:12px;border:2px solid var(--text-muted);border-top-color:transparent;border-radius:50%;animation:spin 0.8s linear infinite;",
                                            }
                                            "Testing..."
                                        }
                                    } else {
                                        "Test Connection"
                                    }
                                }
                                if let Some((success, ref msg)) = modal_test_result {
                                    span {
                                        style: if success {
                                            "font-size:12px;color:#22c55e;font-weight:500;"
                                        } else {
                                            "font-size:12px;color:#ef4444;font-weight:500;"
                                        },
                                        "{msg}"
                                    }
                                }
                            }
                        }
                    }

                    // Export Appservice Registration YAML (Matrix appservice only)
                    {
                        let is_matrix = is_matrix_channel(&fields_vec);
                        let is_appservice = is_matrix && {
                            let vals = inline_values.read();
                            current_matrix_mode(&ch_id, &vals) == "appservice"
                        };
                        if is_appservice {
                            let ch_id_export = ch_id.clone();
                            rsx! {
                                button {
                                    onclick: move |_| {
                                        let vals = inline_values.read();
                                        let reg = build_appservice_registration_from_values(&ch_id_export, &vals);
                                        let yaml = matrix::registration_to_yaml(&reg);
                                        spawn(async move {
                                            matrix::trigger_yaml_download(&yaml);
                                        });
                                    },
                                    class: "channels-action-btn",
                                    "Export Registration YAML"
                                }
                            }
                        } else {
                            rsx! {}
                        }
                    }

                    if let Some(delete_id) = saved_channel_id() {
                        div { class: "channels-cfg__delete-action",
                        if confirm_delete() {
                            div { class: "channels-cfg__delete-confirm",
                                span { "Delete {delete_id}? This cannot be undone." }
                                button {
                                    onclick: {
                                        let ws = ws_delete.clone();
                                        let delete_id = delete_id.clone();
                                        move |_| {
                                            let ws = ws.clone();
                                            let delete_id = delete_id.clone();
                                            deleting.set(true);
                                            inline_msg.set(None);
                                            spawn(async move {
                                                let result = ws
                                                    .call::<serde_json::Value>(
                                                        "channels.config.delete",
                                                        Some(json!({ "channel": delete_id })),
                                                    )
                                                    .await;
                                                deleting.set(false);
                                                match result {
                                                    Ok(value) if value
                                                        .get("deleted")
                                                        .and_then(Value::as_bool)
                                                        .unwrap_or(false) =>
                                                    {
                                                        confirm_delete.set(false);
                                                        config_modal_channel.set(None);
                                                        refresh_tick += 1;
                                                    }
                                                    Ok(_) => inline_msg.set(Some((
                                                        false,
                                                        "Channel configuration was not found.".into(),
                                                    ))),
                                                    Err(error) => inline_msg.set(Some((
                                                        false,
                                                        format!("Delete failed: {error}"),
                                                    ))),
                                                }
                                            });
                                        }
                                    },
                                    disabled: deleting(),
                                    class: "channels-action-btn channels-action-btn--danger",
                                    if deleting() { "Stopping and deleting..." } else { "Confirm delete" }
                                }
                                button {
                                    onclick: move |_| confirm_delete.set(false),
                                    disabled: deleting(),
                                    class: "channels-action-btn",
                                    "Cancel"
                                }
                            }
                        } else {
                            button {
                                onclick: move |_| confirm_delete.set(true),
                                class: "channels-action-btn channels-action-btn--danger",
                                "Delete"
                            }
                        }
                    }
                }
                }
            }
        }
    }
}

/// Builds the Discord OAuth2 callback URL from the current window origin.
fn discord_callback_url() -> String {
    web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .map(|origin| format!("{origin}/_discord/callback"))
        .unwrap_or_else(|| "/_discord/callback".to_string())
}

async fn copy_text_to_clipboard(text: String) {
    if let Some(window) = web_sys::window()
        && let Ok(nav) =
            js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("navigator"))
        && let Ok(clipboard) =
            js_sys::Reflect::get(&nav, &wasm_bindgen::JsValue::from_str("clipboard"))
        && let Ok(write_fn) =
            js_sys::Reflect::get(&clipboard, &wasm_bindgen::JsValue::from_str("writeText"))
    {
        let func: js_sys::Function = write_fn.into();
        if let Ok(promise) = func.call1(&clipboard, &wasm_bindgen::JsValue::from_str(&text)) {
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(promise)).await;
        }
    }
}

/// Renders the Discord OAuth callback URL section with a copy button.
fn render_discord_callback_url() -> Element {
    let url = discord_callback_url();
    let url_for_copy = url.clone();
    let mut copied = use_signal(|| false);

    rsx! {
        div { class: "channels-cfg__section",
            div { class: "channels-cfg__section-title", "OAuth2 Redirect URL" }
            div { class: "channels-field__hint",
                "Use this URL as the Redirect URI in your Discord application's OAuth2 settings."
            }
            div { class: "channels-cfg__field",
                label { class: "channels-field__label", "Callback URL" }
                div { class: "channels-cfg__password-wrap",
                    input {
                        r#type: "text",
                        value: "{url}",
                        readonly: true,
                        class: "channels-field__input channels-cfg__password-input",
                    }
                    button {
                        onclick: move |_| {
                            let text = url_for_copy.clone();
                            spawn(async move {
                                copy_text_to_clipboard(text).await;
                                copied.set(true);
                                let promise = js_sys::Promise::new(&mut |resolve, _| {
                                    if let Some(win) = web_sys::window() {
                                        let _ = win
                                            .set_timeout_with_callback_and_timeout_and_arguments_0(
                                                &resolve, 2000,
                                            );
                                    }
                                });
                                let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                                copied.set(false);
                            });
                        },
                        class: "channels-cfg__reveal-btn",
                        r#type: "button",
                        if copied() { "Copied!" } else { "Copy" }
                    }
                }
            }
        }
    }
}

/// Renders all configuration fields for a channel, dispatching by field type.
fn render_config_fields(
    ch_id: &str,
    fields: &[ConfigField],
    mut values: Signal<std::collections::HashMap<String, String>>,
    mut revealed: Signal<std::collections::HashSet<String>>,
    ws: WsRpc,
    refresh_tick: Signal<u32>,
) -> Element {
    rsx! {
        div { class: "channels-cfg__fields",
            for field in fields.iter() {
                { render_single_field(
                    ch_id,
                    field,
                    fields,
                    values,
                    revealed,
                    ws.clone(),
                    refresh_tick,
                ) }
            }
        }
    }
}

/// Renders a single configuration field based on its FieldType.
fn render_single_field(
    ch_id: &str,
    field: &ConfigField,
    fields: &[ConfigField],
    mut values: Signal<std::collections::HashMap<String, String>>,
    mut revealed: Signal<std::collections::HashSet<String>>,
    ws: WsRpc,
    refresh_tick: Signal<u32>,
) -> Element {
    let value_map = values.read();
    let should_render = field_is_visible(ch_id, field, &value_map);
    if !should_render {
        return rsx! {};
    }

    let key = format!("{}.{}", ch_id, field.key);
    let current_val = value_map.get(&key).cloned().unwrap_or_default();
    let display_label = field_display_label(ch_id, field, &value_map);
    let display_placeholder = field_display_placeholder(ch_id, field, &value_map);
    let help_text = field_display_help(ch_id, field, &value_map);
    let is_required = field_display_required(ch_id, field, &value_map);
    if ch_id == "arkret" && field.key == "inksonBootstrap" {
        if arkret_agent_is_bound(ch_id, &value_map) {
            drop(value_map);
            return rsx! {};
        }
        let pairing_code = arkret_pairing_code_from_bootstrap_text(&current_val);
        let pairing_expires_at = arkret_pairing_expiry_from_bootstrap_text(&current_val);
        let pairing_link_key = arkret_pairing_link_input_key(ch_id);
        let pairing_link = value_map
            .get(&pairing_link_key)
            .cloned()
            .unwrap_or_else(|| {
                if current_val.trim_start().starts_with('{') {
                    String::new()
                } else {
                    current_val.clone()
                }
            });
        let pairing_busy = value_map
            .get(&arkret_pairing_state_key(ch_id))
            .is_some_and(|state| matches!(state.as_str(), "starting" | "waiting" | "finalizing"));
        let key_for_input = key.clone();
        let pairing_link_key_for_input = pairing_link_key.clone();
        let key_ref_key_for_input = field_value_key(ch_id, "keyRef");
        let verification_method_key_for_input = field_value_key(ch_id, "verificationMethod");
        let authorized_event_ref_key_for_input = field_value_key(ch_id, "authorizedEventRef");
        let pairing_state_key_for_input = arkret_pairing_state_key(ch_id);
        drop(value_map);
        return rsx! {
            div { class: "channels-cfg__field",
                label { class: "channels-field__label",
                    Link2 { size: 14 }
                    "{display_label}"
                    if is_required {
                        span { class: "channels-field__required", " *" }
                    }
                    if !help_text.is_empty() {
                        HelpTip { text: help_text.clone() }
                    }
                }
                input {
                    r#type: "url",
                    placeholder: "{display_placeholder}",
                    value: "{pairing_link}",
                    disabled: pairing_busy,
                    oninput: move |e| {
                        let input = e.value();
                        let mut values = values.write();
                        values.insert(key_for_input.clone(), input.clone());
                        values.insert(pairing_link_key_for_input.clone(), input);
                        values.remove(&key_ref_key_for_input);
                        values.remove(&verification_method_key_for_input);
                        values.remove(&authorized_event_ref_key_for_input);
                        values.remove(&pairing_state_key_for_input);
                    },
                    class: "channels-field__input",
                }
                if let Some(pairing_code) = pairing_code.as_ref() {
                    {
                        let pairing_code_for_copy = pairing_code.clone();
                        let display_code = format_arkret_pairing_code(pairing_code);
                        rsx! {
                            div { class: "arkret-pairing-code",
                                div { class: "arkret-pairing-code__content",
                                    span { class: "arkret-pairing-code__label",
                                        "Confirm this code in Inkson"
                                    }
                                    strong { class: "arkret-pairing-code__value", "{display_code}" }
                                    span { class: "arkret-pairing-code__hint",
                                        "Make sure Inkson shows the same code before approving."
                                    }
                                    if let Some(expires_at) = pairing_expires_at.as_ref() {
                                        span { class: "arkret-pairing-code__expiry",
                                            "Expires at {expires_at}"
                                        }
                                    }
                                }
                                button {
                                    class: "channels-action-btn arkret-pairing-code__copy",
                                    r#type: "button",
                                    aria_label: "Copy pairing code",
                                    onclick: move |_| {
                                        let text = pairing_code_for_copy.clone();
                                        spawn(async move {
                                            copy_text_to_clipboard(text).await;
                                        });
                                    },
                                    Copy { size: 14 }
                                    "Copy"
                                }
                            }
                        }
                    }
                }
            }
        };
    }
    if ch_id == "arkret" && field.key == "unbind" {
        let status = current_val.trim().to_owned();
        let is_bound = arkret_agent_is_bound(ch_id, &value_map);
        if !is_bound {
            drop(value_map);
            return rsx! {};
        }
        let bootstrap_summary = value_map
            .get(&field_value_key(ch_id, "inksonBootstrap"))
            .and_then(|value| arkret_bootstrap_summary_from_text(value));
        let bound_agent = bootstrap_summary
            .as_ref()
            .map(|summary| summary.0.clone())
            .or_else(|| {
                value_map
                    .get(&field_value_key(ch_id, "verificationMethod"))
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| "Paired Arkret agent".to_owned());
        let arkret_base_url = bootstrap_summary.map(|summary| summary.1);
        let confirm_key = arkret_unbind_confirm_key(ch_id);
        let confirm_unbind = value_map
            .get(&confirm_key)
            .is_some_and(|value| value == "true");
        let saved_channel_id = value_map
            .get(&saved_channel_id_key(ch_id))
            .cloned()
            .unwrap_or_else(|| ch_id.to_owned());
        let key_for_unbind = key.clone();
        let ws_unbind = ws.clone();
        let key_ref_key_for_unbind = field_value_key(ch_id, "keyRef");
        let bootstrap_key_for_unbind = field_value_key(ch_id, "inksonBootstrap");
        let verification_method_key_for_unbind = field_value_key(ch_id, "verificationMethod");
        let authorized_event_ref_key_for_unbind = field_value_key(ch_id, "authorizedEventRef");
        let pairing_link_key_for_unbind = arkret_pairing_link_input_key(ch_id);
        let pairing_state_key_for_unbind = arkret_pairing_state_key(ch_id);
        let runtime_status_key_for_unbind = field_value_key(ch_id, "runtimeKeyRequest");
        let confirm_key_for_cancel = confirm_key.clone();
        let confirm_key_for_start = confirm_key.clone();
        let confirm_key_for_unbind = confirm_key.clone();
        let mut refresh_after_unbind = refresh_tick;
        drop(value_map);
        return rsx! {
            div { class: "channels-cfg__field arkret-connection",
                div { class: "arkret-connection__header",
                    div { class: "arkret-connection__status",
                        CircleCheck { size: 16 }
                        span { "Paired agent" }
                    }
                    span { class: "arkret-connection__badge", "Paired" }
                }
                div { class: "arkret-connection__details",
                    div { class: "arkret-connection__detail",
                        span { class: "arkret-connection__detail-label", "Agent" }
                        span { class: "arkret-connection__detail-value mono", title: "{bound_agent}", "{bound_agent}" }
                    }
                    if let Some(base_url) = arkret_base_url.as_ref() {
                        div { class: "arkret-connection__detail",
                            span { class: "arkret-connection__detail-label", "Arkret server" }
                            span { class: "arkret-connection__detail-value", title: "{base_url}", "{base_url}" }
                        }
                    }
                }
                if !status.is_empty() {
                    div { class: "channels-field__hint", "{status}" }
                }
                div { class: "arkret-connection__management",
                    if confirm_unbind {
                        div { class: "arkret-disconnect-confirm",
                            div { class: "arkret-disconnect-confirm__message",
                                TriangleAlert { size: 16 }
                                div {
                                    strong { "Disconnect this agent?" }
                                    p {
                                        "This revokes its runtime KeyPackages, stops the connection, and removes local pairing state."
                                    }
                                }
                            }
                            div { class: "channels-cfg__row-actions",
                                button {
                                    class: "channels-action-btn",
                                    r#type: "button",
                                    onclick: move |_| {
                                        values.write().remove(&confirm_key_for_cancel);
                                    },
                                    "Cancel"
                                }
                                button {
                                    class: "channels-action-btn channels-action-btn--danger",
                                    r#type: "button",
                                    onclick: move |_| {
                                        let key = key_for_unbind.clone();
                                        let ws = ws_unbind.clone();
                                        let channel = saved_channel_id.clone();
                                        let key_ref_key = key_ref_key_for_unbind.clone();
                                        let bootstrap_key = bootstrap_key_for_unbind.clone();
                                        let verification_method_key =
                                            verification_method_key_for_unbind.clone();
                                        let authorized_event_ref_key =
                                            authorized_event_ref_key_for_unbind.clone();
                                        let pairing_link_key = pairing_link_key_for_unbind.clone();
                                        let pairing_state_key = pairing_state_key_for_unbind.clone();
                                        let runtime_status_key =
                                            runtime_status_key_for_unbind.clone();
                                        let confirm_key = confirm_key_for_unbind.clone();
                                        values.write().insert(
                                            key.clone(),
                                            "Disconnecting current agent…".to_owned(),
                                        );
                                        spawn(async move {
                                            let result = ws
                                                .call::<Value>(
                                                    "channels.arkret.unbind",
                                                    Some(json!({
                                                        "platform": "arkret",
                                                        "channel": channel,
                                                    })),
                                                )
                                                .await;
                                            match result {
                                                Ok(_) => {
                                                    let mut values = values.write();
                                                    values.remove(&key_ref_key);
                                                    values.remove(&bootstrap_key);
                                                    values.remove(&verification_method_key);
                                                    values.remove(&authorized_event_ref_key);
                                                    values.remove(&pairing_link_key);
                                                    values.remove(&pairing_state_key);
                                                    values.remove(&confirm_key);
                                                    values.remove(&key);
                                                    values.insert(
                                                        runtime_status_key,
                                                        "Agent disconnected. Paste a new Inkson pairing link to connect another agent."
                                                            .to_owned(),
                                                    );
                                                    drop(values);
                                                    refresh_after_unbind += 1;
                                                }
                                                Err(error) => {
                                                    values.write().insert(
                                                        key,
                                                        format!("Disconnect failed: {error}"),
                                                    );
                                                }
                                            }
                                        });
                                    },
                                    Unplug { size: 14 }
                                    "Disconnect agent"
                                }
                            }
                        }
                    } else {
                        button {
                            class: "channels-action-btn channels-action-btn--muted",
                            r#type: "button",
                            onclick: move |_| {
                                values
                                    .write()
                                    .insert(confirm_key_for_start.clone(), "true".to_owned());
                            },
                            Unplug { size: 14 }
                            "Disconnect agent…"
                        }
                    }
                }
            }
        };
    }
    if ch_id == "arkret" && field.key == "runtimeKeyRequest" {
        if arkret_agent_is_bound(ch_id, &value_map) {
            drop(value_map);
            return rsx! {};
        }
        let current_result = current_val.trim().to_owned();
        let has_generated_request = current_result.trim_start().starts_with('{');
        let display_status = if has_generated_request {
            "Approval request prepared. Waiting for backend delivery to Inkson.".to_string()
        } else if current_result.is_empty() {
            String::new()
        } else {
            current_result.clone()
        };
        let pairing_state_key = arkret_pairing_state_key(ch_id);
        let pairing_state = value_map
            .get(&pairing_state_key)
            .map(String::as_str)
            .unwrap_or_default()
            .to_owned();
        let retry_save = pairing_state == "save_failed";
        let pairing_busy = matches!(
            pairing_state.as_str(),
            "starting" | "waiting" | "finalizing"
        );
        let can_request_approval =
            retry_save || arkret_runtime_key_request_can_request(ch_id, &value_map);
        let ch_id_for_generate = ch_id.to_owned();
        let fields_for_generate = fields.to_vec();
        let key_for_generate = key.clone();
        let pairing_state_key_for_generate = pairing_state_key.clone();
        let key_ref_key_for_generate = field_value_key(ch_id, "keyRef");
        let bootstrap_key_for_generate = field_value_key(ch_id, "inksonBootstrap");
        let base_url_key_for_generate = field_value_key(ch_id, "baseUrl");
        let verification_method_key_for_generate = field_value_key(ch_id, "verificationMethod");
        let authorized_event_ref_key_for_generate = field_value_key(ch_id, "authorizedEventRef");
        let unbind_status_key_for_generate = field_value_key(ch_id, "unbind");
        let ws_generate = ws.clone();
        let mut refresh_after_pairing = refresh_tick;
        drop(value_map);
        return rsx! {
            div { class: "channels-cfg__field arkret-pairing-action",
                div { class: "channels-cfg__row-actions",
                    button {
                        class: "channels-action-btn channels-action-btn--primary",
                        r#type: "button",
                        disabled: !can_request_approval || pairing_busy,
                        onclick: move |_| {
                            let ch_id = ch_id_for_generate.clone();
                            let fields = fields_for_generate.clone();
                            let key = key_for_generate.clone();
                            let pairing_state_key = pairing_state_key_for_generate.clone();
                            let key_ref_key = key_ref_key_for_generate.clone();
                            let bootstrap_key = bootstrap_key_for_generate.clone();
                            let base_url_key = base_url_key_for_generate.clone();
                            let verification_method_key = verification_method_key_for_generate.clone();
                            let authorized_event_ref_key = authorized_event_ref_key_for_generate.clone();
                            let unbind_status_key = unbind_status_key_for_generate.clone();
                            let ws = ws_generate.clone();
                            let mut snapshot = values.read().clone();
                            if snapshot
                                .get(&pairing_state_key)
                                .is_some_and(|state| state == "save_failed")
                            {
                                spawn(async move {
                                    finalize_arkret_pairing(
                                        &ws,
                                        values,
                                        &ch_id,
                                        &fields,
                                        &key,
                                        &unbind_status_key,
                                        refresh_after_pairing,
                                    )
                                    .await;
                                });
                                return;
                            }
                            values
                                .write()
                                .insert(pairing_state_key.clone(), "starting".to_owned());
                            spawn(async move {
                                let bootstrap_input = snapshot
                                    .get(&bootstrap_key)
                                    .cloned()
                                    .unwrap_or_default();
                                if bootstrap_input.trim().is_empty() {
                                    values.write().insert(
                                        key,
                                        "Pairing input is required.".to_string(),
                                    );
                                    values
                                        .write()
                                        .insert(pairing_state_key, "error".to_owned());
                                    return;
                                }
                                if !bootstrap_input.trim_start().starts_with('{') {
                                    values.write().insert(
                                        key.clone(),
                                        "Resolving Inkson pairing link...".to_string(),
                                    );
                                    let base_url = snapshot
                                        .get(&base_url_key)
                                        .cloned()
                                        .unwrap_or_default();
                                    let result = ws
                                        .call::<serde_json::Value>(
                                            "channels.arkret.resolve_pairing_bootstrap",
                                            Some(json!({
                                                "input": bootstrap_input,
                                                "base_url": base_url,
                                            })),
                                        )
                                        .await;
                                    match result {
                                        Ok(payload) => {
                                            let bootstrap = payload
                                                .get("inkson_bootstrap")
                                                .cloned()
                                                .unwrap_or(serde_json::Value::Null);
                                            let text = serde_json::to_string_pretty(&bootstrap)
                                                .unwrap_or_else(|_| bootstrap.to_string());
                                            let default_verification_method = bootstrap
                                                .get("agent_id")
                                                .and_then(serde_json::Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(|value| format!("{value}#runtime-1"));
                                            {
                                                let mut values = values.write();
                                                values.insert(bootstrap_key.clone(), text.clone());
                                                if let Some(default_verification_method) =
                                                    default_verification_method
                                                {
                                                    values.insert(
                                                        verification_method_key.clone(),
                                                        default_verification_method.clone(),
                                                    );
                                                    values.remove(&authorized_event_ref_key);
                                                    snapshot.insert(
                                                        verification_method_key.clone(),
                                                        default_verification_method,
                                                    );
                                                    snapshot.remove(&authorized_event_ref_key);
                                                }
                                            }
                                            snapshot.insert(bootstrap_key.clone(), text);
                                        }
                                        Err(err) => {
                                            values.write().insert(
                                                key,
                                                format!("Pairing link resolve failed: {err}"),
                                            );
                                            values
                                                .write()
                                                .insert(pairing_state_key, "error".to_owned());
                                            return;
                                        }
                                    }
                                }
                                let key_ref_needs_generation = snapshot
                                    .get(&key_ref_key)
                                    .map(|value| {
                                        serde_json::from_str::<Value>(value.trim())
                                            .ok()
                                            .and_then(|value| {
                                                value
                                                    .get("kind")
                                                    .and_then(Value::as_str)
                                                    .map(str::to_owned)
                                            })
                                            .is_none_or(|kind| kind != "keyring")
                                    })
                                    .unwrap_or(true);
                                if key_ref_needs_generation {
                                    values.write().insert(
                                        key.clone(),
                                        "Generating local runtime key...".to_string(),
                                    );
                                    let params =
                                        arkret_runtime_key_ref_generation_params(&ch_id, &snapshot);
                                    let result = ws
                                        .call::<serde_json::Value>(
                                            "channels.arkret.generate_runtime_key_ref",
                                            Some(params),
                                        )
                                        .await;
                                    match result {
                                        Ok(payload) => {
                                            let key_ref = payload
                                                .get("key_ref")
                                                .cloned()
                                                .unwrap_or(serde_json::Value::Null);
                                            let text = serde_json::to_string_pretty(&key_ref)
                                                .unwrap_or_else(|_| key_ref.to_string());
                                            values.write().insert(key_ref_key.clone(), text.clone());
                                            snapshot.insert(key_ref_key.clone(), text);
                                        }
                                        Err(err) => {
                                            values.write().insert(
                                                key,
                                                format!("Local runtime key generation failed: {err}"),
                                            );
                                            values
                                                .write()
                                                .insert(pairing_state_key, "error".to_owned());
                                            return;
                                        }
                                    }
                                }
                                let patch = match build_channel_patch(&ch_id, &fields, &snapshot) {
                                    Ok(patch) => patch,
                                    Err(err) => {
                                        values.write().insert(
                                            key,
                                            format!("Runtime key request generation failed: {err}"),
                                        );
                                        values
                                            .write()
                                            .insert(pairing_state_key, "error".to_owned());
                                        return;
                                    }
                                };
                                let result = ws
                                    .call::<serde_json::Value>(
                                        "channels.arkret.runtime_key_request",
                                    Some(json!({
                                        "platform": "arkret",
                                        "config": patch.clone(),
                                    })),
                                )
                                .await;
                                match result {
                                    Ok(payload) => {
                                        let message = payload
                                            .get("message")
                                            .and_then(serde_json::Value::as_str)
                                            .unwrap_or("Approval request sent to Inkson");
                                        let pairing_code = snapshot
                                            .get(&bootstrap_key)
                                            .and_then(|text| {
                                                arkret_pairing_code_from_bootstrap_text(text)
                                            });
                                        values.write().insert(
                                            key.clone(),
                                            format!(
                                                "{message}. {}",
                                                arkret_waiting_for_approval_status(
                                                    pairing_code.as_deref(),
                                                ),
                                            ),
                                        );
                                        values
                                            .write()
                                            .insert(pairing_state_key.clone(), "waiting".to_owned());
                                        arkret_poll_runtime_key_approval(
                                            ws,
                                            values,
                                            key,
                                            authorized_event_ref_key,
                                            patch,
                                            pairing_code,
                                            ch_id,
                                            fields,
                                            unbind_status_key,
                                            refresh_after_pairing,
                                        )
                                        .await;
                                    }
                                    Err(err) => {
                                        values.write().insert(
                                            key,
                                            format!("Runtime key request generation failed: {err}"),
                                        );
                                        values
                                            .write()
                                            .insert(pairing_state_key, "error".to_owned());
                                    }
                                }
                            });
                        },
                        if pairing_busy {
                            LoaderCircle { size: 14, class: "channels-spin" }
                        }
                        if retry_save {
                            "Retry saving"
                        } else if pairing_state == "waiting" {
                            "Waiting for Inkson…"
                        } else if pairing_state == "finalizing" {
                            "Saving connection…"
                        } else {
                            "Start pairing"
                        }
                    }
                }
                if !display_status.is_empty() {
                    div {
                        class: if pairing_state == "save_failed" || pairing_state == "error" {
                            "arkret-pairing-status arkret-pairing-status--error"
                        } else {
                            "arkret-pairing-status"
                        },
                        aria_live: "polite",
                        "{display_status}"
                    }
                }
            }
        };
    }
    if ch_id == "arkret" && field.key == "authorizationResult" {
        let event_ref_key = field_value_key(ch_id, "authorizedEventRef");
        let authorized_event_ref = value_map.get(&event_ref_key).cloned().unwrap_or_default();
        let key_for_input = key.clone();
        let event_ref_key_for_input = event_ref_key.clone();
        drop(value_map);
        return rsx! {
            div { class: "channels-cfg__field",
                label { class: "channels-field__label",
                    "{display_label}"
                    if !help_text.is_empty() {
                        HelpTip { text: help_text.clone() }
                    }
                }
                textarea {
                    placeholder: "{display_placeholder}",
                    value: "{current_val}",
                    oninput: move |e| {
                        let raw = e.value();
                        let event_ref = extract_arkret_authorized_event_ref(&raw);
                        let mut values = values.write();
                        values.insert(key_for_input.clone(), raw.clone());
                        if let Some(event_ref) = event_ref {
                            values.insert(event_ref_key_for_input.clone(), event_ref);
                        } else if raw.trim().is_empty() {
                            values.remove(&event_ref_key_for_input);
                        }
                    },
                    class: "channels-field__input channels-cfg__textarea",
                    rows: "4",
                }
                if !authorized_event_ref.trim().is_empty() {
                    div { class: "channels-field__hint",
                        "Authorization recorded: "
                        span { class: "mono", "{authorized_event_ref}" }
                    }
                }
            }
        };
    }
    if ch_id == "arkret" && field.key == "keyRef" {
        let ch_id_for_generate = ch_id.to_owned();
        let key_for_generate = key.clone();
        let status_key_for_generate = field_value_key(ch_id, "runtimeKeyRequest");
        let ws_generate = ws.clone();
        let key_for_copy = key.clone();
        let key_input = key.clone();
        let current_result = current_val.trim().to_owned();
        drop(value_map);
        return rsx! {
            div { class: "channels-cfg__field",
                label { class: "channels-field__label",
                    "{display_label}"
                    if is_required {
                        span { class: "channels-field__required", " *" }
                    }
                    span { class: "channels-field__secret-badge", "secret" }
                    if !help_text.is_empty() {
                        HelpTip { text: help_text.clone() }
                    }
                }
                textarea {
                    placeholder: "{display_placeholder}",
                    value: "{current_val}",
                    oninput: move |e| { values.write().insert(key_input.clone(), e.value()); },
                    class: "channels-field__input channels-cfg__textarea",
                    rows: "6",
                }
                div { class: "channels-cfg__row-actions",
                    button {
                        class: "channels-action-btn channels-action-btn--primary",
                        r#type: "button",
                        onclick: move |_| {
                            let ch_id = ch_id_for_generate.clone();
                            let key = key_for_generate.clone();
                            let status_key = status_key_for_generate.clone();
                            let ws = ws_generate.clone();
                            let snapshot = values.read().clone();
                            let params = arkret_runtime_key_ref_generation_params(&ch_id, &snapshot);
                            spawn(async move {
                                let result = ws
                                    .call::<serde_json::Value>(
                                        "channels.arkret.generate_runtime_key_ref",
                                        Some(params),
                                    )
                                    .await;
                                match result {
                                    Ok(payload) => {
                                        let key_ref = payload
                                            .get("key_ref")
                                            .cloned()
                                            .unwrap_or(serde_json::Value::Null);
                                        let text = serde_json::to_string_pretty(&key_ref)
                                            .unwrap_or_else(|_| key_ref.to_string());
                                        values.write().insert(key, text);
                                    }
                                    Err(err) => {
                                        values.write().insert(
                                            status_key,
                                            format!("Runtime key generation failed: {err}"),
                                        );
                                    }
                                }
                            });
                        },
                        "Generate File Key"
                    }
                    button {
                        class: "channels-action-btn",
                        r#type: "button",
                        disabled: current_result.is_empty(),
                        onclick: move |_| {
                            let text = values
                                .read()
                                .get(&key_for_copy)
                                .cloned()
                                .unwrap_or_default();
                            if !text.trim().is_empty() {
                                spawn(async move {
                                    copy_text_to_clipboard(text).await;
                                });
                            }
                        },
                        "Copy"
                    }
                }
            }
        };
    }
    drop(value_map);
    let key_input = key.clone();
    let key_reveal = key.clone();
    let is_revealed = revealed.read().contains(&key);

    match &field.field_type {
        FieldType::Text => {
            rsx! {
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label",
                        "{display_label}"
                        if is_required {
                            span { class: "channels-field__required", " *" }
                        }
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.clone() }
                        }
                    }
                    input {
                        r#type: "text",
                        placeholder: "{display_placeholder}",
                        value: "{current_val}",
                        oninput: move |e| { values.write().insert(key_input.clone(), e.value()); },
                        class: "channels-field__input",
                    }
                }
            }
        }
        FieldType::Password => {
            let input_type = if is_revealed { "text" } else { "password" };
            let toggle_label = if is_revealed { "Hide" } else { "Show" };
            rsx! {
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label",
                        "{display_label}"
                        if is_required {
                            span { class: "channels-field__required", " *" }
                        }
                        span { class: "channels-field__secret-badge", "secret" }
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.clone() }
                        }
                    }
                    div { class: "channels-cfg__password-wrap",
                        input {
                            r#type: "{input_type}",
                            placeholder: "{display_placeholder}",
                            value: "{current_val}",
                            oninput: move |e| { values.write().insert(key_input.clone(), e.value()); },
                            class: "channels-field__input channels-cfg__password-input",
                        }
                        button {
                            onclick: move |_| {
                                let mut set = revealed.write();
                                if set.contains(&key_reveal) {
                                    set.remove(&key_reveal);
                                } else {
                                    set.insert(key_reveal.clone());
                                }
                            },
                            class: "channels-cfg__reveal-btn",
                            r#type: "button",
                            "{toggle_label}"
                        }
                    }
                }
            }
        }
        FieldType::Number => {
            rsx! {
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label",
                        "{display_label}"
                        if is_required {
                            span { class: "channels-field__required", " *" }
                        }
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.clone() }
                        }
                    }
                    input {
                        r#type: "number",
                        placeholder: "{display_placeholder}",
                        value: "{current_val}",
                        oninput: move |e| { values.write().insert(key_input.clone(), e.value()); },
                        class: "channels-field__input",
                    }
                }
            }
        }
        FieldType::Textarea => {
            let secret_badge = field.secret;
            rsx! {
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label",
                        "{display_label}"
                        if is_required {
                            span { class: "channels-field__required", " *" }
                        }
                        if secret_badge {
                            span { class: "channels-field__secret-badge", "secret" }
                        }
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.clone() }
                        }
                    }
                    textarea {
                        placeholder: "{display_placeholder}",
                        value: "{current_val}",
                        oninput: move |e| { values.write().insert(key_input.clone(), e.value()); },
                        class: "channels-field__input channels-cfg__textarea",
                        rows: "6",
                    }
                }
            }
        }
        FieldType::Toggle => {
            let is_on = current_val == "true";
            let key_toggle = key.clone();
            rsx! {
                div { class: "channels-cfg__field channels-cfg__toggle-row",
                    label { class: "channels-field__label",
                        "{display_label}"
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.clone() }
                        }
                    }
                    button {
                        onclick: move |_| {
                            let new_val = if is_on { "false" } else { "true" };
                            values.write().insert(key_toggle.clone(), new_val.into());
                        },
                        class: if is_on { "channels-cfg__toggle-btn channels-cfg__toggle-btn--on" } else { "channels-cfg__toggle-btn" },
                        r#type: "button",
                        div { class: "channels-cfg__toggle-track",
                            div { class: "channels-cfg__toggle-thumb" }
                        }
                    }
                }
            }
        }
        FieldType::Select(options) => {
            rsx! {
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label",
                        "{display_label}"
                        if is_required {
                            span { class: "channels-field__required", " *" }
                        }
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.clone() }
                        }
                    }
                    select {
                        value: "{current_val}",
                        onchange: move |e| { values.write().insert(key_input.clone(), e.value()); },
                        class: "channels-field__input channels-cfg__select",
                        for opt in options.iter() {
                            {
                                let display = capitalize_first(opt);
                                rsx! {
                                    option {
                                        value: "{opt}",
                                        selected: current_val == *opt,
                                        "{display}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn arkret_agent_is_bound(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> bool {
    let has_authorization = values
        .get(&field_value_key(channel_id, "authorizedEventRef"))
        .is_some_and(|value| !value.trim().is_empty());
    let finalization_incomplete = values
        .get(&arkret_pairing_state_key(channel_id))
        .is_some_and(|state| matches!(state.as_str(), "finalizing" | "save_failed"));
    has_authorization && !finalization_incomplete
}

fn arkret_bootstrap_summary_from_text(input: &str) -> Option<(String, String)> {
    let bootstrap = serde_json::from_str::<Value>(input.trim())
        .ok()
        .and_then(|value| parse_arkret_agent_pairing_bootstrap(value).ok())?;
    Some((bootstrap.agent_id.to_string(), bootstrap.arkret_base_url))
}

fn format_arkret_pairing_code(code: &str) -> String {
    let characters = code.trim().chars().collect::<Vec<_>>();
    if characters.len() <= 4 {
        return characters.into_iter().collect();
    }
    characters
        .chunks(4)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

async fn finalize_arkret_pairing(
    ws: &WsRpc,
    mut values: Signal<std::collections::HashMap<String, String>>,
    channel_id: &str,
    fields: &[ConfigField],
    status_key: &str,
    connected_status_key: &str,
    mut refresh_tick: Signal<u32>,
) {
    let pairing_state_key = arkret_pairing_state_key(channel_id);
    {
        let mut values = values.write();
        values.insert(pairing_state_key.clone(), "finalizing".to_owned());
        values.insert(
            status_key.to_owned(),
            "Approved by Inkson. Finalizing and saving the Savfox connection…".to_owned(),
        );
    }
    let snapshot = values.read().clone();
    match persist_channel_form(ws, channel_id, fields, &snapshot).await {
        Ok(payload) => {
            let saved_channel_id = payload
                .get("config")
                .and_then(|config| config.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let mut values = values.write();
            values.remove(&pairing_state_key);
            values.insert(
                connected_status_key.to_owned(),
                "Agent paired and channel saved.".to_owned(),
            );
            if let Some(saved_channel_id) = saved_channel_id {
                values.insert(saved_channel_id_key(channel_id), saved_channel_id);
            }
            drop(values);
            refresh_tick += 1;
        }
        Err(error) => {
            let mut values = values.write();
            values.insert(pairing_state_key, "save_failed".to_owned());
            values.insert(
                status_key.to_owned(),
                format!(
                    "Approved in Inkson, but Savfox could not save the connection: {error}. Retry saving to finish."
                ),
            );
        }
    }
}

fn arkret_runtime_key_request_can_request(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> bool {
    values
        .get(&field_value_key(channel_id, "inksonBootstrap"))
        .map(|value| value.trim())
        .is_some_and(|value| !value.is_empty())
}

fn arkret_pairing_code_from_bootstrap_text(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() || !input.starts_with('{') {
        return None;
    }
    serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|value| parse_arkret_agent_pairing_bootstrap(value).ok())
        .map(|bootstrap| bootstrap.pairing_code)
        .filter(|value| !value.trim().is_empty())
}

fn arkret_pairing_expiry_from_bootstrap_text(input: &str) -> Option<String> {
    serde_json::from_str::<Value>(input.trim())
        .ok()
        .and_then(|value| {
            value
                .get("pairing_expires_at")
                .or_else(|| value.get("pairingExpiresAt"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
}

/// Waiting-state status line for the pairing approval poll. Repeats the
/// pairing code next to the spinner text so the user can compare it against
/// the Inkson approval prompt without scrolling back to the bootstrap field.
fn arkret_waiting_for_approval_status(pairing_code: Option<&str>) -> String {
    match pairing_code {
        Some(code) => format!(
            "Waiting for Inkson approval... Compare pairing code {code} with the Inkson prompt."
        ),
        None => "Waiting for Inkson approval...".to_owned(),
    }
}

static ARKRET_APPROVAL_POLL_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Poll the Arkret server for the Inkson controller decision after a
/// `Request approval` submission and drive the pairing status line to a
/// terminal state (approved / expired / paired-by-another-runtime / timeout).
/// A newer `Request approval` click supersedes any older poll loop through
/// the generation counter.
async fn arkret_poll_runtime_key_approval(
    ws: WsRpc,
    mut values: Signal<std::collections::HashMap<String, String>>,
    status_key: String,
    authorized_event_ref_key: String,
    config_patch: Value,
    pairing_code: Option<String>,
    channel_id: String,
    fields: Vec<ConfigField>,
    connected_status_key: String,
    refresh_tick: Signal<u32>,
) {
    use std::sync::atomic::Ordering;

    let generation = ARKRET_APPROVAL_POLL_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    // Poll every 3 s for up to ~10 minutes; the pairing itself expires
    // server-side, so a stale loop can only ever see a terminal state.
    for _ in 0..200 {
        crate::utils::sleep_ms(3000).await;
        if ARKRET_APPROVAL_POLL_GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        let response = ws
            .call::<serde_json::Value>(
                "channels.arkret.runtime_key_request_status",
                Some(json!({
                    "platform": "arkret",
                    "config": config_patch.clone(),
                })),
            )
            .await;
        if ARKRET_APPROVAL_POLL_GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        let payload = match response {
            Ok(payload) => payload,
            Err(err) => {
                values.write().insert(
                    status_key.clone(),
                    format!("Approval status check failed: {err}. Retrying..."),
                );
                continue;
            }
        };
        let approved = payload
            .get("approved")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let paired_by_other_runtime = payload
            .get("paired_by_other_runtime")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let status = payload
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        // Runtime readiness axis (key-management.md §3.6.1): pairing_expired now
        // lives on runtime_state, not the lifecycle status.
        let runtime_state = payload
            .get("runtime_state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if approved {
            let event_ref = payload
                .get("authorized_event_ref")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            let Some(event_ref) = event_ref else {
                let mut values = values.write();
                values.insert(arkret_pairing_state_key(&channel_id), "error".to_owned());
                values.insert(
                    status_key,
                    "Inkson approved the request but did not return an authorization reference."
                        .to_owned(),
                );
                return;
            };
            values.write().insert(authorized_event_ref_key, event_ref);
            finalize_arkret_pairing(
                &ws,
                values,
                &channel_id,
                &fields,
                &status_key,
                &connected_status_key,
                refresh_tick,
            )
            .await;
            return;
        }
        if paired_by_other_runtime {
            let mut values = values.write();
            values.insert(arkret_pairing_state_key(&channel_id), "error".to_owned());
            values.insert(
                status_key,
                "Pairing was completed by a different runtime key. Create a new pairing in Inkson and try again."
                    .to_owned(),
            );
            return;
        }
        if status == "deactivated" {
            let mut values = values.write();
            values.insert(arkret_pairing_state_key(&channel_id), "error".to_owned());
            values.insert(
                status_key,
                "Agent was deactivated before pairing completed.".to_owned(),
            );
            return;
        }
        match runtime_state.as_str() {
            "pairing_expired" => {
                let mut values = values.write();
                values.insert(arkret_pairing_state_key(&channel_id), "error".to_owned());
                values.insert(
                    status_key,
                    "Pairing request expired before approval. Create a new pairing in Inkson and try again."
                        .to_owned(),
                );
                return;
            }
            _ => {
                values.write().insert(
                    status_key.clone(),
                    arkret_waiting_for_approval_status(pairing_code.as_deref()),
                );
            }
        }
    }
    let mut values = values.write();
    values.insert(arkret_pairing_state_key(&channel_id), "error".to_owned());
    values.insert(
        status_key,
        "Timed out waiting for Inkson approval. Start pairing again to retry.".to_owned(),
    );
}

fn arkret_runtime_key_ref_generation_params(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> Value {
    let bootstrap = values
        .get(&field_value_key(channel_id, "inksonBootstrap"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .and_then(|value| parse_arkret_agent_pairing_bootstrap(value).ok());
    let bootstrap_principal = bootstrap
        .as_ref()
        .map(|bootstrap| bootstrap.agent_id.to_string());
    let principal_id = bootstrap_principal.or_else(|| {
        values
            .get(&field_value_key(channel_id, "principalId"))
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    });
    let verification_method = bootstrap.as_ref().map(|bootstrap| {
        let agent_id = bootstrap.agent_id.to_string();
        values
            .get(&field_value_key(channel_id, "verificationMethod"))
            .map(|value| value.trim())
            .filter(|value| arkret_verification_method_matches_agent(value, &agent_id))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{agent_id}#runtime-1"))
    });

    let mut params = serde_json::Map::new();
    params.insert("platform".to_owned(), json!("arkret"));
    if let Some(principal_id) = principal_id {
        params.insert("agent_id".to_owned(), json!(principal_id));
    }
    if let Some(verification_method) = verification_method {
        params.insert("verification_method".to_owned(), json!(verification_method));
    }
    if let Some(key_ref) = values
        .get(&field_value_key(channel_id, "keyRef"))
        .and_then(|value| serde_json::from_str::<Value>(value.trim()).ok())
        .filter(|value| {
            matches!(
                value.get("kind").and_then(Value::as_str),
                Some("file" | "env")
            )
        })
    {
        params.insert("key_ref".to_owned(), key_ref);
    }
    Value::Object(params)
}

fn render_router_fields(
    ch_id: &str,
    mut values: Signal<std::collections::HashMap<String, String>>,
) -> Element {
    let mode_key = router_mode_key(ch_id);
    let agent_id_key = router_agent_id_key(ch_id);
    let default_agent_id_key = router_default_agent_id_key(ch_id);
    let rules_key = router_rules_key(ch_id);

    let router_mode = {
        let value_map = values.read();
        current_router_mode(ch_id, &value_map)
    };
    let direct_agent_id = values
        .read()
        .get(&agent_id_key)
        .cloned()
        .unwrap_or_default();
    let default_agent_id = values
        .read()
        .get(&default_agent_id_key)
        .cloned()
        .unwrap_or_default();
    let rules_json = values.read().get(&rules_key).cloned().unwrap_or_default();
    let rules_placeholder = "[\n  {\n    \"channel\": \"*\",\n    \"sender\": \"@user:example.com\",\n    \"agent_id\": \"support\"\n  }\n]";

    rsx! {
        div { class: "channels-cfg__section",
            div { class: "channels-cfg__section-title", "Routing" }
            div { class: "channels-field__hint",
                "Route this channel to one agent or fan messages out with per-user rules."
            }

            div { class: "channels-cfg__field",
                label { class: "channels-field__label",
                    "Router"
                    HelpTip { text: "Choose how incoming messages are routed to agents".to_string() }
                }
                select {
                    value: "{router_mode}",
                    onchange: move |e| {
                        values.write().insert(mode_key.clone(), e.value());
                    },
                    class: "channels-field__input channels-cfg__select",
                    option { value: "", selected: router_mode.is_empty(), "Use default fallback" }
                    option { value: "agent_id", selected: router_mode == "agent_id", "Single agent" }
                    option { value: "route_rules", selected: router_mode == "route_rules", "Route rules" }
                }
            }

            if router_mode == "agent_id" {
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label",
                        "Agent ID"
                        span { class: "channels-field__required", " *" }
                        HelpTip { text: "The agent ID that all messages on this channel are routed to".to_string() }
                    }
                    input {
                        r#type: "text",
                        placeholder: "default",
                        value: "{direct_agent_id}",
                        oninput: move |e| {
                            values.write().insert(agent_id_key.clone(), e.value());
                        },
                        class: "channels-field__input",
                    }
                }
            }

            if router_mode == "route_rules" {
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label",
                        "Default Agent ID"
                        HelpTip { text: "Fallback agent used when no routing rule matches".to_string() }
                    }
                    input {
                        r#type: "text",
                        placeholder: "default",
                        value: "{default_agent_id}",
                        oninput: move |e| {
                            values
                                .write()
                                .insert(default_agent_id_key.clone(), e.value());
                        },
                        class: "channels-field__input",
                    }
                }
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label",
                        "Rules JSON"
                        HelpTip { text: "JSON array of routing rules matching senders to agents".to_string() }
                    }
                    textarea {
                        placeholder: "{rules_placeholder}",
                        value: "{rules_json}",
                        oninput: move |e| {
                            values.write().insert(rules_key.clone(), e.value());
                        },
                        class: "channels-field__input channels-cfg__textarea",
                        rows: "8",
                    }
                    div { class: "channels-field__hint",
                        "Use a JSON array of rules, or a full router object with `rules`."
                    }
                }
            }
        }
    }
}

fn render_policy_fields(
    ch_id: &str,
    mut values: Signal<std::collections::HashMap<String, String>>,
) -> Element {
    let dm_mode_key = dm_policy_mode_key(ch_id);
    let dm_list_key = dm_policy_list_key(ch_id);
    let grp_mode_key = group_policy_mode_key(ch_id);
    let grp_list_key = group_policy_list_key(ch_id);

    let dm_mode = values.read().get(&dm_mode_key).cloned().unwrap_or_default();
    let dm_list = values.read().get(&dm_list_key).cloned().unwrap_or_default();
    let grp_mode = values
        .read()
        .get(&grp_mode_key)
        .cloned()
        .unwrap_or_default();
    let grp_list = values
        .read()
        .get(&grp_list_key)
        .cloned()
        .unwrap_or_default();

    let dm_list_label = if dm_mode == "denylist" {
        "Deny List (comma-separated)"
    } else {
        "Allow List (comma-separated)"
    };
    let grp_list_label = if grp_mode == "denylist" {
        "Deny List (comma-separated)"
    } else {
        "Allow List (comma-separated)"
    };

    rsx! {
        div { class: "channels-cfg__section",
            div { class: "channels-cfg__section-title", "Access Policies" }
            div { class: "channels-field__hint",
                "Control who can send DMs and which groups the bot responds in."
            }

            // DM Policy
            div { class: "channels-cfg__field",
                label { class: "channels-field__label",
                    "DM Policy"
                    HelpTip { text: "Controls who can send direct messages to the bot".to_string() }
                }
                select {
                    value: "{dm_mode}",
                    onchange: move |e| {
                        values.write().insert(dm_mode_key.clone(), e.value());
                    },
                    class: "channels-field__input channels-cfg__select",
                    option { value: "", selected: dm_mode.is_empty(), "Open (default)" }
                    option { value: "open", selected: dm_mode == "open", "Open" }
                    option { value: "allowlist", selected: dm_mode == "allowlist", "Allowlist" }
                    option { value: "denylist", selected: dm_mode == "denylist", "Denylist" }
                    option { value: "disabled", selected: dm_mode == "disabled", "Disabled" }
                }
            }

            if dm_mode == "allowlist" || dm_mode == "denylist" {
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label", "{dm_list_label}" }
                    textarea {
                        placeholder: "user-id-1, user-id-2, @user:server.org",
                        value: "{dm_list}",
                        oninput: move |e| {
                            values.write().insert(dm_list_key.clone(), e.value());
                        },
                        class: "channels-field__input channels-cfg__textarea",
                        rows: "3",
                    }
                }
            }

            // Group Policy
            div { class: "channels-cfg__field",
                label { class: "channels-field__label",
                    "Group Policy"
                    HelpTip { text: "Controls which groups or rooms the bot will respond in".to_string() }
                }
                select {
                    value: "{grp_mode}",
                    onchange: move |e| {
                        values.write().insert(grp_mode_key.clone(), e.value());
                    },
                    class: "channels-field__input channels-cfg__select",
                    option { value: "", selected: grp_mode.is_empty(), "Open (default)" }
                    option { value: "open", selected: grp_mode == "open", "Open" }
                    option { value: "allowlist", selected: grp_mode == "allowlist", "Allowlist" }
                    option { value: "denylist", selected: grp_mode == "denylist", "Denylist" }
                    option { value: "disabled", selected: grp_mode == "disabled", "Disabled" }
                }
            }

            if grp_mode == "allowlist" || grp_mode == "denylist" {
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label", "{grp_list_label}" }
                    textarea {
                        placeholder: "group-id-1, !room:server.org, C12345678",
                        value: "{grp_list}",
                        oninput: move |e| {
                            values.write().insert(grp_list_key.clone(), e.value());
                        },
                        class: "channels-field__input channels-cfg__textarea",
                        rows: "3",
                    }
                }
            }
        }
    }
}

fn render_add_modal(
    channel_types: &[ChannelTypeInfo],
    mut selected_channel: Signal<Option<String>>,
    mut config_values: Signal<std::collections::HashMap<String, String>>,
    mut saving: Signal<bool>,
    mut save_msg: Signal<Option<String>>,
    ws: WsRpc,
    mut show_add_modal: Signal<bool>,
    mut refresh_tick: Signal<u32>,
    mut add_channel_search: Signal<String>,
    mut channel_name: Signal<String>,
    mut revealed: Signal<std::collections::HashSet<String>>,
) -> Element {
    let selected = selected_channel();
    let selected_type = channel_types
        .iter()
        .find(|t| Some(&t.id) == selected.as_ref());

    let search_query = add_channel_search().trim().to_ascii_lowercase();
    let popular_ids: &[&str] = &[
        "discord", "telegram", "slack", "whatsapp", "signal", "matrix", "arkret",
    ];
    let filtered_types: Vec<&ChannelTypeInfo> = if search_query.is_empty() {
        channel_types.iter().collect()
    } else {
        channel_types
            .iter()
            .filter(|ch| {
                let haystack = format!(
                    "{} {} {}",
                    ch.id.to_ascii_lowercase(),
                    ch.name.to_ascii_lowercase(),
                    ch.description.to_ascii_lowercase()
                );
                haystack.contains(&search_query)
            })
            .collect()
    };
    let popular_types: Vec<&&ChannelTypeInfo> = filtered_types
        .iter()
        .filter(|ch| popular_ids.contains(&ch.id.as_str()))
        .collect();
    let other_types: Vec<&&ChannelTypeInfo> = filtered_types
        .iter()
        .filter(|ch| !popular_ids.contains(&ch.id.as_str()))
        .collect();

    rsx! {
        div {
            class: "channels-modal-backdrop",
            onmousedown: move |_| {
                show_add_modal.set(false);
                add_channel_search.set(String::new());
                selected_channel.set(None);
                config_values.write().clear();
                revealed.write().clear();
                channel_name.set(String::new());
                save_msg.set(None);
            },
            div {
                class: "channels-modal channels-modal--wide",
                onmousedown: |e| e.stop_propagation(),
                div { class: "channels-modal__header",
                    h3 { class: "channels-modal__title",
                        if selected_type.is_some() {
                            "Configure {selected_type.unwrap().name}"
                        } else {
                            "Add Channel"
                        }
                    }
                    button {
                        onclick: move |_| {
                            show_add_modal.set(false);
                            add_channel_search.set(String::new());
                            selected_channel.set(None);
                            config_values.write().clear();
                            revealed.write().clear();
                            channel_name.set(String::new());
                            save_msg.set(None);
                        },
                        class: "channels-modal__close",
                        "x"
                    }
                }

                if let Some(ref msg) = save_msg() {
                    div { class: "channels-modal__alert channels-modal__alert--success",
                        "{msg}"
                    }
                }

                if let Some(ch_type) = selected_type {
                    // Configuration form
                    div { class: "channels-modal__form",
                        {
                            let current_name = channel_name();
                            let id_preview = auto_channel_id(&ch_type.id, &current_name);
                            let name_key = channel_name_key(&ch_type.id);
                            rsx! {
                                div { class: "channels-field",
                                    label { class: "channels-field__label", "Name" }
                                    input {
                                        r#type: "text",
                                        placeholder: "My Channel",
                                        value: "{current_name}",
                                        oninput: move |e| {
                                            let name = e.value();
                                            channel_name.set(name.clone());
                                            config_values.write().insert(name_key.clone(), name);
                                        },
                                        class: "channels-field__input",
                                    }
                                    div { class: "channels-field__hint",
                                        "Channel ID: {id_preview}.json"
                                    }
                                }
                            }
                        }
                        { render_config_fields(
                            &ch_type.id,
                            &ch_type.config_fields,
                            config_values,
                            revealed,
                            ws.clone(),
                            refresh_tick,
                        ) }
                        { render_router_fields(&ch_type.id, config_values) }
                        { render_policy_fields(&ch_type.id, config_values) }

                        div { class: "channels-modal__form-actions",
                            button {
                                onclick: move |_| {
                                    selected_channel.set(None);
                                    config_values.write().clear();
                                    revealed.write().clear();
                                    channel_name.set(String::new());
                                },
                                class: "channels-action-btn",
                                "Back"
                            }
                            {
                                let (arkret_agent_mode, arkret_bound) = {
                                    let values = config_values.read();
                                    (
                                        ch_type.id == "arkret"
                                            && current_arkret_mode(&ch_type.id, &values) == "agent",
                                        arkret_agent_is_bound(&ch_type.id, &values),
                                    )
                                };
                                if arkret_agent_mode && arkret_bound {
                                    rsx! {
                                        button {
                                            onclick: move |_| {
                                                show_add_modal.set(false);
                                                add_channel_search.set(String::new());
                                                selected_channel.set(None);
                                                config_values.write().clear();
                                                revealed.write().clear();
                                                channel_name.set(String::new());
                                                save_msg.set(None);
                                                refresh_tick += 1;
                                            },
                                            class: "channels-action-btn channels-action-btn--primary",
                                            "Done"
                                        }
                                    }
                                } else if arkret_agent_mode {
                                    rsx! {}
                                } else {
                                    rsx! {
                                        button {
                                            onclick: {
                                                let ws = ws.clone();
                                                let ch_id = ch_type.id.clone();
                                                let fields = ch_type.config_fields.clone();
                                                move |_| {
                                                    let ws = ws.clone();
                                                    let ch_id = ch_id.clone();
                                                    let fields = fields.clone();
                                                    let name = channel_name().trim().to_string();
                                                    if name.is_empty() {
                                                        save_msg.set(Some("Name is required.".to_string()));
                                                        return;
                                                    }
                                                    let config = config_values.read();
                                                    let patch = match build_channel_patch(&ch_id, &fields, &config) {
                                                        Ok(patch) => patch,
                                                        Err(err) => {
                                                            save_msg.set(Some(err));
                                                            return;
                                                        }
                                                    };
                                                    let router = match build_router_value(&ch_id, &config) {
                                                        Ok(router) => router,
                                                        Err(err) => {
                                                            save_msg.set(Some(err));
                                                            return;
                                                        }
                                                    };
                                                    let dm_policy = build_dm_policy(&ch_id, &config);
                                                    let group_policy = build_group_policy(&ch_id, &config);
                                                    saving.set(true);
                                                    spawn(async move {
                                                        let params = json!({
                                                            "channel": ch_id,
                                                            "name": name,
                                                            "config": patch,
                                                            "router": router,
                                                            "dm_policy": dm_policy,
                                                            "group_policy": group_policy,
                                                        });
                                                        let result = ws.call::<serde_json::Value>(
                                                            "channels.config.save",
                                                            Some(params),
                                                        ).await;
                                                        saving.set(false);
                                                        match result {
                                                            Ok(_) => {
                                                                show_add_modal.set(false);
                                                                add_channel_search.set(String::new());
                                                                selected_channel.set(None);
                                                                config_values.write().clear();
                                                                revealed.write().clear();
                                                                channel_name.set(String::new());
                                                                save_msg.set(None);
                                                                refresh_tick += 1;
                                                            }
                                                            Err(e) => {
                                                                save_msg.set(Some(format!(
                                                                    "Failed to save: {}",
                                                                    e
                                                                )));
                                                            }
                                                        }
                                                    });
                                                }
                                            },
                                            disabled: saving(),
                                            class: "channels-action-btn channels-action-btn--primary",
                                            if saving() { "Saving..." } else { "Save" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // Channel type selection grid
                    div { class: "channels-picker-search",
                        input {
                            r#type: "text",
                            class: "channels-picker-search__input",
                            placeholder: "Filter platforms by name...",
                            value: "{add_channel_search}",
                            oninput: move |e| add_channel_search.set(e.value()),
                        }
                    }

                    if !popular_types.is_empty() {
                        div { class: "channels-picker-section",
                            div { class: "channels-picker-section__label", "Popular" }
                            div { class: "channels-picker",
                                for ch_type in popular_types.iter() {
                                    {
                                        let ch_id = ch_type.id.clone();
                                        let ch_name = ch_type.name.clone();
                                        rsx! {
                                            button {
                                                key: "{ch_type.id}",
                                                onclick: move |_| {
                                                    selected_channel.set(Some(ch_id.clone()));
                                                    channel_name.set(ch_name.clone());
                                                    config_values.write().clear();
                                                    revealed.write().clear();
                                                    save_msg.set(None);
                                                    add_channel_search.set(String::new());
                                                },
                                                class: "channels-picker__item",
                                                span { class: "channels-card__icon", "{ch_type.icon}" }
                                                div {
                                                    div { class: "channels-picker__name", "{ch_type.name}" }
                                                    div { class: "channels-picker__desc", "{ch_type.description}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if !other_types.is_empty() {
                        div { class: "channels-picker-section",
                            div { class: "channels-picker-section__label", "All Platforms" }
                            div { class: "channels-picker",
                                for ch_type in other_types.iter() {
                                    {
                                        let ch_id = ch_type.id.clone();
                                        let ch_name = ch_type.name.clone();
                                        rsx! {
                                            button {
                                                key: "{ch_type.id}",
                                                onclick: move |_| {
                                                    selected_channel.set(Some(ch_id.clone()));
                                                    channel_name.set(ch_name.clone());
                                                    config_values.write().clear();
                                                    revealed.write().clear();
                                                    save_msg.set(None);
                                                    add_channel_search.set(String::new());
                                                },
                                                class: "channels-picker__item",
                                                span { class: "channels-card__icon", "{ch_type.icon}" }
                                                div {
                                                    div { class: "channels-picker__name", "{ch_type.name}" }
                                                    div { class: "channels-picker__desc", "{ch_type.description}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if filtered_types.is_empty() {
                        div { class: "channels-picker-empty", "No channels match your search." }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn arkret_fields() -> Vec<ConfigField> {
        build_channel_types()
            .into_iter()
            .find(|channel| channel.id == "arkret")
            .expect("arkret channel type")
            .config_fields
    }

    fn sdk_inkson_bootstrap_value() -> Value {
        json!({
            "arkret_base_url": "https://arkret.example.org",
            "service_id": "did:webvh:arkret.example.org",
            "agent_id": "did:webvh:example.org:agents:support",
            "pairing_request_id": "pair-123",
            "pairing_code": "123456",
            "pairing_expires_at": "2026-07-06T12:00:00.000Z"
        })
    }

    fn sdk_inkson_bootstrap_json() -> String {
        serde_json::to_string(&sdk_inkson_bootstrap_value()).expect("bootstrap JSON")
    }

    #[test]
    fn channel_types_include_arkret() {
        let arkret = build_channel_types()
            .into_iter()
            .find(|channel| channel.id == "arkret")
            .expect("arkret channel type");

        assert_eq!(arkret.name, "Arkret");
        assert!(
            arkret
                .config_fields
                .iter()
                .any(|field| field.key == "keyRef")
        );
        assert!(
            arkret
                .config_fields
                .iter()
                .any(|field| field.key == "trustedVerificationMethods")
        );
        assert!(
            arkret
                .config_fields
                .iter()
                .any(|field| field.key == "advanced")
        );
        assert!(
            arkret
                .config_fields
                .iter()
                .any(|field| field.key == "runtimeKeyRequest")
        );
        assert!(
            arkret
                .config_fields
                .iter()
                .any(|field| field.key == "authorizationResult")
        );
        assert!(
            !arkret
                .config_fields
                .iter()
                .any(|field| field.key == "externalAiEndpointConfig")
        );
    }

    #[test]
    fn saved_channel_summaries_preserve_same_kind_instances() {
        let configs = json!([
            {
                "kind": "arkret",
                "id": "arkret-support",
                "name": "Support",
                "enabled": true
            },
            {
                "kind": "arkret",
                "id": "arkret-sales",
                "name": "Sales",
                "enabled": false
            }
        ]);

        let summaries = saved_channel_summaries(Some(&configs));

        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].id, "arkret-support");
        assert_eq!(summaries[1].id, "arkret-sales");
        assert!(summaries[0].enabled);
        assert!(!summaries[1].enabled);
    }

    #[test]
    fn new_channel_name_does_not_reuse_an_existing_instance_id() {
        let configs = saved_channel_summaries(Some(&json!([
            {
                "kind": "arkret",
                "id": "arkret-arkret",
                "name": "Arkret"
            },
            {
                "kind": "arkret",
                "id": "arkret-arkret-2",
                "name": "Arkret 2"
            }
        ])));

        assert_eq!(
            next_available_channel_name("arkret", "Arkret", &configs),
            "Arkret 3"
        );
    }

    #[test]
    fn new_arkret_form_does_not_restore_an_existing_binding() {
        let fields = arkret_fields();
        let values = new_channel_form_values("arkret", &fields, "Arkret 2".to_owned());

        assert_eq!(
            values.get(&channel_name_key("arkret")).map(String::as_str),
            Some("Arkret 2")
        );
        assert!(!values.contains_key(&saved_channel_id_key("arkret")));
        assert!(
            values
                .get(&field_value_key("arkret", "authorizedEventRef"))
                .is_none_or(String::is_empty)
        );
    }

    #[test]
    fn editing_a_channel_preserves_its_exact_instance_id() {
        let mut values = std::collections::HashMap::new();
        values.insert(saved_channel_id_key("arkret"), "arkret-support".to_owned());

        assert_eq!(
            channel_form_id("arkret", "Renamed support", &values),
            "arkret-support"
        );
        assert_eq!(
            channel_form_id("arkret", "New support", &std::collections::HashMap::new()),
            "arkret-new-support"
        );
    }

    #[test]
    fn arkret_pairing_link_uses_single_line_input() {
        let field = arkret_fields()
            .into_iter()
            .find(|field| field.key == "inksonBootstrap")
            .expect("Inkson pairing link field");

        assert_eq!(field.label, "Inkson pairing link");
        assert_eq!(field.field_type, FieldType::Text);
    }

    #[test]
    fn arkret_pairing_code_is_grouped_for_comparison() {
        assert_eq!(format_arkret_pairing_code("82294626"), "8229 4626");
        assert_eq!(format_arkret_pairing_code("1234"), "1234");
        assert_eq!(format_arkret_pairing_code("123456"), "1234 56");
        assert_eq!(
            arkret_pairing_expiry_from_bootstrap_text(&sdk_inkson_bootstrap_json()).as_deref(),
            Some("2026-07-06T12:00:00.000Z")
        );
    }

    #[test]
    fn arkret_status_labels_separate_pairing_from_runtime() {
        assert_eq!(arkret_pairing_state_label("paired"), "Paired");
        assert_eq!(arkret_pairing_state_label("active"), "Paired");
        assert_eq!(arkret_runtime_phase_label("subscribing"), "Listening");
        assert_eq!(arkret_runtime_phase_label("retry_wait"), "Retrying");
    }

    #[test]
    fn arkret_bound_state_waits_for_local_finalization() {
        let mut values = std::collections::HashMap::new();
        values.insert(
            field_value_key("arkret", "authorizedEventRef"),
            "ak:event:01904100-0000-7000-8000-000000000099".to_owned(),
        );

        assert!(arkret_agent_is_bound("arkret", &values));

        values.insert(arkret_pairing_state_key("arkret"), "finalizing".to_owned());
        assert!(!arkret_agent_is_bound("arkret", &values));

        values.insert(arkret_pairing_state_key("arkret"), "save_failed".to_owned());
        assert!(!arkret_agent_is_bound("arkret", &values));

        values.remove(&arkret_pairing_state_key("arkret"));
        assert!(arkret_agent_is_bound("arkret", &values));
    }

    #[test]
    fn arkret_bootstrap_summary_exposes_only_connection_identity() {
        let summary = arkret_bootstrap_summary_from_text(&sdk_inkson_bootstrap_json())
            .expect("bootstrap summary");

        assert_eq!(summary.0, "did:webvh:example.org:agents:support");
        assert_eq!(summary.1, "https://arkret.example.org");
    }

    #[test]
    fn arkret_bootstrap_parser_rejects_old_scope_payload_fields() {
        let mut value = sdk_inkson_bootstrap_value();
        value["requested_scope"] = json!({"actions": ["ak.event.read"]});

        let err = parse_arkret_agent_pairing_bootstrap(value)
            .expect_err("legacy scope payload must be rejected");

        assert!(err.contains("unknown field"));
    }

    #[test]
    fn arkret_simple_account_hides_low_level_fields() {
        let fields = arkret_fields();
        let values = default_channel_values("arkret", &fields);
        let visible = |key: &str| {
            fields
                .iter()
                .find(|field| field.key == key)
                .is_some_and(|field| field_is_visible("arkret", field, &values))
        };

        assert!(visible("inksonBootstrap"));
        assert!(visible("runtimeKeyRequest"));
        assert!(!visible("authorizationResult"));
        assert!(!visible("keyRef"));
        assert!(!visible("verificationMethod"));
        assert!(!visible("authorizedEventRef"));
        assert!(!visible("advanced"));
        assert!(!visible("baseUrl"));
        assert!(!visible("serviceId"));
        assert!(!visible("arkretServerDid"));
        assert!(!visible("principalId"));
        assert!(!visible("externalAiEndpointConfig"));
        assert!(!visible("accessToken"));
        assert!(!visible("loginChallenge"));
        assert!(!visible("defaultRealmId"));
        assert!(!visible("agentId"));
        assert!(!visible("listen"));
        assert!(!visible("send"));
        assert!(!visible("deviceId"));
        assert!(!visible("requestedScope"));
    }

    #[test]
    fn arkret_agent_hides_legacy_fields_even_when_advanced_is_set() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(field_value_key("arkret", "advanced"), "true".to_owned());
        let visible = |key: &str| {
            fields
                .iter()
                .find(|field| field.key == key)
                .is_some_and(|field| field_is_visible("arkret", field, &values))
        };

        assert!(!visible("advanced"));
        assert!(!visible("baseUrl"));
        assert!(!visible("serviceId"));
        assert!(!visible("principalId"));
        assert!(!visible("defaultRealmId"));
        assert!(!visible("agentId"));
        assert!(!visible("externalAiEndpointConfig"));
        assert!(!visible("requestedScope"));
        assert!(!visible("keyRef"));
        assert!(!visible("authorizedEventRef"));
        assert!(!visible("deviceId"));
        assert!(!visible("loginChallenge"));
    }

    #[test]
    fn arkret_agent_hides_approval_result_internal_field() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        let field = fields
            .iter()
            .find(|field| field.key == "authorizationResult")
            .expect("authorizationResult");

        assert!(!field_is_visible("arkret", field, &values));

        values.insert(
            field_value_key("arkret", "runtimeKeyRequest"),
            r#"{"pairing_request_id":"pair-123"}"#.to_owned(),
        );

        assert!(!field_is_visible("arkret", field, &values));
    }

    #[test]
    fn arkret_url_label_tracks_connection_type() {
        let fields = arkret_fields();
        let base_url_field = fields
            .iter()
            .find(|field| field.key == "baseUrl")
            .expect("baseUrl");
        let service_id_field = fields
            .iter()
            .find(|field| field.key == "serviceId")
            .expect("serviceId");
        let mut values = default_channel_values("arkret", &fields);

        assert!(!field_is_visible("arkret", base_url_field, &values));
        assert!(!field_is_visible("arkret", service_id_field, &values));

        values.insert(field_value_key("arkret", "mode"), "applet".to_owned());
        assert_eq!(
            field_display_label("arkret", base_url_field, &values),
            "Applet URL"
        );
        assert_eq!(
            field_display_placeholder("arkret", base_url_field, &values),
            "https://savfox.example/appservices/arkret/arkret-default"
        );
        assert_eq!(
            field_display_label("arkret", service_id_field, &values),
            "Applet Service DID"
        );
        assert!(field_display_required("arkret", service_id_field, &values));
    }

    #[test]
    fn arkret_saved_agent_config_does_not_restore_advanced_toggle() {
        let fields = arkret_fields();
        let saved = json!({
            "name": "Arkret",
            "config": {
                "mode": "agent",
                "baseUrl": "https://arkret.example.org",
                "inksonBootstrap": sdk_inkson_bootstrap_value(),
                "principalId": "did:webvh:example.org:agents:support",
                "defaultRealmId": "ak:realm:abc",
                "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
                "authorizedEventRef": "ak:event:01904100-0000-7000-8000-000000000099"
            }
        });

        let restored = restore_channel_values("arkret", &fields, &saved);

        assert!(!restored.contains_key(&field_value_key("arkret", "advanced")));
        assert_eq!(
            restored.get(&field_value_key("arkret", "keyRef")),
            Some(&"{\n  \"kind\": \"env\",\n  \"var\": \"SAVFOX_ARKRET_AGENT_KEY\"\n}".to_owned())
        );
    }

    #[test]
    fn arkret_runtime_key_request_can_start_from_bootstrap_without_protocol_payload() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"env","var":"SAVFOX_ARKRET_AGENT_KEY"}"#.to_owned(),
        );

        assert!(arkret_runtime_key_request_can_request("arkret", &values));
    }

    #[test]
    fn arkret_runtime_key_request_can_start_from_pairing_link_for_auto_resolve() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            "https://arkret.example.org/_arkret/open/agent-pairing/resolve#token=abcdefghijklmnopqrstuvwxyz"
                .to_owned(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"env","var":"SAVFOX_ARKRET_AGENT_KEY"}"#.to_owned(),
        );

        assert!(arkret_runtime_key_request_can_request("arkret", &values));
        values.insert(field_value_key("arkret", "inksonBootstrap"), String::new());
        assert!(!arkret_runtime_key_request_can_request("arkret", &values));
    }

    #[test]
    fn arkret_runtime_key_ref_generation_params_use_bootstrap_principal_only() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        values.insert(
            field_value_key("arkret", "principalId"),
            "did:webvh:example.org:agents:stale".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "verificationMethod"),
            "did:webvh:example.org:agents:stale#runtime-1".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"inline_seed_base64","value":"secret"}"#.to_owned(),
        );

        let params = arkret_runtime_key_ref_generation_params("arkret", &values);

        assert_eq!(params["platform"], "arkret");
        assert_eq!(params["agent_id"], "did:webvh:example.org:agents:support");
        assert_eq!(
            params["verification_method"],
            "did:webvh:example.org:agents:support#runtime-1"
        );
        assert!(params.get("keyRef").is_none());
        assert!(params.get("value").is_none());
    }

    #[test]
    fn arkret_agent_patch_rejects_missing_key_ref() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );

        let err = build_channel_patch("arkret", &fields, &values).expect_err("missing keyRef");

        assert!(err.contains("local runtime key"));
    }

    #[test]
    fn arkret_agent_patch_rejects_unresolved_pairing_link() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            "https://arkret.example.org/_arkret/open/agent-pairing/resolve#token=abcdefghijklmnopqrstuvwxyz"
                .to_owned(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"env","var":"SAVFOX_ARKRET_AGENT_KEY"}"#.to_owned(),
        );

        let err = build_channel_patch("arkret", &fields, &values)
            .expect_err("unresolved pairing link must not be saved");

        assert!(err.contains("Resolve the Inkson pairing link"));
    }

    #[test]
    fn arkret_account_patch_builds_flat_account_config() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        values.insert(
            field_value_key("arkret", "baseUrl"),
            "https://stale.example.org".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "serviceId"),
            "did:webvh:stale.example.org".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "principalId"),
            "did:webvh:example.org:agents:stale".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"env","var":"SAVFOX_ARKRET_AGENT_KEY"}"#.to_owned(),
        );
        values.insert(
            field_value_key("arkret", "authorizationResult"),
            r#"{"authorized_event_ref":"ak:event:01904100-0000-7000-8000-000000000099"}"#
                .to_owned(),
        );
        values.insert(
            field_value_key("arkret", "requestedScope"),
            "ak.self.events.stream.subscribe\nck.event.read".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "defaultRealmId"),
            "ak:realm:legacy".to_owned(),
        );
        values.insert(field_value_key("arkret", "agentId"), "legacy".to_owned());
        values.insert(
            field_value_key("arkret", "externalAiEndpointConfig"),
            r#"{"provider":"external","model":"agent-model","base_url":"https://ai.example/v1"}"#
                .to_owned(),
        );
        values.insert(
            field_value_key("arkret", "runtimeKeyRequest"),
            r#"{"must":"not be saved"}"#.to_owned(),
        );

        let patch = build_channel_patch("arkret", &fields, &values).expect("patch");

        assert_eq!(patch["mode"], json!("agent"));
        assert!(patch["listen"].is_null());
        assert!(patch["send"].is_null());
        assert!(patch["deviceId"].is_null());
        assert!(patch["defaultRealmId"].is_null());
        assert!(patch["agentId"].is_null());
        assert!(patch["requestedScope"].is_null());
        assert_eq!(
            patch["inksonBootstrap"]["pairing_request_id"],
            json!("pair-123")
        );
        assert_eq!(
            patch["keyRef"],
            json!({"kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY"})
        );
        assert!(patch["externalAiEndpointConfig"].is_null());
        assert!(patch["runtimeKeyRequest"].is_null());
        assert_eq!(
            patch["authorizedEventRef"],
            json!("ak:event:01904100-0000-7000-8000-000000000099")
        );
        assert!(patch["principalId"].is_null());
        assert!(patch["baseUrl"].is_null());
        assert!(patch["serviceId"].is_null());
        assert!(patch["appletId"].is_null());
        assert!(patch["namespaces"].is_null());
    }

    #[test]
    fn arkret_account_patch_ignores_hidden_external_ai_endpoint_config() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(field_value_key("arkret", "advanced"), "true".to_owned());
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"env","var":"SAVFOX_ARKRET_AGENT_KEY"}"#.to_owned(),
        );
        values.insert(
            field_value_key("arkret", "externalAiEndpointConfig"),
            r#""not-object""#.to_owned(),
        );

        let patch = build_channel_patch("arkret", &fields, &values).expect("patch");

        assert!(patch["externalAiEndpointConfig"].is_null());
    }

    #[test]
    fn arkret_bootstrap_defaults_fill_runtime_verification_method() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"env","var":"SAVFOX_ARKRET_AGENT_KEY"}"#.to_owned(),
        );

        let patch = build_channel_patch("arkret", &fields, &values).expect("patch");

        assert!(patch["baseUrl"].is_null());
        assert!(patch["serviceId"].is_null());
        assert!(patch["principalId"].is_null());
        assert!(patch["requestedScope"].is_null());
        assert_eq!(
            patch["verificationMethod"],
            json!("did:webvh:example.org:agents:support#runtime-1")
        );
    }

    #[test]
    fn arkret_bootstrap_replaces_stale_verification_method_and_authorization() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"env","var":"SAVFOX_ARKRET_AGENT_KEY"}"#.to_owned(),
        );
        values.insert(
            field_value_key("arkret", "verificationMethod"),
            "did:webvh:example.org:agents:stale#runtime-1".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "authorizedEventRef"),
            "ak:event:01904100-0000-7000-8000-000000000099".to_owned(),
        );

        let patch = build_channel_patch("arkret", &fields, &values).expect("patch");

        assert_eq!(
            patch["verificationMethod"],
            json!("did:webvh:example.org:agents:support#runtime-1")
        );
        assert!(patch["authorizedEventRef"].is_null());
    }

    #[test]
    fn arkret_applet_patch_builds_structured_namespaces() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(field_value_key("arkret", "mode"), "applet".to_owned());
        values.insert(
            field_value_key("arkret", "baseUrl"),
            "https://savfox.example/appservices/arkret/arkret-default".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "serviceId"),
            "did:web:slack-bridge.example".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "accessToken"),
            "applet-bearer-1".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "appletId"),
            "ak:applet:21532600-0000-7000-8000-000000000000".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "controllerId"),
            "did:webvh:example.com:admin".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "arkretServerUrl"),
            "https://arkret.example.org".to_owned(),
        );
        values.insert(field_value_key("arkret", "protocols"), "slack".to_owned());
        values.insert(
            field_value_key("arkret", "namespaceActors"),
            "did:web:slack-bridge.example:ghost:*".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "namespaceRealms"),
            "slack:team:*:channel:*".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "namespaceHandles"),
            "slack.acme.example/*".to_owned(),
        );

        let patch = build_channel_patch("arkret", &fields, &values).expect("patch");

        assert_eq!(patch["mode"], json!("applet"));
        assert_eq!(patch["controllerId"], json!("did:webvh:example.com:admin"));
        assert!(patch["receiveEvents"].is_null());
        assert!(patch["principalId"].is_null());
        assert_eq!(
            patch["namespaces"]["actors"][0],
            json!({
                "pattern": "did:web:slack-bridge.example:ghost:*",
                "exclusive": true
            })
        );
        assert_eq!(
            patch["namespaces"]["handles"][0],
            json!({
                "pattern": "slack.acme.example/*",
                "exclusive": false
            })
        );
    }
}

/// Injects the channels stylesheet into the document head exactly once,
/// instead of re-emitting an inline `<style>` block on every render/refresh.
fn inject_channels_styles_once() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Ok(el) = doc.create_element("style") {
                el.set_inner_html(CHANNELS_STYLES);
                if let Some(head) = doc.head() {
                    let _ = head.append_child(&el);
                }
            }
        }
    });
}

const CHANNELS_STYLES: &str = r#"
    /* ---- Page layout ---- */
    .channels-page {
        padding: 24px;
        display: flex;
        flex-direction: column;
        gap: 20px;
    }

    /* ---- Health dashboard ---- */
    .channels-health {
        display: grid;
        grid-template-columns: repeat(4, 1fr);
        gap: 12px;
    }

    .channels-health__card {
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        padding: 16px 20px;
        text-align: center;
        position: relative;
        overflow: hidden;
    }

    .channels-health__card::before {
        content: "";
        position: absolute;
        top: 0;
        left: 0;
        right: 0;
        height: 3px;
    }

    .channels-health__card--connected::before {
        background: var(--success);
    }

    .channels-health__card--running::before {
        background: var(--warning, #eab308);
    }

    .channels-health__card--disconnected::before {
        background: var(--text-muted);
    }

    .channels-health__card--errors::before {
        background: var(--danger);
    }

    .channels-health__value {
        font-size: 28px;
        font-weight: 700;
        line-height: 1.2;
    }

    .channels-health__card--connected .channels-health__value {
        color: var(--success);
    }

    .channels-health__card--running .channels-health__value {
        color: var(--warning, #eab308);
    }

    .channels-health__card--disconnected .channels-health__value {
        color: var(--text-muted);
    }

    .channels-health__card--errors .channels-health__value {
        color: var(--danger);
    }

    .channels-health__label {
        font-size: 12px;
        color: var(--text-secondary);
        margin-top: 4px;
    }

    .channels-health__indicator {
        display: inline-block;
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: var(--success);
        margin-top: 8px;
        animation: channels-pulse 2s ease-in-out infinite;
    }

    @keyframes channels-pulse {
        0%, 100% { opacity: 1; }
        50% { opacity: 0.4; }
    }

    /* ---- Toolbar ---- */
    .channels-toolbar {
        display: flex;
        justify-content: space-between;
        align-items: center;
        flex-wrap: wrap;
        gap: 12px;
    }

    .channels-toolbar__left {
        display: flex;
        flex-direction: column;
        gap: 4px;
    }

    .channels-toolbar__title {
        font-size: 20px;
        font-weight: 600;
    }

    .channels-toolbar__subtitle {
        font-size: 13px;
        color: var(--text-secondary);
    }

    .channels-toolbar__actions {
        display: flex;
        gap: 8px;
        align-items: center;
        flex-wrap: wrap;
    }

    /* ---- Toast notification ---- */
    .channels-toast {
        position: fixed;
        top: 20px;
        right: 20px;
        z-index: 2000;
        padding: 12px 20px;
        border-radius: var(--radius);
        font-size: 13px;
        display: flex;
        align-items: center;
        gap: 12px;
        cursor: pointer;
        box-shadow: 0 4px 16px rgba(0,0,0,0.25);
        animation: channels-toast-in 0.3s ease-out;
    }

    @keyframes channels-toast-in {
        from { opacity: 0; transform: translateY(-12px); }
        to   { opacity: 1; transform: translateY(0); }
    }

    .channels-toast--success {
        background: var(--success);
        color: #fff;
    }

    .channels-toast--error {
        background: var(--danger);
        color: #fff;
    }

    .channels-toast__text {
        flex: 1;
    }

    .channels-toast__dismiss {
        opacity: 0.7;
        font-size: 16px;
    }

    /* ---- Channel grid ---- */
    .channels-grid {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(360px, 1fr));
        gap: 16px;
        align-items: start;
    }

    .channels-empty {
        text-align: center;
        padding: 40px;
        color: var(--text-muted);
    }

    /* ---- Channel card ---- */
    .channels-card {
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        display: flex;
        flex-direction: column;
        transition: border-color 0.15s, box-shadow 0.15s;
    }

    .channels-card:hover {
        border-color: var(--text-muted);
    }

    .channels-card--connected {
        border-color: var(--success);
        box-shadow: 0 0 0 1px var(--success);
    }

    .channels-card--connected:hover {
        border-color: var(--success);
    }

    .channels-card--running {
        border-color: var(--warning, #eab308);
    }

    .channels-card--error {
        border-color: var(--danger);
    }

    .channels-card--configured {
        border-color: var(--accent);
        border-left: 3px solid var(--accent);
    }

    .channels-card--disabled {
        opacity: 0.6;
    }

    .channels-card--disabled:hover {
        opacity: 0.85;
    }

    .channels-card__header {
        display: flex;
        justify-content: space-between;
        align-items: flex-start;
        padding: 20px 20px 0 20px;
    }

    .channels-card__identity {
        display: flex;
        align-items: center;
        gap: 12px;
    }

    .channels-card__icon {
        width: 36px;
        height: 36px;
        display: flex;
        align-items: center;
        justify-content: center;
        background: var(--bg-tertiary);
        border-radius: var(--radius);
        font-weight: 600;
        font-size: 14px;
        color: var(--text-secondary);
        flex-shrink: 0;
    }

    .channels-card__meta {
        display: flex;
        flex-direction: column;
        gap: 2px;
    }

    .channels-card__name {
        font-size: 16px;
        font-weight: 600;
    }

    .channels-card__desc {
        font-size: 12px;
        color: var(--text-muted);
    }

    .channels-card__config-id {
        font-size: 11px;
        color: var(--text-muted);
        margin-top: 2px;
        font-family: monospace;
    }

    .channels-card__config-id-label {
        font-size: 10px;
        color: var(--text-muted);
        background: var(--bg-tertiary);
        padding: 1px 4px;
        border-radius: 3px;
        margin-right: 3px;
    }

    .channels-card__config-id-sep {
        color: var(--border);
    }

    .channels-card__config-name {
        color: var(--text-secondary);
    }

    .channels-card__header-actions {
        display: flex;
        align-items: center;
        gap: 8px;
        flex-shrink: 0;
    }

    .channels-card__menu-wrap {
        position: relative;
    }

    .channels-card__menu-trigger {
        width: 30px;
        height: 30px;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        padding: 0;
        border: 1px solid var(--border);
        border-radius: var(--radius);
        background: transparent;
        color: var(--text-muted);
        cursor: pointer;
    }

    .channels-card__menu-trigger:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
    }

    .channels-card__menu {
        position: absolute;
        z-index: 30;
        top: calc(100% + 6px);
        right: 0;
        width: 190px;
        padding: 6px;
        border: 1px solid var(--border);
        border-radius: var(--radius);
        background: var(--bg-primary);
        box-shadow: 0 12px 32px rgba(0,0,0,0.35);
    }

    .channels-card__menu-item {
        width: 100%;
        display: flex;
        align-items: center;
        gap: 9px;
        padding: 8px 9px;
        border: 0;
        border-radius: calc(var(--radius) - 2px);
        background: transparent;
        color: var(--text-secondary);
        font-size: 12px;
        text-align: left;
        cursor: pointer;
    }

    .channels-card__menu-item:hover:not(:disabled) {
        background: var(--bg-hover);
        color: var(--text-primary);
    }

    .channels-card__menu-item:disabled {
        cursor: wait;
        opacity: 0.55;
    }

    .channels-card__menu-item--danger {
        color: var(--danger);
    }

    .channels-card__menu-separator {
        height: 1px;
        margin: 5px 3px;
        background: var(--border);
    }

    .channels-card__menu-confirm {
        display: flex;
        flex-direction: column;
        gap: 8px;
        padding: 7px 8px;
        color: var(--danger);
        font-size: 11px;
        line-height: 1.4;
    }

    .channels-card__menu-confirm-actions {
        display: flex;
        gap: 6px;
    }

    .channels-card__menu-confirm-delete,
    .channels-card__menu-confirm-cancel {
        flex: 1;
        padding: 6px 8px;
        border: 1px solid var(--border);
        border-radius: calc(var(--radius) - 2px);
        background: transparent;
        color: var(--text-secondary);
        font-size: 11px;
        cursor: pointer;
    }

    .channels-card__menu-confirm-delete {
        border-color: var(--danger);
        background: rgba(239,68,68,0.12);
        color: var(--danger);
    }

    .channels-card__menu-confirm-delete:disabled,
    .channels-card__menu-confirm-cancel:disabled {
        cursor: wait;
        opacity: 0.55;
    }

    /* ---- Accounts section ---- */
    .channels-card__platform-info {
        margin: 10px 20px 0 20px;
        padding: 8px 12px;
        display: flex;
        flex-wrap: wrap;
        gap: 4px 14px;
        background: var(--bg-tertiary);
        border-radius: var(--radius);
        font-size: 11px;
    }

    .channels-card__pinfo-item {
        display: inline-flex;
        align-items: center;
        gap: 5px;
    }

    .channels-card__pinfo-label {
        color: var(--text-muted);
    }

    .channels-card__pinfo-value {
        color: var(--text-primary);
        font-weight: 600;
    }

    .channels-card__pinfo-value--truncate {
        max-width: 120px;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }

    /* ---- Activity + error ---- */
    .channels-card__activity {
        margin: 12px 20px 0 20px;
        font-size: 11px;
        color: var(--text-muted);
    }

    .channels-card__metrics {
        margin: 10px 20px 0 20px;
        display: grid;
        grid-template-columns: repeat(2, minmax(0, 1fr));
        gap: 6px 12px;
        padding: 10px 12px;
        border: 1px solid var(--border);
        border-radius: var(--radius);
        background: var(--bg-secondary);
    }

    .channels-card__metric {
        display: flex;
        justify-content: space-between;
        gap: 8px;
        font-size: 11px;
    }

    .channels-card__metric-label {
        color: var(--text-muted);
    }

    .channels-card__metric-value {
        color: var(--text-primary);
        font-weight: 600;
        text-align: right;
        word-break: break-word;
    }

    .channels-card__error {
        margin: 12px 20px 0 20px;
        padding: 8px 12px;
        background: rgba(239,68,68,0.1);
        border: 1px solid var(--danger);
        border-radius: var(--radius);
        color: var(--danger);
        font-size: 12px;
        word-break: break-all;
    }

    /* ---- Card action buttons ---- */
    .channels-card__actions {
        display: flex;
        gap: 8px;
        padding: 16px 20px 12px 20px;
        margin-top: auto;
    }

    /* ---- Buttons ---- */
    .channels-btn {
        padding: 6px 14px;
        background: var(--bg-tertiary);
        color: var(--text-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        font-size: 12px;
        cursor: pointer;
        transition: background 0.15s, color 0.15s;
    }

    .channels-btn:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
    }

    .channels-btn--auto-active {
        background: var(--accent);
        color: #fff;
        border-color: var(--accent);
    }

    .channels-btn--auto-active:hover {
        opacity: 0.9;
    }

    .channels-btn--primary {
        background: var(--button-cta-surface);
        color: #fff;
        border-color: transparent;
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-cta-shadow);
    }

    .channels-btn--primary:hover {
        opacity: 1;
        background: var(--button-cta-surface-hover);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-cta-shadow-hover);
    }

    .channels-action-btn {
        flex: 0 0 auto;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        gap: 6px;
        padding: 8px 16px;
        background: transparent;
        color: var(--text-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        font-size: 13px;
        cursor: pointer;
        transition: background 0.15s, color 0.15s, border-color 0.15s;
        text-align: center;
        white-space: nowrap;
    }

    .channels-action-btn:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
    }

    .channels-action-btn:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    .channels-action-btn--primary {
        background: var(--button-cta-surface);
        color: #fff;
        border-color: transparent;
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-cta-shadow);
    }

    .channels-action-btn--primary:hover {
        opacity: 1;
        background: var(--button-cta-surface-hover);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-cta-shadow-hover);
    }

    .channels-action-btn--configured {
        background: rgba(34,197,94,0.1);
        color: var(--success);
        border-color: var(--success);
        cursor: default;
    }

    .channels-action-btn--configured:hover {
        background: rgba(34,197,94,0.15);
    }

    .channels-action-btn--test {
        color: var(--accent);
        border-color: var(--accent);
    }

    .channels-action-btn--test:hover {
        background: var(--accent);
        color: #fff;
    }

    .channels-action-btn--muted {
        color: var(--text-muted);
    }

    .channels-action-btn--danger {
        color: var(--danger);
        border-color: var(--danger);
    }

    .channels-action-btn--danger:hover {
        background: var(--danger);
        color: #fff;
    }

    /* ---- Modal ---- */
    .channels-modal-backdrop {
        position: fixed;
        top: 0;
        left: 0;
        right: 0;
        bottom: 0;
        background: rgba(0,0,0,0.6);
        display: flex;
        align-items: center;
        justify-content: center;
        z-index: 1000;
    }

    .channels-modal {
        position: relative;
        background: var(--bg-primary);
        border: 1px solid var(--border);
        border-radius: var(--radius-lg);
        padding: 24px;
        width: 90%;
        max-width: 600px;
        max-height: 80vh;
        overflow-y: auto;
    }

    .channels-modal--wide {
        max-width: 780px;
    }

    .channels-modal__header {
        position: sticky;
        top: -24px;
        z-index: 10;
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin: -24px -24px 20px;
        padding: 24px 24px 12px;
        background: var(--bg-primary);
        border-bottom: 1px solid var(--border);
    }

    .channels-modal__title {
        font-size: 18px;
        font-weight: 600;
    }

    .channels-modal__close {
        padding: 4px 8px;
        background: transparent;
        color: var(--text-secondary);
        border: none;
        font-size: 20px;
        cursor: pointer;
        line-height: 1;
    }

    .channels-modal__close:hover {
        color: var(--text-primary);
    }

    .channels-modal__pre {
        background: var(--bg-tertiary);
        padding: 12px;
        border-radius: var(--radius);
        font-size: 12px;
        overflow: auto;
        max-height: 500px;
        color: var(--text-secondary);
        white-space: pre-wrap;
        word-break: break-all;
    }

    .channels-modal__header-left {
        display: flex;
        align-items: center;
        gap: 12px;
    }

    /* ---- Health status indicators ---- */
    .channels-health-dot {
        display: inline-block;
        width: 10px;
        height: 10px;
        border-radius: 50%;
        flex-shrink: 0;
    }

    .channels-health-dot--connected {
        background: var(--success);
        box-shadow: 0 0 6px var(--success);
    }

    .channels-health-dot--running {
        background: var(--warning, #eab308);
        box-shadow: 0 0 6px var(--warning, #eab308);
    }

    .channels-health-dot--error {
        background: var(--danger);
        box-shadow: 0 0 6px var(--danger);
    }

    .channels-health-dot--disconnected {
        background: var(--text-muted);
    }

    .channels-health-status {
        display: flex;
        align-items: center;
        gap: 6px;
    }

    .channels-health-status__text {
        font-size: 13px;
        color: var(--text-secondary);
    }

    .channels-health-error {
        margin-bottom: 16px;
        padding: 12px 16px;
        background: rgba(239,68,68,0.08);
        border: 1px solid var(--danger);
        border-radius: var(--radius);
        display: flex;
        flex-direction: column;
        gap: 8px;
    }

    .channels-health-error__header {
        display: flex;
        align-items: center;
        gap: 8px;
    }

    .channels-health-error__label {
        font-size: 12px;
        font-weight: 600;
        color: var(--danger);
    }

    .channels-health-error__message {
        font-size: 13px;
        color: var(--danger);
        word-break: break-all;
    }

    .channels-modal__alert {
        margin-bottom: 16px;
        padding: 8px 12px;
        border-radius: var(--radius);
        font-size: 13px;
    }

    .channels-modal__alert--success {
        background: rgba(34,197,94,0.1);
        border: 1px solid var(--success);
        color: var(--success);
    }

    /* ---- Form ---- */
    .channels-modal__form {
        display: flex;
        flex-direction: column;
        gap: 0;
    }

    .channels-modal__form-actions {
        position: sticky;
        bottom: -24px;
        z-index: 10;
        display: flex;
        gap: 8px;
        margin: 20px -24px -24px;
        padding: 12px 24px 24px;
        border-top: 1px solid var(--border);
        background: var(--bg-primary);
        box-shadow: 0 -10px 18px rgba(0, 0, 0, 0.12);
    }

    .channels-field {
        margin-bottom: 16px;
    }

    .channels-field__label {
        display: flex;
        align-items: center;
        gap: 8px;
        font-size: 13px;
        font-weight: 500;
        color: var(--text-secondary);
        margin-bottom: 6px;
    }

    .channels-field__hint {
        margin-top: 6px;
        font-size: 12px;
        color: var(--text-muted);
    }

    .channels-field__required {
        color: var(--danger, #ef4444);
        font-weight: 700;
        font-size: 14px;
    }

    .channels-field__secret-badge {
        font-size: 10px;
        padding: 1px 6px;
        background: rgba(239,68,68,0.15);
        color: var(--danger);
        border-radius: 8px;
        font-weight: 600;
    }

    .channels-field__input {
        width: 100%;
        padding: 10px 14px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        color: var(--text-primary);
        font-size: 14px;
        outline: none;
        transition: border-color 0.15s;
        box-sizing: border-box;
    }

    .channels-field__input:focus {
        border-color: var(--accent);
    }

    .channels-field__input--readonly {
        background: var(--bg-secondary);
        color: var(--text-muted);
        cursor: default;
    }

    /* ---- Picker grid ---- */
    .channels-picker-search {
        margin-bottom: 16px;
    }

    .channels-picker-search__input {
        width: 100%;
        padding: 10px 14px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        color: var(--text-primary);
        font-size: 14px;
        outline: none;
        transition: border-color 0.15s;
        box-sizing: border-box;
    }

    .channels-picker-search__input:focus {
        border-color: var(--accent);
    }

    .channels-picker-search__input::placeholder {
        color: var(--text-muted);
    }

    .channels-picker-section {
        margin-bottom: 16px;
    }

    .channels-picker-section__label {
        font-size: 11px;
        font-weight: 600;
        color: var(--text-muted);
        margin-bottom: 8px;
        padding-left: 2px;
    }

    .channels-picker-empty {
        text-align: center;
        padding: 24px;
        color: var(--text-muted);
        font-size: 13px;
    }

    .channels-picker {
        display: grid;
        grid-template-columns: repeat(2, 1fr);
        gap: 12px;
    }

    .channels-picker__item {
        display: flex;
        align-items: center;
        gap: 12px;
        padding: 16px;
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        cursor: pointer;
        text-align: left;
        transition: border-color 0.15s, background 0.15s;
    }

    .channels-picker__item:hover {
        border-color: var(--accent);
        background: var(--bg-hover);
    }

    .channels-picker__name {
        font-size: 14px;
        font-weight: 500;
        color: var(--text-primary);
    }

    .channels-picker__desc {
        font-size: 12px;
        color: var(--text-muted);
        margin-top: 4px;
    }

    /* ================================================================
       Channel configuration (used inside modal)
       ================================================================ */

    .channels-cfg__section {
        margin-top: 18px;
        padding-top: 18px;
        border-top: 1px solid var(--border);
    }

    .channels-cfg__section-title {
        font-size: 12px;
        font-weight: 600;
        letter-spacing: 0.03em;
        color: var(--text-secondary);
        margin-bottom: 8px;
    }

    .channels-cfg__fields {
        display: flex;
        flex-direction: column;
        gap: 0;
    }

    .channels-cfg__field {
        margin-bottom: 14px;
    }

    /* ---- Password field with reveal toggle ---- */
    .channels-cfg__password-wrap {
        display: flex;
        gap: 0;
        align-items: stretch;
    }

    .channels-cfg__password-input {
        border-top-right-radius: 0;
        border-bottom-right-radius: 0;
        flex: 1;
    }

    .channels-cfg__reveal-btn {
        padding: 0 12px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-left: none;
        border-top-right-radius: var(--radius);
        border-bottom-right-radius: var(--radius);
        color: var(--text-muted);
        font-size: 11px;
        font-weight: 500;
        cursor: pointer;
        white-space: nowrap;
        transition: color 0.15s, background 0.15s;
    }

    .channels-cfg__reveal-btn:hover {
        color: var(--text-primary);
        background: var(--bg-hover);
    }

    /* ---- Textarea ---- */
    .channels-cfg__textarea {
        resize: vertical;
        min-height: 80px;
        font-family: var(--font-mono);
        font-size: 12px;
        line-height: 1.5;
    }

    .channels-cfg__row-actions {
        display: flex;
        gap: 8px;
        flex-wrap: wrap;
        margin-top: 8px;
    }

    .arkret-pairing-code {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 20px;
        margin-top: 12px;
        padding: 16px;
        border: 1px solid color-mix(in srgb, var(--accent) 55%, var(--border));
        border-radius: var(--radius-lg);
        background: color-mix(in srgb, var(--accent) 8%, var(--bg-secondary));
    }

    .arkret-pairing-code__content {
        display: flex;
        min-width: 0;
        flex-direction: column;
        gap: 5px;
    }

    .arkret-pairing-code__label {
        color: var(--text-secondary);
        font-size: 12px;
        font-weight: 600;
    }

    .arkret-pairing-code__value {
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 28px;
        line-height: 1.2;
        letter-spacing: 0.12em;
    }

    .arkret-pairing-code__hint {
        color: var(--text-muted);
        font-size: 11px;
        line-height: 1.45;
    }

    .arkret-pairing-code__expiry {
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 10px;
    }

    .arkret-pairing-code__copy {
        flex-shrink: 0;
    }

    .arkret-pairing-action {
        margin-top: -2px;
    }

    .arkret-pairing-status {
        margin-top: 9px;
        padding: 9px 11px;
        border: 1px solid var(--border);
        border-radius: var(--radius);
        background: var(--bg-secondary);
        color: var(--text-secondary);
        font-size: 12px;
        line-height: 1.45;
    }

    .arkret-pairing-status--error {
        border-color: color-mix(in srgb, var(--danger) 55%, var(--border));
        background: color-mix(in srgb, var(--danger) 8%, var(--bg-secondary));
        color: var(--danger);
    }

    .arkret-connection {
        padding: 16px;
        border: 1px solid color-mix(in srgb, var(--success) 55%, var(--border));
        border-radius: var(--radius-lg);
        background: color-mix(in srgb, var(--success) 7%, var(--bg-secondary));
    }

    .arkret-connection__header,
    .arkret-connection__status {
        display: flex;
        align-items: center;
    }

    .arkret-connection__header {
        justify-content: space-between;
        gap: 12px;
    }

    .arkret-connection__status {
        gap: 7px;
        color: var(--text-primary);
        font-size: 13px;
        font-weight: 600;
    }

    .arkret-connection__status svg {
        color: var(--success);
    }

    .arkret-connection__badge {
        padding: 3px 8px;
        border-radius: 999px;
        background: color-mix(in srgb, var(--success) 14%, transparent);
        color: var(--success);
        font-size: 11px;
        font-weight: 600;
    }

    .arkret-connection__details {
        display: grid;
        gap: 8px;
        margin-top: 14px;
    }

    .arkret-connection__detail {
        display: grid;
        grid-template-columns: 92px minmax(0, 1fr);
        gap: 12px;
        font-size: 12px;
    }

    .arkret-connection__detail-label {
        color: var(--text-muted);
    }

    .arkret-connection__detail-value {
        overflow: hidden;
        color: var(--text-secondary);
        text-overflow: ellipsis;
        white-space: nowrap;
    }

    .arkret-connection__management {
        margin-top: 14px;
        padding-top: 12px;
        border-top: 1px solid var(--border);
    }

    .arkret-disconnect-confirm {
        padding: 12px;
        border: 1px solid color-mix(in srgb, var(--danger) 55%, var(--border));
        border-radius: var(--radius);
        background: color-mix(in srgb, var(--danger) 8%, var(--bg-primary));
    }

    .arkret-disconnect-confirm__message {
        display: flex;
        align-items: flex-start;
        gap: 9px;
        color: var(--danger);
        font-size: 12px;
    }

    .arkret-disconnect-confirm__message svg {
        flex-shrink: 0;
        margin-top: 1px;
    }

    .arkret-disconnect-confirm__message p {
        margin: 4px 0 0;
        color: var(--text-secondary);
        line-height: 1.45;
    }

    .channels-spin {
        animation: channels-spin 0.8s linear infinite;
    }

    @keyframes channels-spin {
        to { transform: rotate(360deg); }
    }

    /* ---- Select ---- */
    .channels-cfg__select {
        appearance: none;
        background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 12 12'%3E%3Cpath fill='%23888' d='M6 8L1 3h10z'/%3E%3C/svg%3E");
        background-repeat: no-repeat;
        background-position: right 12px center;
        padding-right: 32px;
        cursor: pointer;
    }

    /* ---- Toggle switch ---- */
    .channels-cfg__toggle-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
    }

    .channels-cfg__toggle-btn {
        position: relative;
        width: 42px;
        height: 24px;
        padding: 0;
        border: none;
        background: transparent;
        cursor: pointer;
        flex-shrink: 0;
    }

    .channels-cfg__toggle-track {
        width: 42px;
        height: 24px;
        border-radius: 12px;
        background: var(--border);
        transition: background 0.2s;
        position: relative;
    }

    .channels-cfg__toggle-btn--on .channels-cfg__toggle-track {
        background: var(--accent);
    }

    .channels-cfg__toggle-thumb {
        position: absolute;
        top: 3px;
        left: 3px;
        width: 18px;
        height: 18px;
        border-radius: 50%;
        background: #fff;
        transition: transform 0.2s;
        box-shadow: 0 1px 3px rgba(0,0,0,0.2);
    }

    .channels-cfg__toggle-btn--on .channels-cfg__toggle-thumb {
        transform: translateX(18px);
    }

    /* ---- Inline config alerts ---- */
    .channels-cfg__alert {
        padding: 8px 12px;
        border-radius: var(--radius);
        font-size: 12px;
        margin-bottom: 12px;
    }

    .channels-cfg__alert--success {
        background: rgba(34,197,94,0.1);
        border: 1px solid var(--success);
        color: var(--success);
    }

    .channels-cfg__alert--error {
        background: rgba(239,68,68,0.1);
        border: 1px solid var(--danger);
        color: var(--danger);
    }

    /* ---- Config action row ---- */
    .channels-cfg__actions {
        position: sticky;
        bottom: -24px;
        z-index: 10;
        display: flex;
        align-items: center;
        gap: 8px;
        margin: 8px -24px -24px;
        padding: 12px 24px 24px;
        border-top: 1px solid var(--border);
        background: var(--bg-primary);
        box-shadow: 0 -10px 18px rgba(0, 0, 0, 0.12);
    }

    .channels-cfg__delete-action {
        display: flex;
        align-items: center;
        margin-left: auto;
    }

    .channels-cfg__delete-confirm {
        display: flex;
        align-items: center;
        justify-content: flex-end;
        gap: 8px;
        flex-wrap: wrap;
        color: var(--danger);
        font-size: 12px;
        text-align: right;
    }

    .channels-action-btn--danger {
        border-color: var(--danger);
        color: var(--danger);
    }

    .channels-action-btn--danger:hover:not(:disabled) {
        background: rgba(239,68,68,0.12);
    }

    /* ---- Responsive ---- */
    @media screen and (max-width: 768px) {
        .channels-health {
            grid-template-columns: repeat(2, 1fr);
        }

        .channels-toolbar {
            flex-direction: column;
            align-items: flex-start;
        }

        .channels-grid {
            grid-template-columns: 1fr;
        }

        .channels-picker {
            grid-template-columns: 1fr;
        }

        .channels-card__metrics {
            grid-template-columns: 1fr;
        }
    }

    @media screen and (max-width: 480px) {
        .channels-health {
            grid-template-columns: 1fr 1fr;
            gap: 8px;
        }

        .channels-health__value {
            font-size: 22px;
        }

        .channels-card__actions {
            flex-wrap: wrap;
        }

        .channels-cfg__actions {
            flex-wrap: wrap;
        }

        .arkret-pairing-code {
            align-items: stretch;
            flex-direction: column;
        }

        .arkret-pairing-code__copy {
            width: 100%;
        }

        .arkret-connection__detail {
            grid-template-columns: 1fr;
            gap: 3px;
        }

        .channels-cfg__delete-confirm {
            justify-content: flex-start;
            text-align: left;
        }
    }
"#;
