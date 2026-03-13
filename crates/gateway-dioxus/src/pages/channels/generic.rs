use dioxus::prelude::*;
use serde_json::{Value, json};

use crate::api::ws::WsRpc;
use crate::components::chip::{Chip, ChipVariant};
use crate::components::skeleton::*;

fn bool_field(status: &Value, key: &str) -> bool {
    status.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn status_value(status: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let value = status.get(*key)?;
        if let Some(text) = value.as_str() {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        if let Some(number) = value.as_u64() {
            return Some(number.to_string());
        }
        if let Some(number) = value.as_i64() {
            return Some(number.to_string());
        }
        if let Some(number) = value.as_f64() {
            return Some(format!("{number:.2}"));
        }
        if let Some(items) = value.as_array() {
            return Some(items.len().to_string());
        }

        None
    })
}

fn channel_metrics(status: &Value) -> Vec<(String, String)> {
    let mut metrics = vec![
        (
            "Configured".to_string(),
            if bool_field(status, "configured") {
                "Yes".to_string()
            } else {
                "No".to_string()
            },
        ),
        (
            "Running".to_string(),
            if bool_field(status, "running") {
                "Yes".to_string()
            } else {
                "No".to_string()
            },
        ),
        (
            "Connected".to_string(),
            if bool_field(status, "connected") {
                "Yes".to_string()
            } else {
                "No".to_string()
            },
        ),
    ];

    for (label, keys) in [
        ("Channel", &["channel_name", "channelName"][..]),
        ("Mode", &["mode"][..]),
        ("Bot", &["bot_username", "bot_name"][..]),
        ("Workspace", &["workspace_name", "workspaceName"][..]),
        ("Server URL", &["server_url", "ship_url", "homeserver"][..]),
        ("Webhook URL", &["callback_url", "webhook_url", "url"][..]),
        ("Ship", &["ship_name"][..]),
        ("App ID", &["app_id"][..]),
        ("Rooms", &["room_count"][..]),
        ("Channels", &["channel_count"][..]),
        ("Last Activity", &["lastActivity"][..]),
        ("Last Probe", &["lastProbeAt"][..]),
    ] {
        if let Some(value) = status_value(status, keys) {
            metrics.push((label.to_string(), value));
        }
    }

    metrics
}

