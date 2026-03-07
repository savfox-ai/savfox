use dioxus::prelude::*;

use crate::api::types::MatrixStatus;
use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};

#[component]
pub fn MatrixChannel() -> Element {
    let ws = use_context::<Signal<WsRpc>>();
    let mut status = use_signal(|| None::<MatrixStatus>);
    let mut loading = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut action_loading = use_signal(|| false);

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
                    p { class: "channels-toolbar__subtitle", "Homeserver connection and channel status." }
                }
            }

            if *loading.read() {
                div { class: "loading", "Loading..." }
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
                        }

                        if let Some(err) = &s.last_error {
                            div { class: "callout danger", style: "margin-top: 12px;", "{err}" }
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
                                    if *action_loading.read() { "Stopping..." } else { "Stop Channel" }
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
                                    if *action_loading.read() { "Starting..." } else { "Start Channel" }
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
