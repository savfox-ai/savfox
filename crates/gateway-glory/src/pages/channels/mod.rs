//! Channel configuration, runtime health and Arkret pairing in Glory + Sapid.
mod view;
use std::collections::HashMap;

use arkret_models_collaboration::agent_operations::AgentPairingBootstrap;
use glory::prelude::{col, each, row, text, when};
use glory::{Cage, Scope, Widget};
use savfox_gateway_shared::CHANNEL_SECRET_REDACTION_PLACEHOLDER;
use savfox_utils::string::normalize_slug;
use serde_json::{Value, json};
pub(crate) use view::render;

use crate::api::ws::WsRpc;
use crate::ui::{self, AppContext};
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
                    key: "deliveryMode".into(),
                    label: "Delivery mode".into(),
                    field_type: FieldType::Select(vec![
                        "interactive_chat".into(),
                        "task_delivery".into(),
                    ]),
                    placeholder: "interactive_chat".into(),
                    secret: false,
                    required: false,
                    help: "Interactive chat sends every assistant reply and is the default. Select task delivery to publish only explicit checkpoints.",
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
                    key: "controllerAccountId".into(),
                    label: "Controller Account ID".into(),
                    field_type: FieldType::Textarea,
                    placeholder: r#"{"principal_id":"ak:did_core:webvh:zControllerScid","station_id":"ak:did_core:web:station.example"}"#.into(),
                    secret: false,
                    required: true,
                    help: "Agent mode: exact principal-plus-Station AccountId of the controller that owns this Agent. Sidecar authority checks compare the complete account identity.",
                },
                ConfigField {
                    key: "controllerPrincipalId".into(),
                    label: "Controller Principal ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "ak:did_core:webvh:zControllerScid".into(),
                    secret: false,
                    required: true,
                    help: "Applet mode: stable principal identity core that owns or signs the applet registration. It must not be the applet service identity.",
                },
                ConfigField {
                    key: "service_did".into(), label: "Applet Service DID".into(),
                    field_type: FieldType::Text, placeholder: r#"did:webvh:zServiceScid:service.example"#.into(),
                    secret: false, required: true, help: "Resolvable DID for the registered Applet service identity.",
                },
                ConfigField {
                    key: "trust_domain".into(), label: "Trust Domain".into(),
                    field_type: FieldType::Text, placeholder: r#"ak:trust_domain:example.net"#.into(),
                    secret: false, required: true, help: "Trust domain from the installed Applet configuration.",
                },
                ConfigField {
                    key: "managed_actor_authoring".into(), label: "Managed Actor Custody".into(),
                    field_type: FieldType::Textarea, placeholder: r#"{"principal_endpoint":"https://actors.example","key_encryption_key_hex":"..."}"#.into(),
                    secret: true, required: true, help: "Existing managed identity endpoint and encryption key configuration.",
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
                    key: "verificationMethod".into(),
                    label: "Runtime key DID URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "did:web:agent.example#key-1".into(),
                    secret: false,
                    required: false,
                    help: "Complete did:*#fragment for this runtime key. Its DID controller must project to the stable Agent ID shown by the Inkson pairing; Savfox never constructs it from an Agent ID or device ID.",
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
        "migration_required" => "Re-provision required".to_owned(),
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
    if bootstrap.pairing_code.len() != 8
        || !bootstrap
            .pairing_code
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        return Err("Inkson pairing code must contain exactly eight digits.".to_owned());
    }
    Ok(bootstrap)
}

fn field_value_key(channel_id: &str, field_key: &str) -> String {
    format!("{channel_id}.{field_key}")
}

fn channel_name_key(channel_id: &str) -> String {
    format!("{channel_id}.__name")
}

fn arkret_pairing_state_key(channel_id: &str) -> String {
    format!("{channel_id}.__pairing_state")
}

fn saved_channel_id_key(channel_id: &str) -> String {
    format!("{channel_id}.__saved_channel_id")
}

fn saved_arkret_binding_key(channel_id: &str) -> String {
    format!("{channel_id}.__saved_arkret_binding")
}

fn arkret_unbind_confirmed(result: &Value) -> bool {
    result.get("unbound").and_then(Value::as_bool) == Some(true)
}

