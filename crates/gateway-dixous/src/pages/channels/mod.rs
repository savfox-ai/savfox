pub mod discord;
pub mod feishu;
pub mod google_chat;
pub mod imessage;
pub mod irc;
pub mod line;
pub mod matrix;
pub mod mattermost;
pub mod msteams;
pub mod nostr;
pub mod signal;
pub mod slack;
pub mod telegram;
pub mod whatsapp;

use dioxus::prelude::*;
use serde_json::{Value, json};
use savfox_utils_string::normalize_slug;

use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};

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

fn get_channel_types() -> Vec<ChannelTypeInfo> {
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
                },
                ConfigField {
                    key: "application_id".into(),
                    label: "Application ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "Discord application ID".into(),
                    secret: false,
                },
                ConfigField {
                    key: "guild_id".into(),
                    label: "Guild ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "Discord guild (server) ID".into(),
                    secret: false,
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
                },
                ConfigField {
                    key: "webhook_url".into(),
                    label: "Webhook URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://your-domain.com/webhooks/telegram".into(),
                    secret: false,
                },
                ConfigField {
                    key: "polling".into(),
                    label: "Use Polling".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
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
                },
                ConfigField {
                    key: "signing_secret".into(),
                    label: "Signing Secret".into(),
                    field_type: FieldType::Password,
                    placeholder: "Slack signing secret".into(),
                    secret: true,
                },
                ConfigField {
                    key: "app_id".into(),
                    label: "App ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "Slack app ID".into(),
                    secret: false,
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
                },
                ConfigField {
                    key: "access_token".into(),
                    label: "Access Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "WhatsApp access token".into(),
                    secret: true,
                },
                ConfigField {
                    key: "verify_token".into(),
                    label: "Verify Token".into(),
                    field_type: FieldType::Text,
                    placeholder: "Webhook verification token".into(),
                    secret: false,
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
                },
                ConfigField {
                    key: "private_key".into(),
                    label: "Private Key".into(),
                    field_type: FieldType::Password,
                    placeholder: "nsec... or hex".into(),
                    secret: true,
                },
                ConfigField {
                    key: "relay_urls".into(),
                    label: "Relay URLs".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "wss://relay.damus.io\nwss://nos.lol".into(),
                    secret: false,
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
                },
                ConfigField {
                    key: "device_name".into(),
                    label: "Device Name".into(),
                    field_type: FieldType::Text,
                    placeholder: "savfox-signal".into(),
                    secret: false,
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
                },
                ConfigField {
                    key: "password".into(),
                    label: "Password".into(),
                    field_type: FieldType::Password,
                    placeholder: "BlueBubbles server password".into(),
                    secret: true,
                },
            ],
        },
        ChannelTypeInfo {
            id: "matrix".into(),
            name: "Matrix".into(),
            icon: "M".into(),
            description: "Connect to Matrix homeservers via bot account".into(),
            config_fields: vec![
                ConfigField {
                    key: "homeserver".into(),
                    label: "Homeserver URL".into(),
                    field_type: FieldType::Text,
                    placeholder: "https://matrix.org".into(),
                    secret: false,
                },
                ConfigField {
                    key: "userId".into(),
                    label: "User ID (optional with token)".into(),
                    field_type: FieldType::Text,
                    placeholder: "@bot:matrix.org".into(),
                    secret: false,
                },
                ConfigField {
                    key: "accessToken".into(),
                    label: "Access Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "syt_... (user ID fetched automatically)".into(),
                    secret: true,
                },
                ConfigField {
                    key: "password".into(),
                    label: "Password (alternative to token)".into(),
                    field_type: FieldType::Password,
                    placeholder: "Bot account password".into(),
                    secret: true,
                },
                ConfigField {
                    key: "deviceName".into(),
                    label: "Device Name".into(),
                    field_type: FieldType::Text,
                    placeholder: "Savfox Gateway".into(),
                    secret: false,
                },
                ConfigField {
                    key: "encryption".into(),
                    label: "Enable E2EE".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
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
                },
                ConfigField {
                    key: "dmAllowFrom".into(),
                    label: "DM Allow From (comma-separated)".into(),
                    field_type: FieldType::Text,
                    placeholder: "@user:server.org, @admin:example.com".into(),
                    secret: false,
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
                },
                ConfigField {
                    key: "groups".into(),
                    label: "Allowed Rooms (comma-separated)".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "!roomId:server.org\n#alias:server.org".into(),
                    secret: false,
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
                },
                ConfigField {
                    key: "bot_token".into(),
                    label: "Bot Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "Mattermost bot token".into(),
                    secret: true,
                },
                ConfigField {
                    key: "team_name".into(),
                    label: "Team Name".into(),
                    field_type: FieldType::Text,
                    placeholder: "my-team".into(),
                    secret: false,
                },
            ],
        },
        ChannelTypeInfo {
            id: "googlechat".into(),
            name: "Google Chat".into(),
            icon: "G".into(),
            description: "Connect to Google Chat spaces".into(),
            config_fields: vec![
                ConfigField {
                    key: "service_account_json".into(),
                    label: "Service Account JSON".into(),
                    field_type: FieldType::Textarea,
                    placeholder: "Paste service account JSON here...".into(),
                    secret: true,
                },
                ConfigField {
                    key: "space_id".into(),
                    label: "Space ID".into(),
                    field_type: FieldType::Text,
                    placeholder: "spaces/AAAA...".into(),
                    secret: false,
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
                },
                ConfigField {
                    key: "port".into(),
                    label: "Port".into(),
                    field_type: FieldType::Number,
                    placeholder: "6667".into(),
                    secret: false,
                },
                ConfigField {
                    key: "nick".into(),
                    label: "Nickname".into(),
                    field_type: FieldType::Text,
                    placeholder: "MyBot".into(),
                    secret: false,
                },
                ConfigField {
                    key: "channel".into(),
                    label: "Channel".into(),
                    field_type: FieldType::Text,
                    placeholder: "#channel".into(),
                    secret: false,
                },
                ConfigField {
                    key: "use_tls".into(),
                    label: "Use TLS".into(),
                    field_type: FieldType::Toggle,
                    placeholder: String::new(),
                    secret: false,
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
                },
                ConfigField {
                    key: "channel_secret".into(),
                    label: "Channel Secret".into(),
                    field_type: FieldType::Password,
                    placeholder: "LINE channel secret".into(),
                    secret: true,
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
                },
                ConfigField {
                    key: "secret".into(),
                    label: "Secret".into(),
                    field_type: FieldType::Password,
                    placeholder: "Optional secret for verification".into(),
                    secret: true,
                },
                ConfigField {
                    key: "method".into(),
                    label: "Method".into(),
                    field_type: FieldType::Select(vec!["POST".into(), "GET".into()]),
                    placeholder: String::new(),
                    secret: false,
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
                },
                ConfigField {
                    key: "app_secret".into(),
                    label: "App Secret".into(),
                    field_type: FieldType::Password,
                    placeholder: "Feishu app secret".into(),
                    secret: true,
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
                },
                ConfigField {
                    key: "access_token".into(),
                    label: "Access Token".into(),
                    field_type: FieldType::Password,
                    placeholder: "DingTalk robot access token".into(),
                    secret: true,
                },
                ConfigField {
                    key: "secret".into(),
                    label: "Sign Secret".into(),
                    field_type: FieldType::Password,
                    placeholder: "Optional DingTalk webhook sign secret".into(),
                    secret: true,
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

fn field_value_key(channel_id: &str, field_key: &str) -> String {
    format!("{channel_id}.{field_key}")
}

fn auto_channel_id(kind: &str, name: &str) -> String {
    let kind_slug = normalize_slug(kind).unwrap_or_else(|| "channel".to_string());
    let name_slug = normalize_slug(name).unwrap_or_else(|| kind_slug.clone());
    format!("{kind_slug}-{name_slug}")
}

fn build_channel_patch(
    channel_id: &str,
    fields: &[ConfigField],
    values: &std::collections::HashMap<String, String>,
) -> Value {
    let mut patch = json!({});
    for field in fields {
        let key = field_value_key(channel_id, &field.key);
        if let Some(val) = values.get(&key) {
            if val.is_empty() {
                continue;
            }
            if matches!(field.field_type, FieldType::Toggle) {
                patch[&field.key] = json!(val == "true");
            } else {
                patch[&field.key] = json!(val);
            }
        }
    }
    patch
}

#[component]
pub fn Channels() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut show_add_modal = use_signal(|| false);
    let mut selected_channel = use_signal(|| None::<String>);
    let mut config_values: Signal<std::collections::HashMap<String, String>> =
        use_signal(|| std::collections::HashMap::new());
    let mut saving = use_signal(|| false);
    let mut save_msg = use_signal(|| Option::<String>::None);
    let mut auto_refresh = use_signal(|| false);
    let mut show_raw_json = use_signal(|| Option::<String>::None);
    let mut testing_channel = use_signal(|| Option::<String>::None);
    let mut test_result = use_signal(|| Option::<(String, bool, String)>::None);
    let mut add_channel_search = use_signal(String::new);
    let mut add_channel_name = use_signal(String::new);

    let channel_types = get_channel_types();

    let ws_config_get = ws.clone();
    let modal_channel_types = channel_types.clone();
    use_effect(move || {
        let modal_open = show_add_modal();
        let selected = selected_channel();
        if !modal_open {
            add_channel_name.set(String::new());
            return;
        }

        let Some(channel_id) = selected else {
            config_values.write().clear();
            add_channel_name.set(String::new());
            return;
        };
        let default_name = modal_channel_types
            .iter()
            .find(|channel_type| channel_type.id == channel_id)
            .map(|channel_type| channel_type.name.clone())
            .unwrap_or_else(|| channel_id.clone());
        add_channel_name.set(default_name);

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

            let mut restored = std::collections::HashMap::<String, String>::new();
            if let Some(config_obj) = saved.get("config").and_then(|value| value.as_object()) {
                for (field, field_value) in config_obj {
                    if let Some(text) = value_to_field_text(field_value) {
                        restored.insert(format!("{channel_id}.{field}"), text);
                    }
                }
            }

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
            ws.call::<serde_json::Value>("channels.config.list", None)
                .await
                .ok()
        }
    });
    let configs_read = channel_configs_data.read();
    let configs_ref = configs_read.as_ref().and_then(|c| c.as_ref());
    let channel_configs: std::collections::HashMap<String, String> = configs_ref
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
                        let name = c
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&key)
                            .to_string();
                        Some((key, name))
                    })
                    .collect()
            })
        })
        .unwrap_or_default();

    let is_loading = channels_data.read().is_none();

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
                div { class: "channels-empty", "Loading channels..." }
            } else if configured_channel_types.is_empty() {
                div { class: "channels-empty", "No channels configured yet. Click + Add Channel to configure one." }
            } else {
                div { class: "channels-grid",
                    for ch_type in configured_channel_types.into_iter() {
                        { render_channel_card(ch_type, channels_status, ws.clone(), refresh_tick, show_raw_json, testing_channel, test_result, &channel_configs) }
                    }
                }
            }
        }

        // ---- Raw JSON modal ----
        if let Some(ref ch_id) = show_raw_json() {
            {
                let raw_json = channels_status
                    .and_then(|s| s.get("channels"))
                    .and_then(|c| c.get(ch_id.as_str()))
                    .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                    .unwrap_or_else(|| "No data available".to_string());
                let ch_id_clone = ch_id.clone();
                rsx! {
                    div {
                        class: "channels-modal-backdrop",
                        onclick: move |_| show_raw_json.set(None),
                        div {
                            class: "channels-modal",
                            onclick: |e| e.stop_propagation(),
                            div { class: "channels-modal__header",
                                h3 { class: "channels-modal__title", "Health: {ch_id_clone}" }
                                button {
                                    onclick: move |_| show_raw_json.set(None),
                                    class: "channels-modal__close",
                                    "x"
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
                &channel_types,
                selected_channel,
                config_values,
                saving,
                save_msg,
                ws.clone(),
                show_add_modal,
                refresh_tick,
                add_channel_search,
                add_channel_name,
            ) }
        }

        style { {CHANNELS_STYLES} }
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
    channel_configs: &std::collections::HashMap<String, String>,
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

    let last_error = channel_data
        .and_then(|c| c.get("lastError"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let accounts = channel_data
        .and_then(|c| c.get("accounts"))
        .and_then(|v| v.as_array())
        .cloned();

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
    let reconnect_attempt_count = channel_data
        .and_then(|c| c.get("reconnectAttemptCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let probe_status = channel_data
        .and_then(|c| c.get("probeStatus"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
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

    let (status_variant, status_text) = if is_running && is_connected {
        (ChipVariant::Success, "Connected")
    } else if is_running {
        (ChipVariant::Warning, "Running")
    } else if is_configured {
        (ChipVariant::Warning, "Configured")
    } else {
        (ChipVariant::Muted, "Not configured")
    };

    let border_class = if is_running && is_connected {
        "channels-card channels-card--connected"
    } else if is_running {
        "channels-card channels-card--running"
    } else if last_error.is_some() {
        "channels-card channels-card--error"
    } else {
        "channels-card"
    };

    let platform = ch_type.id.clone();
    let platform_disconnect = ch_type.id.clone();
    let platform_health = ch_type.id.clone();
    let platform_test = ch_type.id.clone();
    let platform_test_name = ch_type.name.clone();
    let ws_login = ws.clone();
    let ws_logout = ws.clone();
    let ws_test = ws.clone();

    let is_testing = testing_channel().as_deref() == Some(ch_type.id.as_str());

    // Clone fields needed for the inline config section
    let config_fields = ch_type.config_fields.clone();
    let config_ch_id = ch_type.id.clone();
    let config_ch_name = ch_type.name.clone();
    let ws_config = ws.clone();

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
                    }
                }
                div { class: "channels-card__status",
                    Chip { label: status_text.to_string(), variant: status_variant }
                }
            }

            // ---- Accounts section ----
            if let Some(ref accts) = accounts {
                if !accts.is_empty() {
                    div { class: "channels-card__accounts",
                        div { class: "channels-card__section-label",
                            "Accounts"
                            span { style: "font-weight:400;color:var(--text-muted);margin-left:6px;", "({accts.len()})" }
                        }
                        for acct in accts.iter() {
                            {
                                let name = acct.get("name").and_then(|v| v.as_str()).unwrap_or("-").to_string();
                                let acct_status = acct.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
                                let acct_enabled = acct.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                                let acct_error = acct.get("error").and_then(|v| v.as_str()).map(|s| s.to_string());
                                let acct_last_activity = acct.get("lastActivity").and_then(|v| v.as_str()).map(|s| s.to_string());
                                let sv = match acct_status {
                                    "active" | "online" => ChipVariant::Success,
                                    "offline" => ChipVariant::Danger,
                                    "error" => ChipVariant::Danger,
                                    "disabled" => ChipVariant::Muted,
                                    _ => ChipVariant::Muted,
                                };
                                let ws_acct = ws.clone();
                                let acct_name_toggle = name.clone();
                                let platform_toggle = ch_type.id.clone();
                                rsx! {
                                    div { class: "channels-card__account-row",
                                        div { class: "channels-card__account-info",
                                            span { class: "channels-card__account-name", "{name}" }
                                            Chip { label: acct_status.to_string(), variant: sv }
                                            if let Some(ref activity) = acct_last_activity {
                                                span { class: "channels-card__account-activity", "{activity}" }
                                            }
                                        }
                                        div { class: "channels-card__account-actions",
                                            button {
                                                class: if acct_enabled { "channels-account-toggle active" } else { "channels-account-toggle" },
                                                title: if acct_enabled { "Disable account" } else { "Enable account" },
                                                onclick: {
                                                    let ws = ws_acct.clone();
                                                    let name = acct_name_toggle.clone();
                                                    let platform = platform_toggle.clone();
                                                    move |_| {
                                                        let ws = ws.clone();
                                                        let name = name.clone();
                                                        let platform = platform.clone();
                                                        let enabled = !acct_enabled;
                                                        spawn(async move {
                                                            let _ = ws.call::<serde_json::Value>(
                                                                "channels.account.update",
                                                                Some(json!({ "platform": platform, "account": name, "enabled": enabled })),
                                                            ).await;
                                                            refresh_tick += 1;
                                                        });
                                                    }
                                                },
                                                if acct_enabled { "On" } else { "Off" }
                                            }
                                        }
                                    }
                                    if let Some(ref err) = acct_error {
                                        div { class: "channels-card__account-error", "{err}" }
                                    }
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
                    span { class: "channels-card__metric-label", "Reconnects" }
                    span { class: "channels-card__metric-value", "{reconnect_attempt_count}" }
                }
                div { class: "channels-card__metric",
                    span { class: "channels-card__metric-label", "Probe" }
                    span { class: "channels-card__metric-value", "{probe_status}" }
                }
                div { class: "channels-card__metric",
                    span { class: "channels-card__metric-label", "Uptime" }
                    span { class: "channels-card__metric-value", "{uptime_str}" }
                }
                div { class: "channels-card__metric",
                    span { class: "channels-card__metric-label", "Error rate" }
                    span { class: "channels-card__metric-value", "{error_rate_str}" }
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
                button {
                    onclick: move |_| {
                        let ws = ws_login.clone();
                        let platform = platform.clone();
                        spawn(async move {
                            let _ = ws.call::<serde_json::Value>(
                                "channels.login",
                                Some(json!({ "platform": platform })),
                            ).await;
                            refresh_tick += 1;
                        });
                    },
                    class: "channels-action-btn",
                    if is_running { "Re-configure" } else { "Enable" }
                }

                // Test button
                button {
                    onclick: move |_| {
                        let ws = ws_test.clone();
                        let platform = platform_test.clone();
                        let name = platform_test_name.clone();
                        testing_channel.set(Some(platform.clone()));
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
                                    (false, format!("{e}"))
                                }
                            };
                            test_result.set(Some((name, ok, msg)));
                        });
                    },
                    disabled: is_testing,
                    class: "channels-action-btn channels-action-btn--test",
                    if is_testing { "Testing..." } else { "Test" }
                }

                // Health JSON
                button {
                    onclick: move |_| show_raw_json.set(Some(platform_health.clone())),
                    class: "channels-action-btn channels-action-btn--muted",
                    title: "View raw health data",
                    "JSON"
                }

                if is_running || is_connected {
                    button {
                        onclick: move |_| {
                            let ws = ws_logout.clone();
                            let platform = platform_disconnect.clone();
                            spawn(async move {
                                let _ = ws.call::<serde_json::Value>(
                                    "channels.logout",
                                    Some(json!({ "platform": platform })),
                                ).await;
                                refresh_tick += 1;
                            });
                        },
                        class: "channels-action-btn channels-action-btn--danger",
                        "Disconnect"
                    }
                }
            }

            // ---- Per-channel configuration (collapsible) ----
            ChannelInlineConfig {
                channel_id: config_ch_id.clone(),
                channel_name: config_ch_name.clone(),
                fields: config_fields.clone(),
                ws: ws_config.clone(),
                refresh_tick,
                testing_channel,
                test_result,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-channel inline configuration section
// ---------------------------------------------------------------------------

/// Dispatches to the appropriate per-channel config renderer wrapped in a
/// collapsible `<details>` element placed below the channel card actions.
#[component]
fn ChannelInlineConfig(
    channel_id: String,
    channel_name: String,
    fields: Vec<ConfigField>,
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    mut testing_channel: Signal<Option<String>>,
    mut test_result: Signal<Option<(String, bool, String)>>,
) -> Element {
    // Each inline config gets its own local state
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
            let Some(config_obj) = saved.get("config").and_then(Value::as_object) else {
                return;
            };

            let mut restored = std::collections::HashMap::<String, String>::new();
            for field in &fields {
                if let Some(raw) = config_obj.get(&field.key)
                    && let Some(text) = value_to_field_text(raw)
                {
                    restored.insert(field_value_key(&ch_id, &field.key), text);
                }
            }
            inline_values.set(restored);
        });
    });

    rsx! {
        details { class: "channels-cfg",
            summary { class: "channels-cfg__summary",
                span { class: "channels-cfg__chevron" }
                span { "Configuration" }
            }
            div { class: "channels-cfg__body",
                // Status message
                if let Some((ok, ref msg)) = inline_msg() {
                    div {
                        class: if ok { "channels-cfg__alert channels-cfg__alert--success" } else { "channels-cfg__alert channels-cfg__alert--error" },
                        "{msg}"
                    }
                }

                // Render fields
                { render_config_fields(&ch_id, &fields_vec, inline_values, revealed) }

                // Action row
                div { class: "channels-cfg__actions",
                    // Save button
                    button {
                        onclick: {
                            let ws = ws_save.clone();
                            let ch_id = ch_id.clone();
                            let ch_name = ch_name.clone();
                            let fields = fields_vec.clone();
                            move |_| {
                                let ws = ws.clone();
                                let ch_id = ch_id.clone();
                                let ch_name = ch_name.clone();
                                let fields = fields.clone();
                                let vals = inline_values.read();
                                let patch = build_channel_patch(&ch_id, &fields, &vals);
                                let persist_patch = patch.clone();
                                let persist_channel = ch_id.clone();
                                let runtime_channel = ch_id.clone();
                                inline_saving.set(true);
                                spawn(async move {
                                    let persist_result = ws
                                        .call::<serde_json::Value>(
                                            "channels.config.save",
                                            Some(json!({
                                                "channel": persist_channel,
                                                "name": ch_name,
                                                "config": persist_patch,
                                            })),
                                        )
                                        .await;
                                    match persist_result {
                                        Ok(_) => {
                                            let runtime_result = ws
                                                .call::<serde_json::Value>(
                                                    "config.patch",
                                                    Some(json!({
                                                        "patch": {
                                                            "gateway": {
                                                                "bridges": {
                                                                    (runtime_channel): patch
                                                                }
                                                            }
                                                        }
                                                    })),
                                                )
                                                .await;
                                            inline_saving.set(false);
                                            match runtime_result {
                                                Ok(_) => {
                                                    inline_msg.set(Some((
                                                        true,
                                                        "Configuration saved.".into(),
                                                    )));
                                                }
                                                Err(e) => {
                                                    inline_msg.set(Some((
                                                        true,
                                                        format!(
                                                            "Saved to channels store, but runtime patch failed: {e}"
                                                        ),
                                                    )));
                                                }
                                            }
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

                    // Test Connection button
                    button {
                        onclick: {
                            let ws = ws_test.clone();
                            let ch_id = ch_id.clone();
                            let ch_name = ch_name.clone();
                            move |_| {
                                let ws = ws.clone();
                                let platform = ch_id.clone();
                                let name = ch_name.clone();
                                testing_channel.set(Some(platform.clone()));
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
                                            (false, format!("{e}"))
                                        }
                                    };
                                    test_result.set(Some((name, ok, msg)));
                                });
                            }
                        },
                        class: "channels-action-btn channels-action-btn--test",
                        "Test Connection"
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
    let key = format!("{}.{}", ch_id, field.key);
    let current_val = values.read().get(&key).cloned().unwrap_or_default();
    let key_input = key.clone();
    let key_reveal = key.clone();
    let is_revealed = revealed.read().contains(&key);

    match &field.field_type {
        FieldType::Text => {
            rsx! {
                div { class: "channels-cfg__field",
                    label { class: "channels-field__label", "{field.label}" }
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
                        span { class: "channels-field__secret-badge", "secret" }
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
                    label { class: "channels-field__label", "{field.label}" }
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
                        if secret_badge {
                            span { class: "channels-field__secret-badge", "secret" }
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
                    label { class: "channels-field__label", "{field.label}" }
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
                    label { class: "channels-field__label", "{field.label}" }
                    select {
                        value: "{current_val}",
                        onchange: move |e| { values.write().insert(key_input.clone(), e.value()); },
                        class: "channels-field__input channels-cfg__select",
                        for opt in options.iter() {
                            option {
                                value: "{opt}",
                                selected: current_val == *opt,
                                "{opt}"
                            }
                        }
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
) -> Element {
    let selected = selected_channel();
    let selected_type = channel_types
        .iter()
        .find(|t| Some(&t.id) == selected.as_ref());

    let search_query = add_channel_search().trim().to_ascii_lowercase();
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

    rsx! {
        div {
            class: "channels-modal-backdrop",
            onclick: move |_| {
                show_add_modal.set(false);
                add_channel_search.set(String::new());
                selected_channel.set(None);
                config_values.write().clear();
                channel_name.set(String::new());
                save_msg.set(None);
            },
            div {
                class: "channels-modal channels-modal--wide",
                onclick: |e| e.stop_propagation(),
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
                                        placeholder: "My Matrix Channel",
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
                        for field in ch_type.config_fields.iter() {
                            {
                                let key = format!("{}.{}", ch_type.id, field.key);
                                let current_val = config_values
                                    .read()
                                    .get(&key)
                                    .cloned()
                                    .unwrap_or_default();
                                let key_clone = key.clone();
                                let input_type = match &field.field_type {
                                    FieldType::Password => "password",
                                    FieldType::Number => "number",
                                    _ => "text",
                                };
                                let is_textarea = matches!(field.field_type, FieldType::Textarea);
                                let is_toggle = matches!(field.field_type, FieldType::Toggle);
                                let is_select = matches!(field.field_type, FieldType::Select(_));
                                let select_options = if let FieldType::Select(ref opts) = field.field_type { opts.clone() } else { vec![] };
                                rsx! {
                                    div { key: "{key}", class: "channels-field",
                                        label { class: "channels-field__label",
                                            "{field.label}"
                                            if field.secret {
                                                span { class: "channels-field__secret-badge", "secret" }
                                            }
                                        }
                                        if is_textarea {
                                            textarea {
                                                placeholder: "{field.placeholder}",
                                                value: "{current_val}",
                                                oninput: move |e| {
                                                    config_values
                                                        .write()
                                                        .insert(key_clone.clone(), e.value());
                                                },
                                                class: "channels-field__input channels-cfg__textarea",
                                                rows: "6",
                                            }
                                        } else if is_toggle {
                                            {
                                                let is_on = current_val == "true";
                                                let key_t = key.clone();
                                                rsx! {
                                                    button {
                                                        onclick: move |_| {
                                                            let new_val = if is_on { "false" } else { "true" };
                                                            config_values.write().insert(key_t.clone(), new_val.into());
                                                        },
                                                        class: if is_on { "channels-cfg__toggle-btn channels-cfg__toggle-btn--on" } else { "channels-cfg__toggle-btn" },
                                                        r#type: "button",
                                                        div { class: "channels-cfg__toggle-track",
                                                            div { class: "channels-cfg__toggle-thumb" }
                                                        }
                                                    }
                                                }
                                            }
                                        } else if is_select {
                                            select {
                                                value: "{current_val}",
                                                onchange: move |e| {
                                                    config_values
                                                        .write()
                                                        .insert(key_clone.clone(), e.value());
                                                },
                                                class: "channels-field__input channels-cfg__select",
                                                for opt in select_options.iter() {
                                                    option {
                                                        value: "{opt}",
                                                        selected: current_val == *opt,
                                                        "{opt}"
                                                    }
                                                }
                                            }
                                        } else {
                                            input {
                                                r#type: "{input_type}",
                                                placeholder: "{field.placeholder}",
                                                value: "{current_val}",
                                                oninput: move |e| {
                                                    config_values
                                                        .write()
                                                        .insert(key_clone.clone(), e.value());
                                                },
                                                class: "channels-field__input",
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        div { class: "channels-modal__form-actions",
                            button {
                                onclick: move |_| {
                                    selected_channel.set(None);
                                    config_values.write().clear();
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
                                        let patch = build_channel_patch(&ch_id, &fields, &config);
                                        saving.set(true);
                                        spawn(async move {
                                            let params = json!({
                                                "channel": ch_id,
                                                "name": name,
                                                "config": patch,
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
                                if saving() { "Saving..." } else { "Save Configuration" }
                            }
                        }
                    }
                } else {
                    // Channel type selection grid
                    div { class: "channels-picker-search",
                        input {
                            r#type: "text",
                            class: "channels-picker-search__input",
                            placeholder: "Search channels...",
                            value: "{add_channel_search}",
                            oninput: move |e| add_channel_search.set(e.value()),
                        }
                    }
                    div { class: "channels-picker",
                        for ch_type in filtered_types.iter() {
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
                    if filtered_types.is_empty() {
                        div { class: "channels-picker-empty", "No channels match your search." }
                    }
                }
            }
        }
    }
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
        text-transform: uppercase;
        letter-spacing: 0.05em;
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

    .channels-card__status {
        display: flex;
        align-items: center;
        gap: 8px;
        flex-shrink: 0;
    }

    /* ---- Accounts section ---- */
    .channels-card__accounts {
        margin: 12px 20px 0 20px;
        padding: 8px 12px;
        background: var(--bg-tertiary);
        border-radius: var(--radius);
    }

    .channels-card__section-label {
        font-size: 11px;
        color: var(--text-muted);
        text-transform: uppercase;
        letter-spacing: 0.05em;
        margin-bottom: 4px;
    }

    .channels-card__account-row {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 2px 0;
    }

    .channels-card__account-name {
        font-size: 12px;
    }

    .channels-card__account-info {
        display: flex;
        align-items: center;
        gap: 6px;
        flex: 1;
        min-width: 0;
    }

    .channels-card__account-actions {
        display: flex;
        align-items: center;
        gap: 4px;
        flex-shrink: 0;
    }

    .channels-card__account-activity {
        font-size: 10px;
        color: var(--text-muted);
        white-space: nowrap;
    }

    .channels-card__account-error {
        padding: 4px 8px;
        margin: 2px 0 4px 0;
        background: rgba(239,68,68,0.08);
        border-radius: 4px;
        color: var(--danger);
        font-size: 11px;
        word-break: break-all;
    }

    .channels-account-toggle {
        padding: 2px 10px;
        font-size: 11px;
        border: 1px solid var(--border);
        border-radius: 4px;
        background: var(--bg-secondary);
        color: var(--text-muted);
        cursor: pointer;
    }

    .channels-account-toggle.active {
        background: var(--success);
        color: #fff;
        border-color: var(--success);
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
        background: var(--accent);
        color: #fff;
        border-color: var(--accent);
    }

    .channels-btn--primary:hover {
        opacity: 0.9;
    }

    .channels-action-btn {
        flex: 1;
        padding: 8px 12px;
        background: transparent;
        color: var(--text-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        font-size: 13px;
        cursor: pointer;
        transition: background 0.15s, color 0.15s, border-color 0.15s;
        text-align: center;
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
        flex: 2;
        background: var(--accent);
        color: #fff;
        border-color: var(--accent);
    }

    .channels-action-btn--primary:hover {
        opacity: 0.9;
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
        flex: 0 0 auto;
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
        max-width: 680px;
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

    .channels-field__secret-badge {
        font-size: 10px;
        padding: 1px 6px;
        background: rgba(239,68,68,0.15);
        color: var(--danger);
        border-radius: 8px;
        text-transform: uppercase;
        letter-spacing: 0.04em;
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
       Per-channel inline configuration (collapsible <details>)
       ================================================================ */

    .channels-cfg {
        border-top: 1px solid var(--border);
    }

    .channels-cfg__summary {
        display: flex;
        align-items: center;
        gap: 6px;
        padding: 10px 20px;
        font-size: 12px;
        font-weight: 500;
        color: var(--text-secondary);
        cursor: pointer;
        user-select: none;
        list-style: none;
        transition: background 0.15s;
    }

    .channels-cfg__summary::-webkit-details-marker {
        display: none;
    }

    .channels-cfg__summary::marker {
        content: "";
    }

    .channels-cfg__summary:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
    }

    .channels-cfg__chevron {
        display: inline-block;
        font-size: 10px;
        transition: transform 0.15s;
    }

    .channels-cfg__chevron::before {
        content: "\25B8";
    }

    details[open] > .channels-cfg__summary .channels-cfg__chevron {
        transform: rotate(90deg);
    }

    .channels-cfg__body {
        padding: 4px 20px 20px 20px;
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
        font-family: monospace;
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
