use dioxus::prelude::*;

use crate::api::types::MatrixStatus;
use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};
use crate::components::skeleton::*;

pub(crate) fn registration_to_yaml(reg: &serde_json::Value) -> String {
    let id = reg.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let url = reg.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let as_token = reg.get("as_token").and_then(|v| v.as_str()).unwrap_or("");
    let hs_token = reg.get("hs_token").and_then(|v| v.as_str()).unwrap_or("");
    let sender_localpart = reg
        .get("sender_localpart")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let rate_limited = reg
        .get("rate_limited")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut yaml = String::new();
    yaml.push_str(&format!("id: \"{id}\"\n"));
    yaml.push_str(&format!("url: \"{url}\"\n"));
    yaml.push_str(&format!("as_token: \"{as_token}\"\n"));
    yaml.push_str(&format!("hs_token: \"{hs_token}\"\n"));
    yaml.push_str(&format!("sender_localpart: \"{sender_localpart}\"\n"));
    yaml.push_str(&format!("rate_limited: {rate_limited}\n"));
    yaml.push_str("namespaces:\n");

    if let Some(namespaces) = reg.get("namespaces").and_then(|v| v.as_object()) {
        for section in ["users", "aliases", "rooms"] {
            yaml.push_str(&format!("  {section}:\n"));
            if let Some(entries) = namespaces.get(section).and_then(|v| v.as_array()) {
                if entries.is_empty() {
                    yaml.push_str("    []\n");
                } else {
                    for entry in entries {
                        let exclusive = entry
                            .get("exclusive")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let regex = entry.get("regex").and_then(|v| v.as_str()).unwrap_or("");
                        yaml.push_str(&format!("    - exclusive: {exclusive}\n"));
                        yaml.push_str(&format!("      regex: \"{regex}\"\n"));
                    }
                }
            } else {
                yaml.push_str("    []\n");
            }
        }
    }

    yaml
}

