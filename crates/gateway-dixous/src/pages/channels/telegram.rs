use crate::api::types::TelegramStatus;
use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};
use dioxus::prelude::*;

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

fn format_timestamp(ts: i64) -> String {
    let naive = chrono::NaiveDateTime::from_timestamp_millis(ts).unwrap_or_default();
    let datetime: chrono::DateTime<chrono::Utc> =
        chrono::DateTime::from_naive_utc_and_offset(naive, chrono::Utc);
    datetime.format("%Y-%m-%d %H:%M:%S").to_string()
}

#[component]
pub fn TelegramChannel() -> Element {
    let ws = use_context::<Signal<WsRpc>>();
    let mut status = use_signal(|| None::<TelegramStatus>);
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
                .call::<TelegramStatus>(
                    "channels.status",
                    Some(serde_json::json!({"channel": "telegram"})),
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
                        span { class: "channel-icon", dangerous_inner_html: crate::utils::icons::ICON_TELEGRAM }
                        "Telegram"
                    }
                    p { class: "channels-toolbar__subtitle", "Bot status and channel configuration." }
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
                                    if let Some(username) = &s.bot_username {
                                        "@{username}"
                                    } else {
                                        "Telegram"
                                    }
                                }
                            }
                            div { class: "channels-card__status",
                                {
                                    let is_running = s.running.unwrap_or(false);
                                    let is_configured = s.configured.unwrap_or(false);
                                    let (variant, label) = if is_running {
                                        (ChipVariant::Success, "Running")
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
                            if let Some(mode) = &s.mode {
                                div { class: "channels-card__stat",
                                    span { class: "channels-card__stat-label", "Mode" }
                                    span { class: "channels-card__stat-value", "{mode}" }
                                }
                            }
                            if let Some(last_probe) = s.last_probe_at {
                                div { class: "channels-card__stat",
                                    span { class: "channels-card__stat-label", "Last Probe" }
                                    span { class: "channels-card__stat-value", "{format_timestamp(last_probe)}" }
                                }
                            }
                        }

                        if let Some(err) = &s.last_error {
                            div { class: "callout danger", style: "margin-top: 12px;", "{err}" }
                        }

                        if let Some(probe) = &s.probe {
                            div { class: "callout", style: "margin-top: 12px;",
                                "Probe "
                                { if probe.ok.unwrap_or(false) { "ok" } else { "failed" } }
                                " · "
                                { probe.status.clone().unwrap_or_default() }
                                " "
                                { probe.error.clone().unwrap_or_default() }
                            }
                        }

                        div { class: "channels-card__actions", style: "margin-top: 16px;",
                            button {
                                class: "btn channels-btn",
                                disabled: *action_loading.read(),
                                onclick: move |_| {
                                    let ws = ws.clone();
                                    spawn(async move {
                                        action_loading.set(true);
                                        let result = ws.read()
                                            .call::<serde_json::Value>("channels.test", Some(serde_json::json!({"platform": "telegram"})))
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
                                if *action_loading.read() { "Probing..." } else { "Probe" }
                            }
                            button {
                                class: "btn btn-primary",
                                onclick: move |_| {
                                    let ws = ws.clone();
                                    async move {
                                        let _ = ws.read()
                                            .call::<serde_json::Value>("channels.status", Some(serde_json::json!({"channel": "telegram", "probe": true})))
                                            .await;
                                        load_status().await;
                                    }
                                },
                                span { dangerous_inner_html: crate::utils::icons::ICON_REFRESH_CW }
                                " Refresh"
                            }
                        }
                        if let Some(ref msg) = action_msg() {
                            div { class: "channels-card__stat", style: "margin-top: 8px;",
                                span { class: "channels-card__stat-value", "{msg}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