#[component]
pub fn GenericChannelPage(
    platform: &'static str,
    title: &'static str,
    subtitle: &'static str,
    icon_svg: &'static str,
) -> Element {
    let ws = use_context::<Signal<WsRpc>>();
    let mut status = use_signal(|| None::<Value>);
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
                .call::<Value>("channels.status", Some(json!({ "channel": platform })))
                .await;

            match result {
                Ok(value) => status.set(Some(value)),
                Err(err) => error.set(Some(err)),
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
                        span { class: "channel-icon", dangerous_inner_html: icon_svg }
                        "{title}"
                    }
                    p { class: "channels-toolbar__subtitle", "{subtitle}" }
                }
            }

            if *loading.read() {
                SkeletonCard {}
            }

            if let Some(err) = error.read().as_ref() {
                div { class: "callout danger", "{err}" }
            }

            if let Some(status_value) = status.read().as_ref() {
                {
                    let configured = bool_field(status_value, "configured");
                    let running = bool_field(status_value, "running");
                    let connected = bool_field(status_value, "connected");
                    let metrics = channel_metrics(status_value);
                    let last_error = status_value
                        .get("last_error")
                        .or_else(|| status_value.get("lastError"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let (variant, label) = if running && connected {
                        (ChipVariant::Success, "Connected")
                    } else if running {
                        (ChipVariant::Warning, "Running")
                    } else if configured {
                        (ChipVariant::Info, "Configured")
                    } else {
                        (ChipVariant::Muted, "Not configured")
                    };

                    rsx! {
                        div { class: "channels-grid",
                            div { class: "channels-card",
                                div { class: "channels-card__header",
                                    div { class: "channels-card__identity",
                                        div { class: "channels-card__name", "{title}" }
                                    }
                                    div { class: "channels-card__status",
                                        Chip { label: label.to_string(), variant: variant }
                                    }
                                }

                                div { class: "channels-card__stats",
                                    for (metric_label, metric_value) in metrics.into_iter() {
                                        div {
                                            key: "{metric_label}",
                                            class: "channels-card__stat",
                                            span { class: "channels-card__stat-label", "{metric_label}" }
                                            span { class: "channels-card__stat-value", "{metric_value}" }
                                        }
                                    }
                                }

                                if let Some(err) = last_error.as_ref() {
                                    div { class: "callout danger", style: "margin-top: 12px;", "{err}" }
                                }

                                if let Some(msg) = action_msg() {
                                    div { class: "callout", style: "margin-top: 12px;", "{msg}" }
                                }

                                div { class: "channels-card__actions", style: "margin-top: 16px;",
                                    if running || connected {
                                        button {
                                            class: "btn btn-danger",
                                            disabled: *action_loading.read(),
                                            onclick: move |_| {
                                                let ws = ws.clone();
                                                spawn(async move {
                                                    action_loading.set(true);
                                                    let result = ws.read()
                                                        .call::<Value>(
                                                            "channels.logout",
                                                            Some(json!({ "platform": platform })),
                                                        )
                                                        .await;
                                                    action_loading.set(false);
                                                    match result {
                                                        Ok(value) => {
                                                            let message = value
                                                                .get("status")
                                                                .and_then(Value::as_str)
                                                                .unwrap_or("Stopped")
                                                                .to_string();
                                                            action_msg.set(Some(message));
                                                        }
                                                        Err(err) => action_msg.set(Some(format!("Stop failed: {err}"))),
                                                    }
                                                    load_status().await;
                                                });
                                            },
                                            if *action_loading.read() { "Stopping..." } else { "Stop Channel" }
                                        }
                                    } else {
                                        button {
                                            class: "btn btn-success",
                                            disabled: *action_loading.read(),
                                            onclick: move |_| {
                                                let ws = ws.clone();
                                                spawn(async move {
                                                    action_loading.set(true);
                                                    let result = ws.read()
                                                        .call::<Value>(
                                                            "channels.login",
                                                            Some(json!({ "platform": platform })),
                                                        )
                                                        .await;
                                                    action_loading.set(false);
                                                    match result {
                                                        Ok(value) => {
                                                            let message = value
                                                                .get("message")
                                                                .and_then(Value::as_str)
                                                                .unwrap_or("Channel updated")
                                                                .to_string();
                                                            action_msg.set(Some(message));
                                                        }
                                                        Err(err) => action_msg.set(Some(format!("Start failed: {err}"))),
                                                    }
                                                    load_status().await;
                                                });
                                            },
                                            if *action_loading.read() { "Starting..." } else if configured { "Re-check Channel" } else { "Start Channel" }
                                        }
                                    }
                                    button {
                                        class: "btn channels-btn",
                                        disabled: *action_loading.read(),
                                        onclick: move |_| {
                                            let ws = ws.clone();
                                            spawn(async move {
                                                action_loading.set(true);
                                                let result = ws.read()
                                                    .call::<Value>(
                                                        "channels.test",
                                                        Some(json!({ "platform": platform })),
                                                    )
                                                    .await;
                                                action_loading.set(false);
                                                match result {
                                                    Ok(value) => {
                                                        let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
                                                        let message = value
                                                            .get("message")
                                                            .and_then(Value::as_str)
                                                            .unwrap_or(if ok { "Test passed" } else { "Test failed" })
                                                            .to_string();
                                                        action_msg.set(Some(message));
                                                    }
                                                    Err(err) => action_msg.set(Some(format!("Test failed: {err}"))),
                                                }
                                                load_status().await;
                                            });
                                        },
                                        if *action_loading.read() { "Working..." } else { "Test" }
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