fn remember_arkret_scope_candidate(
    channel_id: &str,
    patch: &Value,
    snapshot: &mut std::collections::HashMap<String, String>,
    values: &mut std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let scope = patch
        .get("requestedScope")
        .ok_or_else(|| "Pairing candidate has no requestedScope".to_owned())?;
    let actions: Vec<String> = serde_json::from_value(scope.clone())
        .map_err(|_| "Pairing requestedScope must be a string array".to_owned())?;
    savfox_gateway_shared::arkret::validate_agent_runtime_scope(&actions)?;
    let key = field_value_key(channel_id, "requestedScope");
    for source in [&*snapshot, &*values] {
        if let Some(existing) = source.get(&key) {
            let existing: Value = serde_json::from_str(existing)
                .map_err(|_| "Pairing scope changed during request preparation".to_owned())?;
            if existing != *scope {
                return Err("Pairing scope changed during request preparation; retry with the exact candidate".to_owned());
            }
        }
    }
    let serialized = scope.to_string();
    snapshot.insert(key.clone(), serialized.clone());
    values.insert(key, serialized);
    Ok(())
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
    let has_binding = [
        "inksonBootstrap",
        "keyRef",
        "verificationMethod",
        "authorizedEventRef",
        "signerResolutionEvidenceRef",
        "currentSignerEvidence",
    ]
    .iter()
    .any(|key| config_obj.get(*key).is_some_and(|value| !value.is_null()));
    restored.insert(
        saved_arkret_binding_key(channel_id),
        has_binding.to_string(),
    );
    let namespaces = config_obj.get("namespaces").and_then(Value::as_object);
    if let Some(namespaces) = namespaces {
        let realms = namespace_patterns_to_text(namespaces.get("realms"));
        if !realms.is_empty() {
            restored.insert(field_value_key(channel_id, "namespaceRealms"), realms);
        }
        let handles = namespace_patterns_to_text(namespaces.get("handles"));
        if !handles.is_empty() {
            restored.insert(field_value_key(channel_id, "namespaceHandles"), handles);
        }
    }

    for key in [
        "inksonBootstrap",
        "keyRef",
        "trustedVerificationMethods",
        "managed_actor_authoring",
        "requestedScope",
        "signerResolutionEvidenceRef",
        "currentSignerEvidence",
    ] {
        if let Some(raw) = config_obj.get(key) {
            if key == "requestedScope" && raw.is_null() && !has_binding {
                restored.remove(&field_value_key(channel_id, key));
                continue;
            }
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
        "arkretServerDid",
        "requestedScopes",
        "receiveEvents",
        "receiveEphemeral",
        "rateLimited",
        "authorizationGrantId",
        "registrationEpoch",
        "trustedVerificationMethods",
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
            ("serviceId", "applet") => return "ak:did_core:webvh:zServiceScid".to_string(),
            ("serviceId", _) => return "ak:did_core:web:arkret.example.org".to_string(),
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
                return "Public HTTPS Applet address for discovery, identity authoring and completion. Group content requires an accepted Bot or Ghost Device."
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
        if field.key == "serviceId" || field.key == "controllerPrincipalId" {
            return mode == "applet";
        }
        if field.key == "controllerAccountId" {
            return mode != "applet";
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
            | "controllerAccountId"
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
            | "deliveryMode"
            | "unbind"
    )
}

fn is_arkret_agent_hidden_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "advanced"
            | "controllerAccountId"
            | "verificationMethod"
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
            | "authorizedEventRef"
            | "keyRef"
            | "grantEventPath"
    )
}

