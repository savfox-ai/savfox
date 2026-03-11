use dioxus::prelude::*;

use crate::api::types::WhatsAppStatus;
use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};
use crate::components::skeleton::*;
use crate::utils::time::format_duration_human;

#[component]
pub fn WhatsAppChannel() -> Element {
    let ws = use_context::<Signal<WsRpc>>();
    let mut status = use_signal(|| None::<WhatsAppStatus>);
    let mut loading = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut action_loading = use_signal(|| false);
    let mut action_msg = use_signal(|| None::<String>);

    let load_status = move || {
        let ws = ws.clone();
        async move {
            loading.set(true);
            error.set(None);

            let result = ws
                .read()
                .call::<WhatsAppStatus>(
                    "channels.status",
                    Some(serde_json::json!({"channel": "whatsapp"})),
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
                        span { class: "channel-icon", dangerous_inner_html: crate::utils::icons::ICON_WHATSAPP }
                        "WhatsApp"
                    }
                    p { class: "channels-toolbar__subtitle", "Link WhatsApp Web and monitor connection health." }
                }
            }

            if *loading.read() {
                SkeletonCard {}
            }

            if let Some(err) = error.read().as_ref() {
                div { class: "callout danger", "{err}" }
            }

            if let Some(s) = status.read().as_ref() {
                div { class: "channels-grid",
                    div { class: "channels-card",
                        div { class: "channels-card__header",
                            div { class: "channels-card__identity",
                                div { class: "channels-card__name",
                                    if let Some(self_user) = &s.self_user {
                                        if let Some(name) = &self_user.push_name {
                                            "{name}"
                                        } else {
                                            "WhatsApp"
                                        }
                                    } else {
                                        "WhatsApp"
                                    }
                                }
                            }
                            div { class: "channels-card__status",
                                {
                                    let is_running = s.running.unwrap_or(false);
                                    let is_linked = s.linked.unwrap_or(false);
                                    let is_configured = s.configured.unwrap_or(false);
                                    let (variant, label) = if is_running && is_linked {
                                        (ChipVariant::Success, "Linked")
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
                            div { class: "channels-card__stat",
                                span { class: "channels-card__stat-label", "Configured" }
                                span { class: "channels-card__stat-value", if s.configured.unwrap_or(false) { "Yes" } else { "No" } }
                            }
                            div { class: "channels-card__stat",
                                span { class: "channels-card__stat-label", "Linked" }
                                span { class: "channels-card__stat-value", if s.linked.unwrap_or(false) { "Yes" } else { "No" } }
                            }
                            div { class: "channels-card__stat",
                                span { class: "channels-card__stat-label", "Running" }
                                span { class: "channels-card__stat-value", if s.running.unwrap_or(false) { "Yes" } else { "No" } }
                            }
                            div { class: "channels-card__stat",
                                span { class: "channels-card__stat-label", "Connected" }
                                span { class: "channels-card__stat-value", if s.connected.unwrap_or(false) { "Yes" } else { "No" } }
                            }
                            if let Some(auth_age) = s.auth_age_ms {
                                div { class: "channels-card__stat",
                                    span { class: "channels-card__stat-label", "Auth Age" }
                                    span { class: "channels-card__stat-value",
                                        "{format_duration_human(auth_age as u64)}"
                                    }
                                }
                            }
                        }

                        if let Some(err) = &s.last_error {
                            div { class: "callout danger", style: "margin-top: 12px;", "{err}" }
                        }

                        if let Some(ref msg) = action_msg() {
                            div { class: "callout", style: "margin-top: 12px;", "{msg}" }
                        }

                        if let Some(qr) = &s.qr_data_url {
                            div { class: "qr-wrap", style: "margin-top: 12px;",
                                img {
                                    src: "{qr}",
                                    alt: "WhatsApp QR Code",
                                    class: "qr-code"
                                }
                            }
                        }

                        div { class: "channels-card__actions", style: "margin-top: 16px; flex-wrap: wrap;",
                            button {
                                class: "btn btn-primary",
                                disabled: *action_loading.read(),
                                onclick: move |_| {
                                    let ws = ws.clone();
                                    spawn(async move {
                                        action_loading.set(true);
                                        let result = ws.read()
                                            .call::<serde_json::Value>("web.login.start", Some(serde_json::json!({"channel": "whatsapp"})))
                                            .await;
                                        action_loading.set(false);
                                        match result {
                                            Ok(v) => {
                                                let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("Login flow started").to_string();
                                                action_msg.set(Some(msg));
                                            }
                                            Err(e) => action_msg.set(Some(format!("Start failed: {e}"))),
                                        }
                                        load_status().await;
                                    });
                                },
                                if *action_loading.read() { "Working..." } else { "Show QR" }
                            }
                            button {
                                class: "btn channels-btn",
                                disabled: *action_loading.read(),
                                onclick: move |_| {
                                    let ws = ws.clone();
                                    spawn(async move {
                                        action_loading.set(true);
                                        let result = ws.read()
                                            .call::<serde_json::Value>("web.login.start", Some(serde_json::json!({
                                                "channel": "whatsapp",
                                                "relink": true
                                            })))
                                            .await;

                                        action_loading.set(false);
                                        match result {
                                            Ok(_) => action_msg.set(Some("Started relink flow".into())),
                                            Err(e) => action_msg.set(Some(format!("Relink failed: {e}"))),
                                        }
                                        load_status().await;
                                    });
                                },
                                "Relink"
                            }
                            button {
                                class: "btn channels-btn",
                                disabled: *action_loading.read(),
                                onclick: move |_| {
                                    let ws = ws.clone();
                                    spawn(async move {
                                        action_loading.set(true);
                                        let result = ws.read()
                                            .call::<serde_json::Value>("web.login.wait", Some(serde_json::json!({"channel": "whatsapp"})))
                                            .await;
                                        action_loading.set(false);
                                        match result {
                                            Ok(v) => {
                                                let connected = v.get("connected").and_then(|c| c.as_bool()).unwrap_or(false);
                                                action_msg.set(Some(if connected {
                                                    "WhatsApp linked successfully".to_string()
                                                } else {
                                                    "Still waiting for QR scan".to_string()
                                                }));
                                            }
                                            Err(e) => action_msg.set(Some(format!("Wait failed: {e}"))),
                                        }
                                        load_status().await;
                                    });
                                },
                                "Wait for Scan"
                            }
                            button {
                                class: "btn btn-danger",
                                disabled: *action_loading.read(),
                                onclick: move |_| {
                                    let ws = ws.clone();
                                    spawn(async move {
                                        action_loading.set(true);
                                        let result = ws.read()
                                            .call::<serde_json::Value>("channels.logout", Some(serde_json::json!({"channel": "whatsapp"})))
                                            .await;
                                        action_loading.set(false);
                                        match result {
                                            Ok(_) => action_msg.set(Some("Logged out".into())),
                                            Err(e) => action_msg.set(Some(format!("Logout failed: {e}"))),
                                        }
                                        load_status().await;
                                    });
                                },
                                "Logout"
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
