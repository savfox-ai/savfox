use dioxus::prelude::*;

use crate::api::types::MsTeamsStatus;
use crate::api::ws::WsRpc;

#[component]
pub fn MsTeamsChannel() -> Element {
    let ws = use_context::<Signal<WsRpc>>();
    let mut status = use_signal(|| None::<MsTeamsStatus>);
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
                .call::<MsTeamsStatus>(
                    "channels.status",
                    Some(serde_json::json!({"channel": "msteams"})),
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
        div { class: "channel-page",
            div { class: "channel-header",
                span { class: "channel-icon", dangerous_inner_html: crate::utils::icons::ICON_MSTEAMS }
                h2 { "Microsoft Teams" }
            }

            if *loading.read() {
                div { class: "loading", "Loading..." }
            }

            if let Some(err) = error.read().as_ref() {
                div { class: "error-message", "{err}" }
            }

            if let Some(s) = status.read().as_ref() {
                div { class: "channel-content",
                    div { class: "status-card",
                        h3 { "Status" }
                        div { class: "status-grid",
                            div { class: "status-item",
                                span { class: "status-label", "Configured" }
                                span { class: "status-value",
                                    if s.configured.unwrap_or(false) { "Yes" } else { "No" }
                                }
                            }
                            div { class: "status-item",
                                span { class: "status-label", "Running" }
                                span { class: if s.running.unwrap_or(false) { "status-value status-running" } else { "status-value status-stopped" },
                                    if s.running.unwrap_or(false) { "Running" } else { "Stopped" }
                                }
                            }
                            div { class: "status-item",
                                span { class: "status-label", "Connected" }
                                span { class: if s.connected.unwrap_or(false) { "status-value status-connected" } else { "status-value status-disconnected" },
                                    if s.connected.unwrap_or(false) { "Connected" } else { "Disconnected" }
                                }
                            }
                            if let Some(tenant) = &s.tenant_id {
                                div { class: "status-item",
                                    span { class: "status-label", "Tenant ID" }
                                    span { class: "status-value", "{tenant}" }
                                }
                            }
                            if let Some(name) = &s.bot_name {
                                div { class: "status-item",
                                    span { class: "status-label", "Bot Name" }
                                    span { class: "status-value", "{name}" }
                                }
                            }
                            if let Some(err) = &s.last_error {
                                div { class: "status-item error",
                                    span { class: "status-label", "Last Error" }
                                    span { class: "status-value error", "{err}" }
                                }
                            }
                        }
                    }

                    div { class: "action-buttons",
                        button {
                            class: "btn btn-primary",
                            onclick: move |_| {
                                spawn(async move {
                                    load_status().await;
                                });
                            },
                            span { dangerous_inner_html: crate::utils::icons::ICON_REFRESH_CW }
                            "Refresh"
                        }

                        if s.running.unwrap_or(false) {
                            button {
                                class: "btn btn-danger",
                                onclick: move |_| {
                                    let ws = ws.clone();
                                    async move {
                                        action_loading.set(true);
                                        let _ = ws.read()
                                            .call::<serde_json::Value>("channels.logout", Some(serde_json::json!({"channel": "msteams"})))
                                            .await;
                                        action_loading.set(false);
                                        load_status().await;
                                    }
                                },
                                disabled: *action_loading.read(),
                                if *action_loading.read() { "Stopping..." } else { "Stop Bridge" }
                            }
                        } else {
                            button {
                                class: "btn btn-success",
                                onclick: move |_| {
                                    let ws = ws.clone();
                                    async move {
                                        action_loading.set(true);
                                        let _ = ws.read()
                                            .call::<serde_json::Value>("channels.login", Some(serde_json::json!({"channel": "msteams"})))
                                            .await;
                                        action_loading.set(false);
                                        load_status().await;
                                    }
                                },
                                disabled: *action_loading.read(),
                                if *action_loading.read() { "Starting..." } else { "Start Bridge" }
                            }
                        }
                    }
                }
            }
        }
    }
}