fn is_arkret_applet_only_field(field_key: &str) -> bool {
    matches!(
        field_key,
        "appletId"
            | "controllerPrincipalId"
            | "service_did"
            | "trust_domain"
            | "managed_actor_authoring"
            | "arkretServerUrl"
            | "protocols"
            | "requestedScopes"
            | "namespaceRealms"
            | "namespaceHandles"
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
                "requestedScopes"
                    | "receiveEvents"
                    | "receiveEphemeral"
                    | "rateLimited"
                    | "authorizationGrantId"
                    | "registrationEpoch"
                    | "trustedVerificationMethods"
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
            "controllerAccountId" => {
                let parsed = parse_json_config_field(&field.label, value)?;
                let account = serde_json::from_value::<arkret_wire::AccountId>(parsed)
                    .map_err(|error| format!("Controller Account ID is invalid: {error}"))?;
                account
                    .validate()
                    .map_err(|error| format!("Controller Account ID is invalid: {error}"))?;
                let parsed = serde_json::to_value(account)
                    .map_err(|error| format!("Controller Account ID is invalid: {error}"))?;
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
                if values.contains_key(&saved_channel_id_key(channel_id))
                    && arkret_agent_is_bound(channel_id, values)
                {
                    // Saved configurations redact the consumed pairing code.
                    // Editing delivery settings must preserve the accepted
                    // runtime identity without starting a new pairing flow.
                    let bootstrap: AgentPairingBootstrap = serde_json::from_value(parsed.clone())
                        .map_err(|error| {
                        format!("Saved Inkson Bootstrap is invalid: {error}")
                    })?;
                    if bootstrap.runtime_identity.is_some() {
                        bootstrap.validated_runtime_identity()?;
                    }
                } else {
                    parse_arkret_agent_pairing_bootstrap(parsed.clone())?;
                }
                patch[&field.key] = parsed;
            }
            "managed_actor_authoring" => {
                let parsed = parse_json_config_field("Managed Actor Custody", value)?;
                if !parsed.is_object() {
                    return Err("Managed Actor Custody must be a JSON object.".into());
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
        let realms = values
            .get(&field_value_key(channel_id, "namespaceRealms"))
            .map(|value| namespace_patterns_from_text(value, true))
            .unwrap_or_default();
        let handles = values
            .get(&field_value_key(channel_id, "namespaceHandles"))
            .map(|value| namespace_patterns_from_text(value, false))
            .unwrap_or_default();
        if realms.is_empty() && handles.is_empty() {
            patch["namespaces"] = Value::Null;
        } else {
            patch["namespaces"] = json!({
                "realms": realms,
                "handles": handles,
            });
        }
    } else {
        patch["namespaces"] = Value::Null;
        clear_arkret_agent_obsolete_fields(&mut patch);
        apply_arkret_hidden_agent_runtime_values(channel_id, values, &mut patch)?;
        if values.contains_key(&saved_channel_id_key(channel_id))
            && values
                .get(&saved_arkret_binding_key(channel_id))
                .is_none_or(|value| value != "false")
            && patch_value_empty(patch.get("requestedScope"))
        {
            return Err("Saved Arkret pairing has no requestedScope; recover its exact scope before editing.".to_owned());
        }
        apply_arkret_bootstrap_defaults(&mut patch)?;
        validate_arkret_agent_runtime_request_inputs(
            &patch,
            values.contains_key(&saved_channel_id_key(channel_id))
                && arkret_agent_is_bound(channel_id, values),
        )?;
    }

    Ok(patch)
}

fn apply_arkret_hidden_agent_runtime_values(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
    patch: &mut Value,
) -> Result<(), String> {
    if let Some(value) = values
        .get(&field_value_key(channel_id, "controllerAccountId"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        let account: arkret_wire::AccountId = serde_json::from_str(value)
            .map_err(|error| format!("Controller Account ID is invalid: {error}"))?;
        account
            .validate()
            .map_err(|error| format!("Controller Account ID is invalid: {error}"))?;
        patch["controllerAccountId"] = serde_json::to_value(account)
            .map_err(|error| format!("Controller Account ID is invalid: {error}"))?;
    }
    if let Some(value) = values.get(&field_value_key(channel_id, "requestedScope")) {
        let actions: Vec<String> = serde_json::from_str(value).map_err(|_| {
            "Saved Arkret requestedScope must be an exact JSON string array; migrate the pairing instead of rewriting its authorization.".to_owned()
        })?;
        savfox_gateway_shared::arkret::validate_agent_runtime_scope(&actions)?;
        if actions.is_empty() {
            return Err(
                "Saved Arkret requestedScope is empty; authorization migration is required."
                    .to_owned(),
            );
        }
        patch["requestedScope"] = json!(actions);
    }
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
    if let Some(value) = values
        .get(&field_value_key(channel_id, "signerResolutionEvidenceRef"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        patch["signerResolutionEvidenceRef"] =
            parse_json_config_field("Signer Resolution Evidence Reference", value)?;
    }
    if let Some(value) = values
        .get(&field_value_key(channel_id, "currentSignerEvidence"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        patch["currentSignerEvidence"] = parse_json_config_field("Current Signer Evidence", value)?;
    }

    Ok(())
}

fn validate_arkret_agent_runtime_request_inputs(
    patch: &Value,
    preserve_saved_key: bool,
) -> Result<(), String> {
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
        .get("controllerAccountId")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(
            "Arkret agent mode requires an exact Controller Account ID object.".to_string(),
        );
    }
    if patch
        .pointer("/keyRef/kind")
        .and_then(Value::as_str)
        .is_none_or(|kind| {
            kind != "keyring"
                && !(preserve_saved_key && kind == CHANNEL_SECRET_REDACTION_PLACEHOLDER)
        })
    {
        return Err(
            "Arkret agent runtime keys must use the platform credential vault (kind='keyring'). Generate a new local runtime key before saving."
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
            "Arkret agent mode needs the complete runtime verification method DID URL (did:*#fragment) for this Agent."
                .to_string(),
        );
    }
    Ok(())
}

fn apply_arkret_bootstrap_defaults(patch: &mut Value) -> Result<(), String> {
    let Some(bootstrap_value) = patch.get("inksonBootstrap").cloned() else {
        return Ok(());
    };
    let Ok(bootstrap) = parse_arkret_agent_pairing_bootstrap(bootstrap_value) else {
        return Ok(());
    };
    if bootstrap.runtime_identity.is_some() {
        let identity = bootstrap.validated_runtime_identity()?;
        if patch_value_empty(patch.get("controllerAccountId")) {
            patch["controllerAccountId"] = json!(identity.controller_account_id);
        }
        if patch_value_empty(patch.get("verificationMethod")) {
            patch["verificationMethod"] = json!(identity.verification_method);
        }
    }

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
        if !patch_value_empty(patch.get("authorizedEventRef")) {
            return Err("Existing Arkret authorization has no saved requestedScope; recover its exact scope or provision a new Agent instead of adding defaults.".to_owned());
        }
        patch["requestedScope"] =
            json!(savfox_gateway_shared::arkret::default_agent_runtime_scope()?);
    }
    let agent_id = bootstrap.agent_id.to_string();
    let existing_verification_method = patch
        .get("verificationMethod")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(verification_method) = existing_verification_method
        && !arkret_verification_method_matches_agent(verification_method, &agent_id)
    {
        return Err(
            "Runtime key DID URL must be a complete did:*#fragment whose controller projects to the paired stable Agent ID."
                .to_owned(),
        );
    }
    Ok(())
}

fn arkret_verification_method_matches_agent(verification_method: &str, agent_id: &str) -> bool {
    savfox_gateway_shared::arkret::agent_verification_method_matches_core_id(
        agent_id,
        verification_method,
    )
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
            field_value_key(channel_id, "deliveryMode"),
            "interactive_chat".to_string(),
        );
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
    }
    values
}

fn arkret_runtime_error_key(reason: Option<&str>) -> &'static str {
    match reason {
        Some("capability_denied") => "arkret_runtime.permissions_denied",
        Some("proof_invalid") => "arkret_runtime.authentication_failed",
        Some("station_identity_changed") => "arkret_runtime.station_identity_changed",
        _ => "arkret_runtime.failed",
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
        .and_then(|value| serde_json::from_value::<AgentPairingBootstrap>(value).ok())?;
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

fn arkret_runtime_key_request_can_request(
    channel_id: &str,
    values: &std::collections::HashMap<String, String>,
) -> bool {
    values
        .get(&field_value_key(channel_id, "inksonBootstrap"))
        .map(|value| value.trim())
        .is_some_and(|value| !value.is_empty())
}

fn apply_arkret_pairing_identity(
    channel_id: &str,
    values: &mut std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let input = values
        .get(&field_value_key(channel_id, "inksonBootstrap"))
        .ok_or_else(|| "Pairing input is required.".to_owned())?;
    let bootstrap = parse_arkret_agent_pairing_bootstrap(
        serde_json::from_str(input)
            .map_err(|error| format!("Invalid pairing bootstrap: {error}"))?,
    )?;
    let identity = bootstrap.validated_runtime_identity()?;
    values.insert(
        field_value_key(channel_id, "controllerAccountId"),
        serde_json::to_string(&identity.controller_account_id)
            .map_err(|error| error.to_string())?,
    );
    values.insert(
        field_value_key(channel_id, "verificationMethod"),
        identity.verification_method.to_string(),
    );
    Ok(())
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

fn finish_arkret_pairing_wait(
    values: &mut std::collections::HashMap<String, String>,
    state_key: &str,
    status_key: &str,
    message: &str,
) {
    values.insert(state_key.to_owned(), "poll_failed".to_owned());
    values.insert(status_key.to_owned(), message.to_owned());
}

/// Build local keyring generation parameters from the validated pairing identity.
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
    let verification_method = bootstrap.as_ref().and_then(|bootstrap| {
        let agent_id = bootstrap.agent_id.to_string();
        values
            .get(&field_value_key(channel_id, "verificationMethod"))
            .map(|value| value.trim())
            .filter(|value| arkret_verification_method_matches_agent(value, &agent_id))
            .map(str::to_owned)
    });

    let mut params = serde_json::Map::new();
    params.insert("platform".to_owned(), json!("arkret"));
    if let Some(principal_id) = principal_id {
        params.insert("agent_id".to_owned(), json!(principal_id));
    }
    if let Some(pairing_request_id) = bootstrap
        .as_ref()
        .map(|bootstrap| bootstrap.pairing_request_id.to_string())
    {
        params.insert("pairing_request_id".to_owned(), json!(pairing_request_id));
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn arkret_runtime_error_summary_uses_structured_reason() {
        assert_eq!(
            arkret_runtime_error_key(Some("capability_denied")),
            "arkret_runtime.permissions_denied"
        );
        assert_eq!(
            arkret_runtime_error_key(Some("proof_invalid")),
            "arkret_runtime.authentication_failed"
        );
        assert_eq!(arkret_runtime_error_key(None), "arkret_runtime.failed");
        assert_eq!(
            arkret_runtime_error_key(Some("station_identity_changed")),
            "arkret_runtime.station_identity_changed"
        );
    }

    #[test]
    fn arkret_agent_pairing_requests_authenticated_signal_presence() {
        let scope = savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap();
        assert!(
            scope
                .iter()
                .any(|action| action == "ak.self.signal.command.send.v1")
        );
    }

    fn agent_scope_form() -> (Vec<ConfigField>, std::collections::HashMap<String, String>) {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        insert_controller_account(&mut values);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            json!({
                "arkret_base_url":"https://arkret.example.org",
                "service_id":"ak:did_core:web:arkret.example.org",
                "agent_id":"ak:did_core:web:agent.example",
                "pairing_request_id":"new-pairing",
                "pairing_code":"01234567",
                "pairing_expires_at":"2026-09-02T00:00:00.000Z"
            })
            .to_string(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            json!({"kind":"keyring", "service":"savfox-arkret", "account":"runtime"}).to_string(),
        );
        values.insert(
            field_value_key("arkret", "verificationMethod"),
            "did:web:agent.example#runtime-1".to_owned(),
        );
        (fields, values)
    }

    #[test]
    fn arkret_agent_scope_new_candidate_uses_registered_floor_without_retired_operations() {
        let (fields, values) = agent_scope_form();
        let patch = build_channel_patch("arkret", &fields, &values).unwrap();
        let expected = savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap();
        assert_eq!(patch["requestedScope"], json!(expected));
        assert!(
            expected
                .iter()
                .any(|action| action == "ak.self.committed_event.stream.subscribe.v1")
        );
        assert!(
            expected
                .iter()
                .any(|action| action == "ak.self.signal.command.send.v1")
        );
        assert!(
            !expected
                .iter()
                .any(|action| action == "ak.self.authorization_leases.command.issue.v1")
        );
    }

    #[test]
    fn arkret_agent_scope_existing_values_keep_exact_order_and_explicit_permissions() {
        let (fields, mut values) = agent_scope_form();
        let mut scope = savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap();
        scope.reverse();
        scope.push("ak.self.committed_event.resource.get.v1".to_owned());
        values.insert(saved_channel_id_key("arkret"), "saved-agent".to_owned());
        values.insert(
            field_value_key("arkret", "requestedScope"),
            json!(scope).to_string(),
        );
        let patch = build_channel_patch("arkret", &fields, &values).unwrap();
        assert_eq!(patch["requestedScope"], json!(scope));
        let restored = restore_channel_values("arkret", &fields, &json!({"config":patch}));
        assert_eq!(
            serde_json::from_str::<Value>(&restored[&field_value_key("arkret", "requestedScope")])
                .unwrap(),
            json!(scope)
        );
    }

    #[test]
    fn arkret_agent_scope_existing_missing_or_old_values_are_not_completed() {
        let (fields, mut values) = agent_scope_form();
        values.insert(saved_channel_id_key("arkret"), "saved-agent".to_owned());
        assert!(
            build_channel_patch("arkret", &fields, &values)
                .unwrap_err()
                .contains("no requestedScope")
        );
        for invalid in [
            json!(["ak.self.events.read.scan"]),
            json!(["ak.self.events.query.scan"]),
            json!(["ak.self.seals.read.frontier.v1"]),
            json!(["ak.self.authorization_leases.command.issue.v1"]),
            json!([]),
        ] {
            values.insert(
                field_value_key("arkret", "requestedScope"),
                invalid.to_string(),
            );
            let original = values.clone();
            assert!(build_channel_patch("arkret", &fields, &values).is_err());
            assert_eq!(values, original);
        }
        let mut incomplete = savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap();
        incomplete.retain(|action| action != "ak.self.committed_event.stream.subscribe.v1");
        values.insert(
            field_value_key("arkret", "requestedScope"),
            json!(incomplete).to_string(),
        );
        assert!(
            build_channel_patch("arkret", &fields, &values)
                .unwrap_err()
                .contains("ak.self.committed_event.stream.subscribe.v1")
        );
    }

    #[test]
    fn arkret_agent_scope_unbound_slot_can_start_a_new_candidate_only_after_confirmation() {
        assert!(!arkret_unbind_confirmed(
            &json!({"ok":true,"unbound":false})
        ));
        assert!(!arkret_unbind_confirmed(&json!({"ok":true})));
        assert!(arkret_unbind_confirmed(&json!({"ok":true,"unbound":true})));
        let (fields, new_values) = agent_scope_form();
        let mut values = restore_channel_values(
            "arkret",
            &fields,
            &json!({"config":{
                "mode":"agent", "inksonBootstrap":null, "keyRef":null,
                "verificationMethod":null, "authorizedEventRef":null, "requestedScope":null
            }}),
        );
        assert!(!values.contains_key(&field_value_key("arkret", "requestedScope")));
        values.insert(
            saved_channel_id_key("arkret"),
            "existing-empty-slot".to_owned(),
        );
        values.extend(new_values);
        let patch = build_channel_patch("arkret", &fields, &values).unwrap();
        assert_eq!(
            patch["requestedScope"],
            json!(savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap())
        );
    }

    #[test]
    fn arkret_agent_scope_approval_finalizes_the_exact_original_candidate() {
        let (fields, mut values) = agent_scope_form();
        let mut snapshot = values.clone();
        let request = build_channel_patch("arkret", &fields, &snapshot).unwrap();
        remember_arkret_scope_candidate("arkret", &request, &mut snapshot, &mut values).unwrap();
        assert_eq!(
            snapshot[&field_value_key("arkret", "requestedScope")],
            values[&field_value_key("arkret", "requestedScope")]
        );
        values.insert(
            field_value_key("arkret", "authorizedEventRef"),
            "accepted-key-authorization".to_owned(),
        );
        let finalized = build_channel_patch("arkret", &fields, &values).unwrap();
        assert_eq!(finalized["requestedScope"], request["requestedScope"]);
        assert_eq!(
            finalized["authorizedEventRef"],
            json!("accepted-key-authorization")
        );
        values.insert(
            field_value_key("arkret", "requestedScope"),
            json!(["ak.event.read"]).to_string(),
        );
        assert!(
            remember_arkret_scope_candidate("arkret", &request, &mut snapshot, &mut values)
                .is_err()
        );
    }

    fn arkret_fields() -> Vec<ConfigField> {
        build_channel_types()
            .into_iter()
            .find(|channel| channel.id == "arkret")
            .expect("arkret channel type")
            .config_fields
    }

    fn insert_controller_account(values: &mut std::collections::HashMap<String, String>) {
        values.insert(
            field_value_key("arkret", "controllerAccountId"),
            json!({
                "principal_id": "ak:did_core:webvh:zControllerScid",
                "station_id": "ak:did_core:web:station.example"
            })
            .to_string(),
        );
    }

    fn sdk_inkson_bootstrap_value() -> Value {
        json!({
            "arkret_base_url": "https://arkret.example.org",
            "service_id": "ak:did_core:web:arkret.example.org",
            "agent_id": "ak:did_core:web:agent.example",
            "pairing_request_id": "pair-123",
            "pairing_code": "01234567",
            "pairing_expires_at": "2026-07-06T12:00:00.000Z"
        })
    }

    fn sdk_inkson_bootstrap_json() -> String {
        serde_json::to_string(&sdk_inkson_bootstrap_value()).expect("bootstrap JSON")
    }

    #[test]
    fn paired_channel_delivery_edit_preserves_identity_with_redacted_pairing_code() {
        let (fields, mut values) = agent_scope_form();
        let mut bootstrap = sdk_inkson_bootstrap_value();
        bootstrap["pairing_code"] = json!(CHANNEL_SECRET_REDACTION_PLACEHOLDER);
        values.insert(
            saved_channel_id_key("arkret"),
            "arkret-aa-runtime".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            json!({
                "kind":CHANNEL_SECRET_REDACTION_PLACEHOLDER,
                "service":CHANNEL_SECRET_REDACTION_PLACEHOLDER,
                "account":CHANNEL_SECRET_REDACTION_PLACEHOLDER
            })
            .to_string(),
        );
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            bootstrap.to_string(),
        );
        values.insert(
            field_value_key("arkret", "authorizedEventRef"),
            "ak:event:AWgGCEbMHnelRQfzqg1C_onV9Ej_FdpdAZyM_JoFgAd3".to_owned(),
        );
        let scope = savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap();
        values.insert(
            field_value_key("arkret", "requestedScope"),
            serde_json::to_string(&scope).unwrap(),
        );
        values.insert(
            field_value_key("arkret", "deliveryMode"),
            "interactive_chat".to_owned(),
        );
        let patch = build_channel_patch("arkret", &fields, &values).unwrap();
        assert_eq!(patch["deliveryMode"], "interactive_chat");
        assert_eq!(patch["requestedScope"], json!(scope));
        assert_eq!(
            patch["authorizedEventRef"],
            values[&field_value_key("arkret", "authorizedEventRef")]
        );
        values.remove(&field_value_key("arkret", "authorizedEventRef"));
        assert!(
            build_channel_patch("arkret", &fields, &values)
                .unwrap_err()
                .contains("eight digits")
        );
    }

    #[test]
    fn arkret_pairing_resolved_identity_prepares_complete_patch_without_manual_fields() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        let mut bootstrap = sdk_inkson_bootstrap_value();
        bootstrap["runtime_identity"] = json!({
            "controller_account_id": {
                "principal_id": "ak:did_core:web:controller.example",
                "station_id": "ak:did_core:web:station.example"
            },
            "verification_method": "did:web:agent.example#runtime-pairing"
        });
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            bootstrap.to_string(),
        );
        apply_arkret_pairing_identity("arkret", &mut values).unwrap();
        values.insert(
            field_value_key("arkret", "keyRef"),
            json!({"kind":"keyring", "service":"savfox-arkret", "account":"runtime"}).to_string(),
        );
        let patch = build_channel_patch("arkret", &fields, &values).unwrap();
        assert_eq!(
            patch["controllerAccountId"],
            bootstrap["runtime_identity"]["controller_account_id"]
        );
        assert_eq!(
            patch["verificationMethod"],
            "did:web:agent.example#runtime-pairing"
        );
        bootstrap["runtime_identity"]["verification_method"] =
            json!("did:web:other.example#runtime-pairing");
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            bootstrap.to_string(),
        );
        assert!(apply_arkret_pairing_identity("arkret", &mut values).is_err());
    }

    #[test]
    fn arkret_pairing_missing_identity_reports_missing_binding() {
        let mut values = std::collections::HashMap::new();
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        let error = apply_arkret_pairing_identity("arkret", &mut values).unwrap_err();
        assert!(error.contains("did not provide runtime identity"));
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
    fn every_secret_form_field_is_covered_by_the_shared_server_schema() {
        for channel in build_channel_types() {
            for field in channel
                .config_fields
                .into_iter()
                .filter(|field| field.secret)
            {
                assert!(
                    savfox_gateway_shared::channel_config_key_is_sensitive(&field.key),
                    "{}.{} is marked secret in the UI but missing from the shared schema",
                    channel.id,
                    field.key
                );
            }
        }
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
        assert_eq!(format_arkret_pairing_code("00112233"), "0011 2233");
        assert_eq!(
            arkret_pairing_expiry_from_bootstrap_text(&sdk_inkson_bootstrap_json()).as_deref(),
            Some("2026-07-06T12:00:00.000Z")
        );
    }

    #[test]
    fn arkret_pairing_bootstrap_rejects_non_eight_digit_codes() {
        for code in ["1234567", "123456789", "abcd1234", "AAAAAAAAAAAAAAAAAAAAAA"] {
            let mut bootstrap = sdk_inkson_bootstrap_value();
            bootstrap["pairing_code"] = json!(code);
            assert!(parse_arkret_agent_pairing_bootstrap(bootstrap).is_err());
        }
        assert!(parse_arkret_agent_pairing_bootstrap(sdk_inkson_bootstrap_value()).is_ok());
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
            "ak:event:01904100-0000-8000-8000-000000000099".to_owned(),
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

        assert_eq!(summary.0, "ak:did_core:web:agent.example");
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
        assert!(!visible("controllerAccountId"));
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
        insert_controller_account(&mut values);
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
        insert_controller_account(&mut values);
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
    fn arkret_new_channel_replies_by_default_and_preserves_explicit_task_delivery() {
        let fields = arkret_fields();
        let fresh = default_channel_values("arkret", &fields);
        assert_eq!(
            fresh
                .get(&field_value_key("arkret", "deliveryMode"))
                .map(String::as_str),
            Some("interactive_chat")
        );
        let saved = json!({"config": {"deliveryMode": "task_delivery"}});
        let restored = restore_channel_values("arkret", &fields, &saved);
        assert_eq!(
            restored
                .get(&field_value_key("arkret", "deliveryMode"))
                .map(String::as_str),
            Some("task_delivery")
        );
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
        insert_controller_account(&mut values);

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
                "principalId": "ak:did_core:web:agent.example",
                "defaultRealmId": "ak:realm:abc",
                "keyRef": { "kind": "env", "var": "SAVFOX_ARKRET_AGENT_KEY" },
                "authorizedEventRef": "ak:event:01904100-0000-8000-8000-000000000099"
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
    fn arkret_runtime_key_request_can_start_without_internal_identity_fields() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"keyring","service":"savfox-arkret","account":"runtime-support"}"#
                .to_owned(),
        );
        assert!(arkret_runtime_key_request_can_request("arkret", &values));
        values.insert(
            field_value_key("arkret", "verificationMethod"),
            "did:web:agent.example#runtime-1".to_owned(),
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
        assert!(arkret_runtime_key_request_can_request("arkret", &values));
        values.insert(field_value_key("arkret", "inksonBootstrap"), String::new());
        assert!(!arkret_runtime_key_request_can_request("arkret", &values));
    }

    #[test]
    fn arkret_pairing_failure_unlocks_retry_without_discarding_runtime_key() {
        let state_key = arkret_pairing_state_key("arkret");
        let status_key = field_value_key("arkret", "runtimeKeyRequest");
        let key_ref_key = field_value_key("arkret", "keyRef");
        let mut values = std::collections::HashMap::from([
            (state_key.clone(), "waiting".to_owned()),
            (
                field_value_key("arkret", "inksonBootstrap"),
                "pairing-link".to_owned(),
            ),
            (key_ref_key.clone(), "existing-runtime-key".to_owned()),
        ]);
        finish_arkret_pairing_wait(&mut values, &state_key, &status_key, "Status check failed");
        assert_eq!(
            values.get(&state_key).map(String::as_str),
            Some("poll_failed")
        );
        assert!(arkret_runtime_key_request_can_request("arkret", &values));
        assert_eq!(
            values.get(&key_ref_key).map(String::as_str),
            Some("existing-runtime-key")
        );
    }

    #[test]
    fn arkret_runtime_key_ref_generation_params_use_bootstrap_core_and_verified_method() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        insert_controller_account(&mut values);
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
            "did:web:agent.example#runtime-1".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"inline_seed_base64","value":"secret"}"#.to_owned(),
        );

        let params = arkret_runtime_key_ref_generation_params("arkret", &values);

        assert_eq!(params["platform"], "arkret");
        assert_eq!(params["agent_id"], "ak:did_core:web:agent.example");
        assert_eq!(params["pairing_request_id"], "pair-123");
        assert_eq!(
            params["verification_method"],
            "did:web:agent.example#runtime-1"
        );
        assert!(params.get("keyRef").is_none());
        assert!(params.get("value").is_none());
    }

    #[test]
    fn arkret_agent_patch_rejects_missing_key_ref() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        insert_controller_account(&mut values);
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
        insert_controller_account(&mut values);
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
        insert_controller_account(&mut values);
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
            field_value_key("arkret", "verificationMethod"),
            "did:web:agent.example#runtime-1".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"keyring","service":"savfox-arkret","account":"runtime-support"}"#
                .to_owned(),
        );
        values.insert(
            field_value_key("arkret", "authorizationResult"),
            r#"{"authorized_event_ref":"ak:event:01904100-0000-8000-8000-000000000099"}"#
                .to_owned(),
        );
        values.insert(
            field_value_key("arkret", "requestedScope"),
            serde_json::to_string(
                &savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap(),
            )
            .unwrap(),
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
        assert_eq!(
            patch["controllerAccountId"],
            json!({
                "principal_id": "ak:did_core:webvh:zControllerScid",
                "station_id": "ak:did_core:web:station.example"
            })
        );
        assert!(patch["listen"].is_null());
        assert!(patch["send"].is_null());
        assert!(patch["deviceId"].is_null());
        assert!(patch["defaultRealmId"].is_null());
        assert!(patch["agentId"].is_null());
        assert_eq!(
            patch["requestedScope"],
            json!(savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap())
        );
        assert_eq!(
            patch["inksonBootstrap"]["pairing_request_id"],
            json!("pair-123")
        );
        assert_eq!(
            patch["keyRef"],
            json!({
                "kind": "keyring",
                "service": "savfox-arkret",
                "account": "runtime-support"
            })
        );
        assert!(patch["externalAiEndpointConfig"].is_null());
        assert!(patch["runtimeKeyRequest"].is_null());
        assert_eq!(
            patch["authorizedEventRef"],
            json!("ak:event:01904100-0000-8000-8000-000000000099")
        );
        assert!(patch["principalId"].is_null());
        assert!(patch["baseUrl"].is_null());
        assert!(patch["serviceId"].is_null());
        assert!(patch["appletId"].is_null());
        assert!(patch["namespaces"].is_null());
    }

    #[test]
    fn arkret_agent_patch_rejects_principal_only_controller_value() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(
            field_value_key("arkret", "controllerAccountId"),
            json!("ak:did_core:web:example.org:users:alice").to_string(),
        );

        let error = build_channel_patch("arkret", &fields, &values)
            .expect_err("controller authority must carry a complete AccountId");
        assert!(error.contains("invalid"));
    }

    #[test]
    fn arkret_account_patch_ignores_hidden_external_ai_endpoint_config() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        insert_controller_account(&mut values);
        values.insert(field_value_key("arkret", "advanced"), "true".to_owned());
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        values.insert(
            field_value_key("arkret", "verificationMethod"),
            "did:web:agent.example#runtime-1".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"keyring","service":"savfox-arkret","account":"runtime-support"}"#
                .to_owned(),
        );
        values.insert(
            field_value_key("arkret", "externalAiEndpointConfig"),
            r#""not-object""#.to_owned(),
        );

        let patch = build_channel_patch("arkret", &fields, &values).expect("patch");

        assert!(patch["externalAiEndpointConfig"].is_null());
    }

    #[test]
    fn arkret_agent_patch_rejects_legacy_runtime_key_sources() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        insert_controller_account(&mut values);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"env","var":"SAVFOX_ARKRET_AGENT_KEY"}"#.to_owned(),
        );

        let error =
            build_channel_patch("arkret", &fields, &values).expect_err("legacy key must fail");

        assert!(error.contains("kind='keyring'"));
    }

    #[test]
    fn arkret_bootstrap_does_not_manufacture_runtime_verification_method() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        insert_controller_account(&mut values);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"keyring","service":"savfox-arkret","account":"runtime-support"}"#
                .to_owned(),
        );

        let error = build_channel_patch("arkret", &fields, &values)
            .expect_err("bootstrap core id cannot manufacture a complete DID URL");

        assert!(error.contains("complete runtime verification method DID URL"));
    }

    #[test]
    fn arkret_bootstrap_rejects_foreign_verification_method_without_clearing_authorization() {
        let fields = arkret_fields();
        let mut values = default_channel_values("arkret", &fields);
        values.insert(
            field_value_key("arkret", "inksonBootstrap"),
            sdk_inkson_bootstrap_json(),
        );
        values.insert(
            field_value_key("arkret", "keyRef"),
            r#"{"kind":"keyring","service":"savfox-arkret","account":"runtime-support"}"#
                .to_owned(),
        );
        values.insert(
            field_value_key("arkret", "verificationMethod"),
            "did:webvh:example.org:agents:stale#runtime-1".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "authorizedEventRef"),
            "ak:event:01904100-0000-8000-8000-000000000099".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "requestedScope"),
            json!(savfox_gateway_shared::arkret::default_agent_runtime_scope().unwrap())
                .to_string(),
        );

        let error = build_channel_patch("arkret", &fields, &values)
            .expect_err("foreign method must fail closed");

        assert!(error.contains("controller projects to the paired stable Agent ID"));
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
            "ak:did_core:web:slack-bridge.example".to_owned(),
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
            field_value_key("arkret", "controllerPrincipalId"),
            "ak:did_core:webvh:zAdminScid".to_owned(),
        );
        values.insert(
            field_value_key("arkret", "arkretServerUrl"),
            "https://arkret.example.org".to_owned(),
        );
        values.insert(field_value_key("arkret", "protocols"), "slack".to_owned());
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
        assert!(patch.get("accessToken").is_none());
        assert!(patch.get("loginChallenge").is_none());
        assert!(patch.get("ghostDidPrefix").is_none());
        assert!(patch.get("bot_account_id").is_none());
        assert!(patch.get("deviceId").is_none());
        assert!(!fields.iter().any(|field| field.key == "bot_account_id"));
        assert_eq!(
            patch["controllerPrincipalId"],
            json!("ak:did_core:webvh:zAdminScid")
        );
        assert!(patch["receiveEvents"].is_null());
        assert!(patch["principalId"].is_null());
        assert!(patch["namespaces"].get("actors").is_none());
        assert!(!fields.iter().any(|field| field.key == "namespaceActors"));
        assert_eq!(
            patch["namespaces"]["handles"][0],
            json!({
                "pattern": "slack.acme.example/*",
                "exclusive": false
            })
        );
    }
}
