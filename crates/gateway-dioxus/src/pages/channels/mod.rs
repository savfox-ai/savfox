pub mod cokret;
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

use dioxus::prelude::*;
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

#[derive(Clone, Debug, PartialEq)]
enum FieldType {
    Text,
    Password,
    Number,
    Textarea,
    Toggle,
    Select(Vec<String>),
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
            id: "cokret".into(),
            name: "Cokret".into(),
            icon: "Ck".into(),
            description: "Connect a Cokret AI agent or registered applet".into(),
            config_fields: vec![
                ConfigField {
                    key: "mode".into(),
                    label: "Connection Type".into(),
                    field_type: FieldType::Select(vec!["account".into(), "applet".into()]),
                    placeholder: "account".into(),
                    secret: false,
                    required: true,
                    help: "Account connects a Yougen-created AI agent. Applet exposes Savfox as a registered Cokret Applet endpoint.",
                },
                ConfigField {
                    key: "baseUrl".into(),
                    label: "Cokret / Applet URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "Account: https://cokret.example.org; Applet: https://savfox.example/appservices/cokret/cokret-default".into(),
                    secret: false,
                    required: true,
                    help: "Account mode uses the Cokret server URL. Applet mode uses the public Savfox callback URL registered in Cokret.",
                },
                ConfigField {
                    key: "serviceDid".into(),
                    label: "Service / Audience DID".into(),
                    field_type: FieldType::Text,
                    placeholder: "did:webvh:cokret.example.org or did:web:savfox.example".into(),
                    secret: false,
                    required: false,
                    help: "Applet service DID, or the DID-proof login audience when account advanced auth is enabled.",
                },
                ConfigField {
                    key: "accessToken".into(),
                    label: "Session Grant / Bearer Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "ck.session.grant or applet bearer token".into(),
                    secret: true,
                    required: false,
                    help: "Session grant produced by Cokret/Yougen pairing, or the inbound applet bearer token. Device IDs are generated automatically.",
                },
                ConfigField {
                    key: "deviceId".into(),
                    label: "Device ID (internal)".into(),
                    field_type: FieldType::Text,
                    placeholder: "ck:device:...".into(),
                    secret: false,
                    required: false,
                    help: "Internal Cokret runtime device id. Savfox derives this automatically.",
                },
                ConfigField {
                    key: "principalId".into(),
                    label: "Agent DID".into(),
                    field_type: FieldType::Text,
                    placeholder: "did:web:agent.example".into(),
                    secret: false,
                    required: true,
                    help: "AI agent principal DID created in Yougen.",
                },
                ConfigField {
                    key: "defaultRealmId".into(),
                    label: "Realm ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "ck:realm:...".into(),
                    secret: false,
                    required: false,
                    help: "Realm where this AI agent reads and writes. Required when outbound replies are enabled.",
                },
                ConfigField {
                    key: "defaultFlowId".into(),
                    label: "Flow ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "ck:flow:...".into(),
                    secret: false,
                    required: false,
                    help: "Optional flow id attached to outbound account messages.",
                },
                ConfigField {
                    key: "agentId".into(),
                    label: "Savfox Agent".into(),
                    field_type: FieldType::Text,
                    placeholder: "default".into(),
                    secret: false,
                    required: false,
                    help: "Savfox agent that handles inbound Cokret messages.",
                },
                ConfigField {
                    key: "listen".into(),
                    label: "Receive messages".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Start the account listener for incoming Cokret events.",
                },
                ConfigField {
                    key: "send".into(),
                    label: "Send replies".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Allow Savfox to send replies through this account.",
                },
                ConfigField {
                    key: "advanced".into(),
                    label: "Advanced settings".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Show low-level Cokret auth, scope, signing, and applet runtime fields.",
                },
                ConfigField {
                    key: "scopes".into(),
                    label: "Permission Scopes".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "ck.message.create\nck.message.read".into(),
                    secret: false,
                    required: false,
                    help: "Optional account scopes, one per line or comma-separated.",
                },
                ConfigField {
                    key: "appletId".into(),
                    label: "Applet ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "ck:applet:...".into(),
                    secret: false,
                    required: true,
                    help: "Registered Cokret applet identifier.",
                },
                ConfigField {
                    key: "controllerDid".into(),
                    label: "Controller DID".into(),
                    field_type: FieldType::Text,
                    placeholder: "did:webvh:cokret.example.org".into(),
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
                    help: "Visible applet bot actor DID. Defaults to serviceDid:bot.",
                },
                ConfigField {
                    key: "cokretServerUrl".into(),
                    label: "Cokret Server URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://cokret.example.org".into(),
                    secret: false,
                    required: false,
                    help: "Cokret server used by the applet for outbound event submission.",
                },
                ConfigField {
                    key: "cokretServerDid".into(),
                    label: "Cokret Server DID".into(),
                    field_type: FieldType::Text,
                    placeholder: "did:webvh:cokret.example.org".into(),
                    secret: false,
                    required: false,
                    help: "Trusted Cokret server DID for DID-proof login and HTTP Message Signature verification.",
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
                    help: "Applet actor namespace patterns. Saved as Cokret namespaces.actors[].",
                },
                ConfigField {
                    key: "namespaceRealms".into(),
                    label: "Realm Namespaces".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "slack:team:*:channel:*".into(),
                    secret: false,
                    required: false,
                    help: "Applet realm namespace patterns. Saved as Cokret namespaces.realms[].",
                },
                ConfigField {
                    key: "namespaceHandles".into(),
                    label: "Handle Namespaces".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "slack.example/*".into(),
                    secret: false,
                    required: false,
                    help: "Applet third-party handle namespace patterns. Saved as Cokret namespaces.handles[].",
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
                    placeholder: "ck.message.create\nck.flow.create".into(),
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
                    help: "Allow Cokret to push event transactions to this applet",
                },
                ConfigField {
                    key: "receiveEphemeral".into(),
                    label: "Receive Ephemeral".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Allow ephemeral Cokret events such as typing/presence",
                },
                ConfigField {
                    key: "rateLimited".into(),
                    label: "Allow Rate Limiting".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
                    required: false,
                    help: "Permit the Cokret server to rate-limit applet transaction pushes",
                },
                ConfigField {
                    key: "authorizationGrantId".into(),
                    label: "Authorization Grant ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "ck:event:...".into(),
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
                    placeholder: r#"[{"verificationMethod":"did:webvh:cokret.example.org#key-1","publicKeyMultibase":"z..."}]"#.into(),
                    secret: false,
                    required: false,
                    help: "Optional array of Cokret server HTTP Message Signature public keys",
                },
                ConfigField {
                    key: "loginChallenge".into(),
                    label: "Login Challenge".into(),
                    field_type: FieldType::Text,
                    placeholder: "challenge-from-cokret".into(),
                    secret: false,
                    required: false,
                    help: "Server-issued challenge required when keyRef is used for DID-proof login",
                },
                ConfigField {
                    key: "verificationMethod".into(),
                    label: "Signing Verification Method".into(),
                    field_type: FieldType::Text,
                    placeholder: "did:web:agent.example#key-1".into(),
                    secret: false,
                    required: false,
                    help: "Verification method id for the local Ed25519 signer",
                },
                ConfigField {
                    key: "grantEventPath".into(),
                    label: "Grant Event Path".into(),
                    field_type: FieldType::Text,
                    placeholder: "C:\\secrets\\cokret-grant.json".into(),
                    secret: false,
                    required: false,
                    help: "Path to a pre-signed ck.capability.grant Event JSON",
                },
                ConfigField {
                    key: "keyRef".into(),
                    label: "Key Ref JSON".into(),
                    field_type: FieldType::Textarea,
                    placeholder: r#"{"kind":"env","var":"SAVFOX_COKRET_BOT_KEY"}"#.into(),
                    secret: true,
                    required: false,
                    help: "Optional signer key reference for DID-proof login and event signing",
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
    let map = channels_status
        .and_then(|s| s.get("channels"))
        .and_then(|c| c.as_object());

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

fn field_value_key(channel_id: &str, field_key: &str) -> String {
    format!("{channel_id}.{field_key}")
}

fn channel_name_key(channel_id: &str) -> String {
    format!("{channel_id}.__name")
}

fn compute_channel_id(name: &str, kind: &str) -> String {
    let slug = normalize_slug(name).unwrap_or_else(|| "default".to_string());
    format!("{kind}-{slug}")
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

        if is_cokret_channel(fields) {
            restore_cokret_derived_values(channel_id, &mut restored, config_obj);
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

fn restore_cokret_derived_values(
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

    for key in ["keyRef", "trustedVerificationMethods"] {
        if let Some(raw) = config_obj.get(key) {
            let rendered = serde_json::to_string_pretty(raw).unwrap_or_else(|_| raw.to_string());
            restored.insert(field_value_key(channel_id, key), rendered);
        }
    }

    if cokret_config_has_advanced_values(config_obj) {
        restored.insert(field_value_key(channel_id, "advanced"), "true".to_string());
    }
}

fn cokret_config_has_advanced_values(config_obj: &serde_json::Map<String, Value>) -> bool {
    let mode = config_obj
        .get("mode")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "account".to_string());
    let account_advanced = [
        "serviceDid",
        "cokretServerDid",
        "defaultFlowId",
        "scopes",
        "loginChallenge",
        "verificationMethod",
        "grantEventPath",
        "keyRef",
    ];
    let applet_advanced = [
        "botActorId",
        "cokretServerDid",
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
    let keys: &[&str] = if mode == "applet" {
        &applet_advanced
    } else {
        &account_advanced
    };
    keys.iter()
        .any(|key| config_obj.get(*key).is_some_and(cokret_value_is_present))
}

fn cokret_value_is_present(value: &Value) -> bool {
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

fn is_cokret_channel(fields: &[ConfigField]) -> bool {
    fields.iter().any(|field| field.key == "mode")
        && fields.iter().any(|field| field.key == "appletId")
        && fields.iter().any(|field| field.key == "principalId")
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

fn current_cokret_mode(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> String {
    values
        .get(&field_value_key(channel_id, "mode"))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| value == "applet")
        .unwrap_or_else(|| "account".to_string())
}

fn cokret_advanced_enabled(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> bool {
    values
        .get(&field_value_key(channel_id, "advanced"))
        .map(|value| value == "true")
        .unwrap_or(false)
}

fn is_cokret_internal_field(field_key: &str) -> bool {
    matches!(field_key, "deviceId")
}

fn is_cokret_account_only_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "principalId"
            | "defaultRealmId"
            | "defaultFlowId"
            | "agentId"
            | "listen"
            | "send"
            | "scopes"
    )
}

fn is_cokret_applet_only_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "appletId"
            | "controllerDid"
            | "botActorId"
            | "cokretServerUrl"
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

fn is_cokret_helper_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "advanced" | "namespaceActors" | "namespaceRealms" | "namespaceHandles"
    )
}

fn is_cokret_advanced_field(field_key: &str, mode: &str) -> bool {
    matches!(
        field_key,
        "cokretServerDid" | "loginChallenge" | "verificationMethod" | "grantEventPath" | "keyRef"
    ) || (mode != "applet" && matches!(field_key, "serviceDid" | "defaultFlowId" | "scopes"))
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
            ))
}

fn should_skip_hidden_cokret_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "deviceId" | "receiveEvents" | "receiveEphemeral" | "rateLimited" | "ghostDidPrefix"
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
    if channel_id == "cokret" {
        let cokret_mode = current_cokret_mode(channel_id, values);
        if is_cokret_internal_field(&field.key) {
            return false;
        }
        if is_cokret_account_only_field(&field.key) && cokret_mode == "applet" {
            return false;
        }
        if is_cokret_applet_only_field(&field.key) && cokret_mode != "applet" {
            return false;
        }
        if is_cokret_advanced_field(&field.key, &cokret_mode) {
            return cokret_advanced_enabled(channel_id, values);
        }
    }
    true
}

fn build_channel_patch(
    channel_id: &str,
    fields: &[ConfigField],
    values: &std::collections::HashMap<String, String>,
) -> Result<Value, String> {
    if is_cokret_channel(fields) {
        return build_cokret_channel_patch(channel_id, fields, values);
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

fn build_cokret_channel_patch(
    channel_id: &str,
    fields: &[ConfigField],
    values: &std::collections::HashMap<String, String>,
) -> Result<Value, String> {
    let mut patch = json!({});
    let mode = current_cokret_mode(channel_id, values);
    patch["mode"] = json!(mode);

    for field in fields {
        if field.key == "mode" {
            continue;
        }

        if is_cokret_internal_field(&field.key) {
            continue;
        }

        if !field_is_visible(channel_id, field, values) {
            if !is_cokret_helper_field(&field.key) && !should_skip_hidden_cokret_field(&field.key) {
                patch[&field.key] = Value::Null;
            }
            continue;
        }

        if is_cokret_helper_field(&field.key) {
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
                let parsed = parse_json_config_field("Key Ref JSON", value)?;
                if !parsed.is_object() {
                    return Err("Key Ref JSON must be a JSON object.".to_string());
                }
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
    }

    Ok(patch)
}

fn default_channel_values(
    channel_id: &str,
    fields: &[ConfigField],
) -> std::collections::HashMap<String, String> {
    let mut values = std::collections::HashMap::new();
    if is_cokret_channel(fields) {
        values.insert(field_value_key(channel_id, "mode"), "account".to_string());
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
    let mut modal_revealed: Signal<std::collections::HashSet<String>> =
        use_signal(|| std::collections::HashSet::new());

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

    let ws_config_get = ws.clone();
    let modal_channel_types = channel_types;
    use_effect(move || {
        let modal_open = show_add_modal();
        let selected = selected_channel();
        if !modal_open {
            add_channel_name.set(String::new());
            modal_revealed.write().clear();
            return;
        }

        let Some(channel_id) = selected else {
            config_values.write().clear();
            add_channel_name.set(String::new());
            modal_revealed.write().clear();
            return;
        };
        let default_name = modal_channel_types
            .iter()
            .find(|channel_type| channel_type.id == channel_id)
            .map(|channel_type| channel_type.name.clone())
            .unwrap_or_else(|| channel_id.clone());
        let selected_fields = modal_channel_types
            .iter()
            .find(|channel_type| channel_type.id == channel_id)
            .map(|channel_type| channel_type.config_fields.clone())
            .unwrap_or_default();
        add_channel_name.set(default_name);
        config_values.set(default_channel_values(&channel_id, &selected_fields));

        let ws = ws_config_get.clone();
        spawn(async move {
            let result = ws
                .call::<serde_json::Value>(
                    "channels.config.get",
                    Some(json!({ "channel": channel_id })),
                )
                .await;
            let Ok(payload) = result else {
                return;
            };
            let Some(saved) = payload.get("config") else {
                return;
            };
            if saved.is_null() {
                return;
            }

            let mut restored = default_channel_values(&channel_id, &selected_fields);
            restored.extend(restore_channel_values(&channel_id, &selected_fields, saved));
            if let Some(name) = saved.get("name").and_then(Value::as_str) {
                add_channel_name.set(name.to_string());
            }
            config_values.set(restored);
        });
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
    let configs_read = channel_configs_data.read();
    let configs_ref = configs_read.as_ref().and_then(|c| c.as_ref());
    let channel_configs: std::collections::HashMap<String, (String, String, bool)> = configs_ref
        .and_then(|d| {
            d.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        let key = c
                            .get("kind")
                            .and_then(|v| v.as_str())
                            .or_else(|| {
                                c.get("id")
                                    .and_then(|v| v.as_str())
                                    .and_then(|raw| raw.split('-').next())
                            })?
                            .to_ascii_lowercase();
                        let id = c
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&key)
                            .to_string();
                        let name = c
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&key)
                            .to_string();
                        let enabled = c.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                        Some((key, (id, name, enabled)))
                    })
                    .collect()
            })
        })
        .unwrap_or_default();

    let is_loading = channels_data.read().is_none() || channel_configs_data.read().is_none();

    let channels_read = channels_data.read();
    let channels_status = channels_read.as_ref().and_then(|c| c.as_ref());

    // Health counts
    let (connected_count, running_count, disconnected_count, error_count) =
        compute_health_counts(channels_status);

    let status_channels = channels_status
        .and_then(|s| s.get("channels"))
        .and_then(|c| c.as_object());
    let configured_channel_types: Vec<&ChannelTypeInfo> = channel_types
        .iter()
        .filter(|ch_type| {
            let has_saved = channel_configs.contains_key(ch_type.id.as_str());
            let Some(channel) =
                status_channels.and_then(|channels| channels.get(ch_type.id.as_str()))
            else {
                return has_saved;
            };
            let is_configured = channel
                .get("configured")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let is_running = channel
                .get("running")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let is_connected = channel
                .get("connected")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            is_configured || is_running || is_connected || has_saved
        })
        .collect();

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
            } else if configured_channel_types.is_empty() {
                div { class: "channels-empty", "No channels configured yet. Click + Add Channel to configure one." }
            } else {
                div { class: "channels-grid",
                    for ch_type in configured_channel_types.into_iter() {
                        { render_channel_card(ch_type, channels_status, ws.clone(), refresh_tick, show_raw_json, testing_channel, test_result, &channel_configs, config_modal_channel) }
                    }
                }
            }
        }

        // ---- Raw JSON / Health modal ----
        if let Some(ref ch_id) = show_raw_json() {
            {
                let channel_data = channels_status
                    .and_then(|s| s.get("channels"))
                    .and_then(|c| c.get(ch_id.as_str()));
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

                let reconnect_ch_id = ch_id.clone();
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
                                        if let Some((cfg_id, cfg_name, _)) = channel_configs.get(ch_id) {
                                            span { style: "font-size:13px;font-weight:400;color:var(--text-muted);margin-left:8px;",
                                                "({cfg_name}"
                                                span { style: "margin-left:4px;font-size:11px;color:var(--text-muted);opacity:0.7;", "{cfg_id}" }
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
        if let Some(ref modal_ch_id) = config_modal_channel() {
            {
                let ch_type = channel_types.iter().find(|ct| ct.id == *modal_ch_id);
                if let Some(ct) = ch_type {
                    rsx! {
                        ChannelConfigModal {
                            channel_id: ct.id.clone(),
                            channel_name: ct.name.clone(),
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
    channels_status: Option<&serde_json::Value>,
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    mut show_raw_json: Signal<Option<String>>,
    mut testing_channel: Signal<Option<String>>,
    mut test_result: Signal<Option<(String, bool, String)>>,
    channel_configs: &std::collections::HashMap<String, (String, String, bool)>,
    mut config_modal_channel: Signal<Option<String>>,
) -> Element {
    let channel_data = channels_status
        .and_then(|s| s.get("channels"))
        .and_then(|c| c.get(&ch_type.id));

    let status_configured = channel_data
        .and_then(|c| c.get("configured"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let has_saved_config = channel_configs.contains_key(&ch_type.id);
    let is_configured = status_configured || has_saved_config;

    let is_running = channel_data
        .and_then(|c| c.get("running"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let is_connected = channel_data
        .and_then(|c| c.get("connected"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let config_entry = channel_configs.get(&ch_type.id);
    let is_enabled = config_entry
        .map(|(_, _, enabled)| *enabled)
        .unwrap_or(is_running || is_connected);
    let config_id = config_entry.map(|(id, ..)| id.as_str());
    let config_name = config_entry.map(|(_, name, _)| name.as_str());

    let last_error = channel_data
        .and_then(|c| c.get("lastError"))
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

    let (status_variant, status_text) = if is_running && is_connected {
        (ChipVariant::Success, "Connected")
    } else if is_running {
        (ChipVariant::Warning, "Running")
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
    let platform_health = ch_type.id.clone();
    let platform_test = ch_type.id.clone();
    let platform_test_name = ch_type.name.clone();
    let ws_login = ws.clone();
    let ws_test = ws.clone();

    let is_testing = testing_channel().as_deref() == Some(ch_type.id.as_str());
    let platform_config = ch_type.id.clone();

    rsx! {
        div {
            key: "{ch_type.id}",
            class: "{border_class}",

            // ---- Card header ----
            div { class: "channels-card__header",
                div { class: "channels-card__identity",
                    span { class: "channels-card__icon", "{ch_type.icon}" }
                    div { class: "channels-card__meta",
                        div { class: "channels-card__name", "{ch_type.name}" }
                        div { class: "channels-card__desc", "{ch_type.description}" }
                        if let Some(cfg_id) = config_id {
                            div { class: "channels-card__config-id",
                                span { class: "channels-card__config-id-label", "id" }
                                " {cfg_id}"
                                if let Some(cfg_name) = config_name {
                                    span { class: "channels-card__config-id-sep", " / " }
                                    span { class: "channels-card__config-name", "{cfg_name}" }
                                }
                            }
                        }
                    }
                }
                div { class: "channels-card__status",
                    Chip { label: status_text.to_string(), variant: status_variant }
                }
            }

            // ---- Platform-specific info ----
            {
                let has_platform_info = bot_username.is_some() || guild_count.is_some()
                    || matrix_user_id.is_some() || room_count.is_some()
                    || nostr_public_key.is_some() || relay_count.is_some();
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
                        let new_enabled = !is_enabled;
                        spawn(async move {
                            let _ = ws.call::<serde_json::Value>(
                                "channels.config.save",
                                Some(json!({
                                    "channel": platform,
                                    "config": { "enabled": new_enabled },
                                })),
                            ).await;
                            if new_enabled {
                                let _ = ws.call::<serde_json::Value>(
                                    "channels.login",
                                    Some(json!({ "platform": platform })),
                                ).await;
                            } else {
                                let _ = ws.call::<serde_json::Value>(
                                    "channels.logout",
                                    Some(json!({ "platform": platform })),
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

                // Test button (TASK-014: enhanced with loading spinner and inline result)
                button {
                    onclick: move |_| {
                        let ws = ws_test.clone();
                        let platform = platform_test.clone();
                        let name = platform_test_name.clone();
                        testing_channel.set(Some(platform.clone()));
                        test_result.set(None);
                        spawn(async move {
                            let result = ws.call::<serde_json::Value>(
                                "channels.test",
                                Some(json!({ "platform": platform })),
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

                // Inline test result for this card (TASK-014)
                {
                    let card_ch_id = ch_type.id.clone();
                    let card_result = test_result().and_then(|(ref id, ok, ref msg)| {
                        if id == &ch_type.name || id == &card_ch_id {
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
                                if success {
                                    "Connection successful"
                                } else {
                                    "{msg}"
                                }
                            }
                        }
                    } else {
                        rsx! { span { style: "display:none;" } }
                    }
                }

                // Configure
                button {
                    onclick: move |_| config_modal_channel.set(Some(platform_config.clone())),
                    class: "channels-action-btn",
                    title: "Channel settings",
                    "Settings"
                }

                // Health JSON
                button {
                    onclick: move |_| show_raw_json.set(Some(platform_health.clone())),
                    class: "channels-action-btn channels-action-btn--muted",
                    title: "View raw health data",
                    "JSON"
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
    let mut inline_msg = use_signal(|| Option::<(bool, String)>::None);
    let mut revealed: Signal<std::collections::HashSet<String>> =
        use_signal(|| std::collections::HashSet::new());

    let ch_id = channel_id;
    let ch_name = channel_name;
    let fields_vec = fields;
    let ws_save = ws.clone();
    let ws_test = ws.clone();
    let ws_load = ws.clone();
    let load_ch_id = ch_id.clone();
    let load_fields = fields_vec.clone();

    use_effect(move || {
        let ws = ws_load.clone();
        let ch_id = load_ch_id.clone();
        let fields = load_fields.clone();
        spawn(async move {
            let result = ws
                .call::<serde_json::Value>("channels.config.get", Some(json!({ "channel": ch_id })))
                .await;
            let Ok(payload) = result else {
                return;
            };
            let Some(saved) = payload.get("config") else {
                return;
            };
            let mut restored = default_channel_values(&ch_id, &fields);
            restored.extend(restore_channel_values(&ch_id, &fields, saved));
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
                    let computed_id = compute_channel_id(&current_name, &ch_id);
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
                                value: "{computed_id}",
                                readonly: true,
                                class: "channels-field__input channels-field__input--readonly",
                            }
                        }
                    }
                }

                // Render fields
                { render_config_fields(&ch_id, &fields_vec, inline_values, revealed) }

                // Discord callback URL section
                if ch_id == "discord" {
                    { render_discord_callback_url() }
                }

                { render_router_fields(&ch_id, inline_values) }
                { render_policy_fields(&ch_id, inline_values) }

                // Action row
                div { class: "channels-cfg__actions",
                    // Save button
                    button {
                        onclick: {
                            let ws = ws_save.clone();
                            let ch_id = ch_id.clone();
                            let fields = fields_vec.clone();
                            move |_| {
                                let ws = ws.clone();
                                let ch_id = ch_id.clone();
                                let fields = fields.clone();
                                let vals = inline_values.read();
                                let channel_name = vals
                                    .get(&channel_name_key(&ch_id))
                                    .filter(|v| !v.trim().is_empty())
                                    .cloned()
                                    .unwrap_or_else(|| "default".to_string());
                                let channel_computed_id = compute_channel_id(&channel_name, &ch_id);
                                let patch = match build_channel_patch(&ch_id, &fields, &vals) {
                                    Ok(patch) => patch,
                                    Err(err) => {
                                        inline_msg.set(Some((false, err)));
                                        return;
                                    }
                                };
                                let router = match build_router_value(&ch_id, &vals) {
                                    Ok(router) => router,
                                    Err(err) => {
                                        inline_msg.set(Some((false, err)));
                                        return;
                                    }
                                };
                                let dm_policy = build_dm_policy(&ch_id, &vals);
                                let group_policy = build_group_policy(&ch_id, &vals);
                                let persist_patch = patch.clone();
                                let persist_router = router.clone();
                                let persist_channel = ch_id.clone();

                                inline_saving.set(true);
                                spawn(async move {
                                    let persist_result = ws
                                        .call::<serde_json::Value>(
                                            "channels.config.save",
                                            Some(json!({
                                                "channel": persist_channel,
                                                "id": channel_computed_id,
                                                "name": channel_name,
                                                "config": persist_patch,
                                                "router": persist_router,
                                                "dm_policy": dm_policy,
                                                "group_policy": group_policy,
                                            })),
                                        )
                                        .await;
                                    match persist_result {
                                        Ok(_) => {
                                            inline_saving.set(false);
                                            inline_msg.set(Some((
                                                true,
                                                "Configuration saved.".into(),
                                            )));
                                            refresh_tick += 1;
                                        }
                                        Err(e) => {
                                            inline_saving.set(false);
                                            inline_msg.set(Some((false, format!("Save failed: {e}"))));
                                        }
                                    }
                                });
                            }
                        },
                        disabled: inline_saving(),
                        class: "channels-action-btn channels-action-btn--primary",
                        if inline_saving() { "Saving..." } else { "Save" }
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
                                            let channel_computed_id = compute_channel_id(&channel_name, &platform);
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
                                if let Some(window) = web_sys::window() {
                                    if let Ok(nav) = js_sys::Reflect::get(
                                        &window,
                                        &wasm_bindgen::JsValue::from_str("navigator"),
                                    ) {
                                        if let Ok(clipboard) = js_sys::Reflect::get(
                                            &nav,
                                            &wasm_bindgen::JsValue::from_str("clipboard"),
                                        ) {
                                            if let Ok(write_fn) = js_sys::Reflect::get(
                                                &clipboard,
                                                &wasm_bindgen::JsValue::from_str("writeText"),
                                            ) {
                                                let func: js_sys::Function = write_fn.into();
                                                if let Ok(p) = func.call1(
                                                    &clipboard,
                                                    &wasm_bindgen::JsValue::from_str(&text),
                                                ) {
                                                    let _ = wasm_bindgen_futures::JsFuture::from(
                                                        js_sys::Promise::from(p),
                                                    )
                                                    .await;
                                                }
                                            }
                                        }
                                    }
                                }
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
) -> Element {
    rsx! {
        div { class: "channels-cfg__fields",
            for field in fields.iter() {
                { render_single_field(ch_id, field, values, revealed) }
            }
        }
    }
}

/// Renders a single configuration field based on its FieldType.
fn render_single_field(
    ch_id: &str,
    field: &ConfigField,
    mut values: Signal<std::collections::HashMap<String, String>>,
    mut revealed: Signal<std::collections::HashSet<String>>,
) -> Element {
    let should_render = {
        let value_map = values.read();
        field_is_visible(ch_id, field, &value_map)
    };
    if !should_render {
        return rsx! {};
    }

    let key = format!("{}.{}", ch_id, field.key);
    let current_val = values.read().get(&key).cloned().unwrap_or_default();
    let key_input = key.clone();
    let key_reveal = key.clone();
    let is_revealed = revealed.read().contains(&key);

    let help_text = field.help;
    let is_required = field.required;

    match &field.field_type {
        FieldType::Text => {
            rsx! {
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label",
                        "{field.label}"
                        if is_required {
                            span { class: "channels-field__required", " *" }
                        }
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.to_string() }
                        }
                    }
                    input {
                        r#type: "text",
                        placeholder: "{field.placeholder}",
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
                        "{field.label}"
                        if is_required {
                            span { class: "channels-field__required", " *" }
                        }
                        span { class: "channels-field__secret-badge", "secret" }
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.to_string() }
                        }
                    }
                    div { class: "channels-cfg__password-wrap",
                        input {
                            r#type: "{input_type}",
                            placeholder: "{field.placeholder}",
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
                        "{field.label}"
                        if is_required {
                            span { class: "channels-field__required", " *" }
                        }
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.to_string() }
                        }
                    }
                    input {
                        r#type: "number",
                        placeholder: "{field.placeholder}",
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
                        "{field.label}"
                        if is_required {
                            span { class: "channels-field__required", " *" }
                        }
                        if secret_badge {
                            span { class: "channels-field__secret-badge", "secret" }
                        }
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.to_string() }
                        }
                    }
                    textarea {
                        placeholder: "{field.placeholder}",
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
                        "{field.label}"
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.to_string() }
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
                        "{field.label}"
                        if is_required {
                            span { class: "channels-field__required", " *" }
                        }
                        if !help_text.is_empty() {
                            HelpTip { text: help_text.to_string() }
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
        "discord", "telegram", "slack", "whatsapp", "signal", "matrix", "cokret",
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
                            rsx! {
                                div { class: "channels-field",
                                    label { class: "channels-field__label", "Name" }
                                    input {
                                        r#type: "text",
                                        placeholder: "My Channel",
                                        value: "{current_name}",
                                        oninput: move |e| channel_name.set(e.value()),
                                        class: "channels-field__input",
                                    }
                                    div { class: "channels-field__hint",
                                        "Channel ID: {id_preview}.json"
                                    }
                                }
                            }
                        }
                        { render_config_fields(&ch_type.id, &ch_type.config_fields, config_values, revealed) }
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
                                                    save_msg.set(Some(
                                                        "Configuration saved successfully!".into(),
                                                    ));
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

    fn cokret_fields() -> Vec<ConfigField> {
        build_channel_types()
            .into_iter()
            .find(|channel| channel.id == "cokret")
            .expect("cokret channel type")
            .config_fields
    }

    #[test]
    fn channel_types_include_cokret() {
        let cokret = build_channel_types()
            .into_iter()
            .find(|channel| channel.id == "cokret")
            .expect("cokret channel type");

        assert_eq!(cokret.name, "Cokret");
        assert!(
            cokret
                .config_fields
                .iter()
                .any(|field| field.key == "keyRef")
        );
        assert!(
            cokret
                .config_fields
                .iter()
                .any(|field| field.key == "trustedVerificationMethods")
        );
        assert!(
            cokret
                .config_fields
                .iter()
                .any(|field| field.key == "advanced")
        );
    }

    #[test]
    fn cokret_simple_account_hides_low_level_fields() {
        let fields = cokret_fields();
        let values = default_channel_values("cokret", &fields);
        let visible = |key: &str| {
            let field = fields.iter().find(|field| field.key == key).expect(key);
            field_is_visible("cokret", field, &values)
        };

        assert!(visible("principalId"));
        assert!(visible("defaultRealmId"));
        assert!(visible("agentId"));
        assert!(visible("advanced"));
        assert!(!visible("deviceId"));
        assert!(!visible("serviceDid"));
        assert!(!visible("keyRef"));
        assert!(!visible("scopes"));
    }

    #[test]
    fn cokret_advanced_account_reveals_auth_fields() {
        let fields = cokret_fields();
        let mut values = default_channel_values("cokret", &fields);
        values.insert(field_value_key("cokret", "advanced"), "true".to_owned());
        let visible = |key: &str| {
            let field = fields.iter().find(|field| field.key == key).expect(key);
            field_is_visible("cokret", field, &values)
        };

        assert!(visible("serviceDid"));
        assert!(visible("keyRef"));
        assert!(visible("loginChallenge"));
        assert!(visible("grantEventPath"));
        assert!(visible("scopes"));
        assert!(!visible("deviceId"));
    }

    #[test]
    fn cokret_saved_advanced_config_restores_advanced_toggle() {
        let fields = cokret_fields();
        let saved = json!({
            "name": "Cokret",
            "config": {
                "mode": "account",
                "baseUrl": "https://cokret.example.org",
                "principalId": "did:webvh:example.org:agents:support",
                "accessToken": "token-1",
                "defaultRealmId": "ck:realm:abc",
                "keyRef": { "kind": "env", "var": "SAVFOX_COKRET_BOT_KEY" },
                "loginChallenge": "challenge-from-cokret"
            }
        });

        let restored = restore_channel_values("cokret", &fields, &saved);

        assert_eq!(
            restored.get(&field_value_key("cokret", "advanced")),
            Some(&"true".to_owned())
        );
        assert_eq!(
            restored.get(&field_value_key("cokret", "keyRef")),
            Some(&"{\n  \"kind\": \"env\",\n  \"var\": \"SAVFOX_COKRET_BOT_KEY\"\n}".to_owned())
        );
    }

    #[test]
    fn cokret_account_patch_builds_flat_account_config() {
        let fields = cokret_fields();
        let mut values = default_channel_values("cokret", &fields);
        values.insert(
            field_value_key("cokret", "baseUrl"),
            "https://cokret.example.org".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "serviceDid"),
            "did:webvh:cokret.example.org".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "principalId"),
            "did:webvh:example.org:agents:support".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "deviceId"),
            "ck:device:01904100-0000-7000-8000-000000000001".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "accessToken"),
            "token-1".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "defaultRealmId"),
            "ck:realm:abc".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "scopes"),
            "ck.message.create\nck.message.read".to_owned(),
        );

        let patch = build_channel_patch("cokret", &fields, &values).expect("patch");

        assert_eq!(patch["mode"], json!("account"));
        assert_eq!(patch["listen"], json!(true));
        assert_eq!(patch["send"], json!(true));
        assert!(patch["deviceId"].is_null());
        assert!(patch["serviceDid"].is_null());
        assert!(patch["scopes"].is_null());
        assert_eq!(
            patch["principalId"],
            json!("did:webvh:example.org:agents:support")
        );
        assert_eq!(patch["defaultRealmId"], json!("ck:realm:abc"));
        assert!(patch["appletId"].is_null());
        assert!(patch["namespaces"].is_null());
    }

    #[test]
    fn cokret_applet_patch_builds_structured_namespaces() {
        let fields = cokret_fields();
        let mut values = default_channel_values("cokret", &fields);
        values.insert(field_value_key("cokret", "mode"), "applet".to_owned());
        values.insert(
            field_value_key("cokret", "baseUrl"),
            "https://savfox.example/appservices/cokret/cokret-default".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "serviceDid"),
            "did:web:slack-bridge.example".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "accessToken"),
            "applet-bearer-1".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "appletId"),
            "ck:applet:21532600-0000-7000-8000-000000000000".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "controllerDid"),
            "did:webvh:example.com:admin".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "cokretServerUrl"),
            "https://cokret.example.org".to_owned(),
        );
        values.insert(field_value_key("cokret", "protocols"), "slack".to_owned());
        values.insert(
            field_value_key("cokret", "namespaceActors"),
            "did:web:slack-bridge.example:ghost:*".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "namespaceRealms"),
            "slack:team:*:channel:*".to_owned(),
        );
        values.insert(
            field_value_key("cokret", "namespaceHandles"),
            "slack.acme.example/*".to_owned(),
        );

        let patch = build_channel_patch("cokret", &fields, &values).expect("patch");

        assert_eq!(patch["mode"], json!("applet"));
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

    .channels-card__status {
        display: flex;
        align-items: center;
        gap: 8px;
        flex-shrink: 0;
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
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 20px;
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
        display: flex;
        gap: 8px;
        margin-top: 20px;
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
        display: flex;
        gap: 8px;
        margin-top: 8px;
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
    }
"#;