#[component]
pub fn MatrixChannel() -> Element {
    let ws = use_context::<Signal<WsRpc>>();
    let mut status = use_signal(|| None::<MatrixStatus>);
    let mut loading = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut action_loading = use_signal(|| false);
    let mut show_yaml = use_signal(|| false);

    let load_status = move || {
        let ws = ws.clone();
        async move {
            loading.set(true);
            error.set(None);

            let result = ws
                .read()
                .call::<MatrixStatus>(
                    "channels.status",
                    Some(serde_json::json!({"channel": "matrix"})),
                )
                .await;

            match result {
                Ok(s) => status.set(Some(s)),
                Err(e) => error.set(Some(e)),
            }
            loading.set(false);
        }
    };

    use_effect(move || {
        spawn(async move {
            load_status().await;
        });
    });

    rsx! {
        div { class: "channels-page",
            div { class: "channels-toolbar",
                div { class: "channels-toolbar__left",
                    h2 { class: "channels-toolbar__title",
                        span { class: "channel-icon", dangerous_inner_html: crate::utils::icons::ICON_MATRIX }
                        "Matrix"
                    }
                    p { class: "channels-toolbar__subtitle", "Homeserver, user session, and appservice status." }
                }
            }

            if *loading.read() {
                SkeletonCard {}
            }

            if let Some(err) = error.read().as_ref() {
                div { class: "callout danger", "{err}" }
            }

            if let Some(s) = status.read().as_ref() {
                {
                    let mode_label = s.mode.as_deref().unwrap_or("user");
                    let is_appservice = mode_label == "appservice";
                    let registration_json = s
                        .registration
                        .as_ref()
                        .and_then(|value| serde_json::to_string_pretty(value).ok());
                    let registration_yaml = s
                        .registration
                        .as_ref()
                        .map(registration_to_yaml);
                    let stop_label = format!("Stop {mode_label}");
                    let start_label = format!("Start {mode_label}");

                    rsx! {
                        div { class: "channels-grid",
                            div { class: "channels-card",
                                div { class: "channels-card__header",
                                    div { class: "channels-card__identity",
                                        div { class: "channels-card__name",
                                            if let Some(homeserver) = &s.homeserver {
                                                "{homeserver}"
                                            } else {
                                                "Matrix"
                                            }
                                        }
                                    }
                                    div { class: "channels-card__status",
                                        {
                                            let is_running = s.running.unwrap_or(false);
                                            let is_connected = s.connected.unwrap_or(false);
                                            let is_configured = s.configured.unwrap_or(false);
                                            let (variant, label) = if is_running && is_connected {
                                                (ChipVariant::Success, "Connected")
                                            } else if is_running {
                                                (ChipVariant::Warning, "Running")
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
                                    if let Some(mode) = &s.mode {
                                        div { class: "channels-card__stat",
                                            span { class: "channels-card__stat-label", "Mode" }
                                            span { class: "channels-card__stat-value", "{mode}" }
                                        }
                                    }
                                    div { class: "channels-card__stat",
                                        span { class: "channels-card__stat-label", "Configured" }
                                        span { class: "channels-card__stat-value", if s.configured.unwrap_or(false) { "Yes" } else { "No" } }
                                    }
                                    div { class: "channels-card__stat",
                                        span { class: "channels-card__stat-label", "Running" }
                                        span { class: "channels-card__stat-value", if s.running.unwrap_or(false) { "Yes" } else { "No" } }
                                    }
                                    div { class: "channels-card__stat",
                                        span { class: "channels-card__stat-label", "Connected" }
                                        span { class: "channels-card__stat-value", if s.connected.unwrap_or(false) { "Yes" } else { "No" } }
                                    }
                                    if let Some(user_id) = &s.user_id {
                                        div { class: "channels-card__stat",
                                            span { class: "channels-card__stat-label", "User ID" }
                                            span { class: "channels-card__stat-value", "{user_id}" }
                                        }
                                    }
                                    if let Some(count) = s.room_count {
                                        div { class: "channels-card__stat",
                                            span { class: "channels-card__stat-label", "Rooms" }
                                            span { class: "channels-card__stat-value", "{count}" }
                                        }
                                    }
                                    if let Some(appservice_url) = &s.appservice_url {
                                        div { class: "channels-card__stat",
                                            span { class: "channels-card__stat-label", "Appservice URL" }
                                            span { class: "channels-card__stat-value", "{appservice_url}" }
                                        }
                                    }
                                    if let Some(sender_localpart) = &s.sender_localpart {
                                        div { class: "channels-card__stat",
                                            span { class: "channels-card__stat-label", "Sender Localpart" }
                                            span { class: "channels-card__stat-value", "{sender_localpart}" }
                                        }
                                    }
                                }

                                if let Some(err) = &s.last_error {
                                    div { class: "callout danger", style: "margin-top: 12px;", "{err}" }
                                }

                                if let Some(registration_json) = registration_json {
                                    div { class: "callout", style: "margin-top: 12px;",
                                        div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;",
                                            div { style: "font-weight:600;", "Appservice Registration" }
                                            div { style: "display:flex;gap:6px;",
                                                button {
                                                    class: "btn channels-btn",
                                                    style: "padding:4px 10px;font-size:11px;",
                                                    onclick: move |_| show_yaml.set(!show_yaml()),
                                                    if show_yaml() { "Show JSON" } else { "Show YAML" }
                                                }
                                                if is_appservice {
                                                    if let Some(ref yaml_content) = registration_yaml {
                                                        {
                                                            let yaml_for_download = yaml_content.clone();
                                                            rsx! {
                                                                button {
                                                                    class: "btn channels-btn",
                                                                    style: "padding:4px 10px;font-size:11px;",
                                                                    onclick: move |_| {
                                                                        let yaml = yaml_for_download.clone();
                                                                        spawn(async move {
                                                                            trigger_yaml_download(&yaml);
                                                                        });
                                                                    },
                                                                    "Export YAML"
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        if show_yaml() {
                                            if let Some(ref yaml) = registration_yaml {
                                                pre { style: "margin:0;white-space:pre-wrap;word-break:break-word;font-size:12px;", "{yaml}" }
                                            }
                                        } else {
                                            pre { style: "margin:0;white-space:pre-wrap;word-break:break-word;font-size:12px;", "{registration_json}" }
                                        }
                                    }
                                }

                                div { class: "channels-card__actions", style: "margin-top: 16px;",
                                    if s.running.unwrap_or(false) {
                                        button {
                                            class: "btn btn-danger",
                                            onclick: move |_| {
                                                let ws = ws.clone();
                                                async move {
                                                    action_loading.set(true);
                                                    let _ = ws.read()
                                                        .call::<serde_json::Value>("channels.logout", Some(serde_json::json!({"channel": "matrix"})))
                                                        .await;
                                                    action_loading.set(false);
                                                    load_status().await;
                                                }
                                            },
                                            disabled: *action_loading.read(),
                                            if *action_loading.read() { "Stopping..." } else { "{stop_label}" }
                                        }
                                    } else {
                                        button {
                                            class: "btn btn-success",
                                            onclick: move |_| {
                                                let ws = ws.clone();
                                                async move {
                                                    action_loading.set(true);
                                                    let _ = ws.read()
                                                        .call::<serde_json::Value>("channels.login", Some(serde_json::json!({"channel": "matrix"})))
                                                        .await;
                                                    action_loading.set(false);
                                                    load_status().await;
                                                }
                                            },
                                            disabled: *action_loading.read(),
                                            if *action_loading.read() { "Starting..." } else { "{start_label}" }
                                        }
                                    }
                                    button {
                                        class: "btn channels-btn",
                                        onclick: move |_| {
                                            spawn(async move {
                                                load_status().await;
                                            });
                                        },
                                        span { dangerous_inner_html: crate::utils::icons::ICON_REFRESH_CW }
                                        " Refresh"
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

pub(crate) fn trigger_yaml_download(yaml: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let window = web_sys::window().unwrap();
        let document = window.document().unwrap();
        let blob_parts = js_sys::Array::new();
        blob_parts.push(&wasm_bindgen::JsValue::from_str(yaml));
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type("application/x-yaml");
        let blob = web_sys::Blob::new_with_str_sequence_and_options(&blob_parts, &opts).unwrap();
        let url = web_sys::Url::create_object_url_with_blob(&blob).unwrap();
        let a = document
            .create_element("a")
            .unwrap()
            .dyn_into::<web_sys::HtmlAnchorElement>()
            .unwrap();
        a.set_href(&url);
        a.set_download("appservice-registration.yaml");
        a.click();
        let _ = web_sys::Url::revoke_object_url(&url);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = yaml;
    }
}
