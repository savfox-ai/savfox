use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::NostrStatus;
use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};
use crate::components::skeleton::*;

#[component]
pub fn NostrChannel() -> Element {
    let ws = use_context::<Signal<WsRpc>>();
    let mut status = use_signal(|| None::<NostrStatus>);
    let mut loading = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut action_loading = use_signal(|| false);
    let mut action_msg = use_signal(|| None::<String>);

    let mut name = use_signal(String::new);
    let mut about = use_signal(String::new);
    let mut picture = use_signal(String::new);
    let mut nip05 = use_signal(String::new);
    let mut public_key = use_signal(String::new);
    let mut private_key = use_signal(String::new);
    let mut relays = use_signal(Vec::<String>::new);
    let mut new_relay = use_signal(String::new);
    let mut export_json = use_signal(String::new);

    let load_status = move || {
        let ws = ws.clone();
        async move {
            loading.set(true);
            error.set(None);
            let result = ws
                .read()
                .call::<NostrStatus>("channels.status", Some(json!({ "channel": "nostr" })))
                .await;
            match result {
                Ok(s) => status.set(Some(s)),
                Err(e) => error.set(Some(e)),
            }
            loading.set(false);
        }
    };

    let load_profile = move || {
        let ws = ws.clone();
        async move {
            let profile_result = ws
                .read()
                .call::<serde_json::Value>("channels.nostr.profile.get", None)
                .await;
            match profile_result {
                Ok(val) => {
                    let profile = val.get("profile").unwrap_or(&val);
                    name.set(
                        profile
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    );
                    about.set(
                        profile
                            .get("about")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    );
                    picture.set(
                        profile
                            .get("picture")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    );
                    nip05.set(
                        profile
                            .get("nip05")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    );
                    public_key.set(
                        profile
                            .get("public_key")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    );
                    private_key.set(
                        profile
                            .get("private_key")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    );
                }
                Err(e) => error.set(Some(e)),
            }

            let relays_result = ws
                .read()
                .call::<serde_json::Value>("channels.nostr.relays.get", None)
                .await;
            match relays_result {
                Ok(val) => {
                    let relay_list = val
                        .get("relays")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|item| item.as_str().map(ToString::to_string))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    relays.set(relay_list);
                }
                Err(e) => error.set(Some(e)),
            }
        }
    };

    use_effect(move || {
        spawn(async move {
            load_status().await;
            load_profile().await;
        });
    });

    rsx! {
        div { class: "channels-page",
            div { class: "channels-toolbar",
                div { class: "channels-toolbar__left",
                    h2 { class: "channels-toolbar__title",
                        span { class: "channel-icon", dangerous_inner_html: crate::utils::icons::ICON_GLOBE }
                        "Nostr"
                    }
                    p { class: "channels-toolbar__subtitle", "Decentralized DMs via Nostr relays (NIP-04)." }
                }
            }

            if *loading.read() {
                SkeletonCard {}
            }

            if let Some(err) = error.read().as_ref() {
                div { class: "callout danger", "{err}" }
            }

            div { class: "channels-card", style: "margin-bottom: 16px;",
                h3 { class: "channels-card__title", "Setup Guide" }
                ol { style: "padding-left: 1.5rem;",
                    li { "Fill out your profile fields (display name, about, picture, NIP-05)." }
                    li { "Import an existing keypair (private key + optional public key)." }
                    li { "Add or remove relays, then save relay configuration." }
                    li { "Test connection and export a profile backup." }
                }
            }

            if let Some(s) = status.read().as_ref() {
                div { class: "channels-card", style: "margin-bottom: 16px;",
                    div { class: "channels-card__header",
                        div { class: "channels-card__identity",
                            div { class: "channels-card__name", "Status" }
                        }
                        div { class: "channels-card__status",
                            {
                                let is_connected = s.connected.unwrap_or(false);
                                let is_configured = s.configured.unwrap_or(false);
                                let (variant, label) = if is_connected {
                                    (ChipVariant::Success, "Connected")
                                } else if is_configured {
                                    (ChipVariant::Warning, "Configured")
                                } else {
                                    (ChipVariant::Muted, "Not configured")
                                };
                                rsx! { Chip { label: label.to_string(), variant: variant } }
                            }
                        }
                    }

                    div { class: "channels-card__stats",
                        div { class: "channels-card__stat",
                            span { class: "channels-card__stat-label", "Configured" }
                            span { class: "channels-card__stat-value", if s.configured.unwrap_or(false) { "Yes" } else { "No" } }
                        }
                        div { class: "channels-card__stat",
                            span { class: "channels-card__stat-label", "Connected" }
                            span { class: "channels-card__stat-value", if s.connected.unwrap_or(false) { "Connected" } else { "Disconnected" } }
                        }
                        if let Some(pk) = &s.public_key {
                            if !pk.is_empty() {
                                div { class: "channels-card__stat",
                                    span { class: "channels-card__stat-label", "Public Key" }
                                    span { class: "channels-card__stat-value", "{pk}" }
                                }
                            }
                        }
                        if let Some(count) = s.relay_count {
                            div { class: "channels-card__stat",
                                span { class: "channels-card__stat-label", "Relays" }
                                span { class: "channels-card__stat-value", "{count}" }
                            }
                        }
                    }
                    if let Some(err) = &s.last_error {
                        div { class: "callout danger", style: "margin-top: 12px;", "{err}" }
                    }
                }
            }

            div { class: "channels-card", style: "margin-bottom: 16px;",
                h3 { class: "channels-card__title", "Profile" }
                div { class: "channels-card__actions", style: "margin-bottom: 12px;",
                    input {
                        class: "channels-field__input",
                        placeholder: "Display Name",
                        value: "{name}",
                        oninput: move |e| name.set(e.value()),
                    }
                    input {
                        class: "channels-field__input",
                        placeholder: "NIP-05 (e.g. user@example.com)",
                        value: "{nip05}",
                        oninput: move |e| nip05.set(e.value()),
                    }
                    input {
                        class: "channels-field__input",
                        placeholder: "Picture URL",
                        value: "{picture}",
                        oninput: move |e| picture.set(e.value()),
                    }
                }

                textarea {
                    class: "channels-field__input channels-cfg__textarea",
                    rows: "3",
                    placeholder: "About",
                    value: "{about}",
                    oninput: move |e| about.set(e.value()),
                }

                div { class: "channels-card__actions", style: "margin-top: 12px; margin-bottom: 12px;",
                    input {
                        class: "channels-field__input",
                        placeholder: "Public key (npub or hex)",
                        value: "{public_key}",
                        oninput: move |e| public_key.set(e.value()),
                    }
                    input {
                        class: "channels-field__input",
                        r#type: "password",
                        placeholder: "Private key (nsec or hex)",
                        value: "{private_key}",
                        oninput: move |e| private_key.set(e.value()),
                    }
                }

                div { class: "channels-card__actions",
                    button {
                        class: "btn btn-success",
                        disabled: *action_loading.read(),
                        onclick: move |_| {
                            let ws = ws.clone();
                            spawn(async move {
                                action_loading.set(true);
                                let result = ws.read()
                                    .call::<serde_json::Value>(
                                        "channels.nostr.profile.set",
                                        Some(json!({
                                            "profile": {
                                                "name": name(),
                                                "about": about(),
                                                "picture": picture(),
                                                "nip05": nip05(),
                                                "public_key": public_key(),
                                                "private_key": private_key(),
                                            }
                                        })),
                                    )
                                    .await;
                                action_loading.set(false);
                                match result {
                                    Ok(_) => {
                                        action_msg.set(Some("Nostr profile saved".to_string()));
                                        load_status().await;
                                    }
                                    Err(e) => action_msg.set(Some(format!("Save failed: {e}"))),
                                }
                            });
                        },
                        "Save Profile"
                    }
                    button {
                        class: "btn channels-btn",
                        disabled: *action_loading.read() || private_key().trim().is_empty(),
                        onclick: move |_| {
                            let ws = ws.clone();
                            spawn(async move {
                                action_loading.set(true);
                                let result = ws.read()
                                    .call::<serde_json::Value>(
                                        "channels.nostr.profile.import",
                                        Some(json!({
                                            "private_key": private_key(),
                                            "public_key": public_key(),
                                        })),
                                    )
                                    .await;
                                action_loading.set(false);
                                match result {
                                    Ok(_) => {
                                        action_msg.set(Some("Keypair imported".to_string()));
                                        load_status().await;
                                        load_profile().await;
                                    }
                                    Err(e) => action_msg.set(Some(format!("Import failed: {e}"))),
                                }
                            });
                        },
                        "Import Keypair"
                    }
                    button {
                        class: "btn channels-btn",
                        disabled: *action_loading.read(),
                        onclick: move |_| {
                            let ws = ws.clone();
                            spawn(async move {
                                action_loading.set(true);
                                let result = ws.read()
                                    .call::<serde_json::Value>("channels.nostr.profile.export", None)
                                    .await;
                                action_loading.set(false);
                                match result {
                                    Ok(v) => {
                                        let export_body = v.get("profile").unwrap_or(&v);
                                        let pretty = serde_json::to_string_pretty(export_body)
                                            .unwrap_or_else(|_| "{}".to_string());
                                        export_json.set(pretty);
                                        action_msg.set(Some("Profile exported".to_string()));
                                    }
                                    Err(e) => action_msg.set(Some(format!("Export failed: {e}"))),
                                }
                            });
                        },
                        "Export Profile"
                    }
                }
            }

            div { class: "channels-card", style: "margin-bottom: 16px;",
                h3 { class: "channels-card__title", "Relays" }
                p { class: "channels-card__desc", "Manage relay endpoints used for publish/subscribe." }
                div { class: "channels-card__actions", style: "margin-bottom: 12px;",
                    input {
                        class: "channels-field__input",
                        placeholder: "wss://relay.example.com",
                        value: "{new_relay}",
                        oninput: move |e| new_relay.set(e.value()),
                    }
                    button {
                        class: "btn channels-btn",
                        disabled: *action_loading.read(),
                        onclick: move |_| {
                            let candidate = new_relay().trim().to_string();
                            if candidate.is_empty() {
                                return;
                            }
                            if !candidate.starts_with("wss://") && !candidate.starts_with("ws://") {
                                action_msg.set(Some("Relay URL must start with ws:// or wss://".to_string()));
                                return;
                            }
                            let mut list = relays();
                            let exists = list.iter().any(|r| r.eq_ignore_ascii_case(&candidate));
                            if exists {
                                action_msg.set(Some("Relay already exists".to_string()));
                                return;
                            }
                            list.push(candidate);
                            relays.set(list);
                            new_relay.set(String::new());
                        },
                        "Add Relay"
                    }
                }

                if relays.read().is_empty() {
                    div { class: "channels-card__stat",
                        span { class: "channels-card__stat-label", "No relays configured yet." }
                    }
                } else {
                    div { class: "channels-card__stats",
                        for relay in relays().iter().cloned() {
                            div { class: "channels-card__stat", style: "align-items: center;",
                                span { class: "channels-card__stat-value", "{relay}" }
                                button {
                                    class: "btn btn-danger",
                                    style: "padding: 2px 8px;",
                                    disabled: *action_loading.read(),
                                    onclick: {
                                        let relay_to_remove = relay.clone();
                                        move |_| {
                                            let mut list = relays();
                                            list.retain(|r| r != &relay_to_remove);
                                            relays.set(list);
                                        }
                                    },
                                    "Remove"
                                }
                            }
                        }
                    }
                }

                div { class: "channels-card__actions", style: "margin-top: 12px;",
                    button {
                        class: "btn btn-success",
                        disabled: *action_loading.read(),
                        onclick: move |_| {
                            let ws = ws.clone();
                            spawn(async move {
                                action_loading.set(true);
                                let result = ws.read()
                                    .call::<serde_json::Value>(
                                        "channels.nostr.relays.set",
                                        Some(json!({ "relays": relays() })),
                                    )
                                    .await;
                                action_loading.set(false);
                                match result {
                                    Ok(_) => {
                                        action_msg.set(Some("Relays saved".to_string()));
                                        load_status().await;
                                        load_profile().await;
                                    }
                                    Err(e) => action_msg.set(Some(format!("Relay save failed: {e}"))),
                                }
                            });
                        },
                        "Save Relays"
                    }
                    button {
                        class: "btn channels-btn",
                        disabled: *action_loading.read(),
                        onclick: move |_| {
                            spawn(async move {
                                load_profile().await;
                                load_status().await;
                            });
                        },
                        "Reload Relays"
                    }
                }
            }

            if let Some(ref msg) = action_msg() {
                div { class: "callout", style: "margin-bottom: 16px;", "{msg}" }
            }

            div { class: "channels-card__actions", style: "margin-bottom: 16px;",
                button {
                    class: "btn channels-btn",
                    disabled: *action_loading.read(),
                    onclick: move |_| {
                        let ws = ws.clone();
                        spawn(async move {
                            action_loading.set(true);
                            let result = ws.read()
                                .call::<serde_json::Value>("channels.test", Some(json!({ "platform": "nostr" })))
                                .await;
                            action_loading.set(false);
                            match result {
                                Ok(v) => {
                                    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
                                    let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("Test complete");
                                    action_msg.set(Some(format!("{}: {}", if ok { "OK" } else { "Failed" }, msg)));
                                }
                                Err(e) => action_msg.set(Some(format!("Test failed: {e}"))),
                            }
                        });
                    },
                    "Test Connection"
                }
                button {
                    class: "btn channels-btn",
                    onclick: move |_| {
                        spawn(async move {
                            load_status().await;
                            load_profile().await;
                        });
                    },
                    span { dangerous_inner_html: crate::utils::icons::ICON_REFRESH_CW }
                    " Refresh"
                }
            }

            if !export_json().is_empty() {
                div { class: "channels-card",
                    h3 { class: "channels-card__title", "Export JSON" }
                    pre { class: "channels-modal__pre", "{export_json}" }
                }
            }
        }
    }
}
