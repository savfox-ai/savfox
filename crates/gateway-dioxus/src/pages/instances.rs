use dioxus::prelude::*;
use lucide_dioxus::Server;
use serde_json::json;

use crate::api::ws::WsRpc;
use crate::components::empty_state::EmptyState;
use crate::components::skeleton::*;

/// Instances with no activity for more than 5 minutes are considered stale.
const STALE_THRESHOLD_SECS: u64 = 300;

#[derive(Clone, Debug, serde::Deserialize)]
struct PresenceEntry {
    host: Option<String>,
    mode: Option<String>,
    roles: Option<Vec<String>>,
    scopes: Option<Vec<String>>,
    platform: Option<String>,
    device_family: Option<String>,
    model_identifier: Option<String>,
    version: Option<String>,
    last_input_seconds: Option<u64>,
    reason: Option<String>,
    ts: Option<u64>,
}

#[component]
pub fn Instances() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut sort_by = use_signal(|| "activity".to_string());

    let ws_presence = ws.clone();
    let presence_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_presence.clone();
        async move {
            ws.call::<serde_json::Value>("system-presence", None)
                .await
                .ok()
        }
    });

    let presence_read = presence_data.read();
    let entries: Vec<PresenceEntry> = presence_read
        .as_ref()
        .and_then(|d| d.as_ref())
        .and_then(|d| d.get("entries"))
        .and_then(|e| serde_json::from_value(e.clone()).ok())
        .unwrap_or_default();

    let mut entries = entries;
    match sort_by().as_str() {
        "mode" => entries.sort_by(|a, b| {
            let ma = a.mode.as_deref().unwrap_or("");
            let mb = b.mode.as_deref().unwrap_or("");
            ma.cmp(mb).then_with(|| {
                let ta = a.ts.unwrap_or(0);
                let tb = b.ts.unwrap_or(0);
                tb.cmp(&ta)
            })
        }),
        _ /* "activity" */ => entries.sort_by(|a, b| {
            let ta = a.ts.unwrap_or(0);
            let tb = b.ts.unwrap_or(0);
            tb.cmp(&ta)
        }),
    }

    let is_loading = presence_data.read().is_none();
    // Use js_sys::Date for WASM (std::time::SystemTime panics on wasm32)
    let now = (js_sys::Date::now() / 1000.0) as u64;

    rsx! {
        div { class: "page-content",
            div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:24px;",
                div {
                    h2 { style: "font-size:20px;font-weight:600;", "Instances" }
                    p { style: "font-size:13px;color:var(--text-secondary);margin-top:4px;", "Presence beacons from the gateway and connected clients" }
                }
                div { style: "display:flex;gap:8px;align-items:center;",
                    select {
                        value: "{sort_by}",
                        onchange: move |e: Event<FormData>| sort_by.set(e.value().to_string()),
                        style: "padding:6px 10px;background:var(--bg-tertiary);color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:13px;",
                        option { value: "activity", "Sort: Activity" }
                        option { value: "mode", "Sort: Mode" }
                    }
                    button {
                        onclick: move |_| refresh_tick += 1,
                        class: "tool-btn",
                        "Refresh"
                    }
                }
            }

            if is_loading {
                div { style: "padding:40px;",
                    SkeletonCard {}
                }
            } else if entries.is_empty() {
                EmptyState {
                    icon: rsx! { Server { size: 20 } },
                    message: "No instances reported yet. Connected clients will appear here when they send presence beacons.".to_string(),
                }
            } else {
                div { style: "display:flex;flex-direction:column;gap:12px;",
                    for entry in entries.iter() {
                        { render_instance_card(entry, now, ws.clone(), refresh_tick) }
                    }
                }
            }
        }
    }
}

fn render_instance_card(
    entry: &PresenceEntry,
    now: u64,
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
) -> Element {
    let host = entry
        .host
        .clone()
        .unwrap_or_else(|| "unknown host".to_string());
    let mode = entry.mode.clone().unwrap_or_else(|| "unknown".to_string());
    let roles = entry.roles.clone().unwrap_or_default();
    let scopes = entry.scopes.clone().unwrap_or_default();
    let last_input = entry
        .last_input_seconds
        .map(|s| format!("{}s ago", s))
        .unwrap_or_else(|| "n/a".to_string());

    let elapsed = entry.ts.map(|ts| now.saturating_sub(ts)).unwrap_or(0);
    let age = entry
        .ts
        .map(|ts| {
            let elapsed = now.saturating_sub(ts);
            if elapsed < 60 {
                format!("{}s ago", elapsed)
            } else if elapsed < 3600 {
                format!("{}m ago", elapsed / 60)
            } else {
                format!("{}h ago", elapsed / 3600)
            }
        })
        .unwrap_or_else(|| "n/a".to_string());

    let is_stale = elapsed > STALE_THRESHOLD_SECS;

    let mode_color = match mode.as_str() {
        "cli" | "dashboard" => "var(--accent)",
        "gateway" => "var(--success)",
        "channel" => "var(--warning)",
        _ => "var(--text-secondary)",
    };

    let host_for_disconnect = host.clone();
    let host_for_kick = host.clone();

    let mut confirm_disconnect = use_signal(|| false);
    let mut confirm_kick = use_signal(|| false);

    rsx! {
        div { class: "ui-card",
            div { style: "display:flex;justify-content:space-between;align-items:flex-start;",
                div { style: "flex:1;",
                    div { style: "display:flex;align-items:center;gap:8px;margin-bottom:4px;",
                        span { style: "font-weight:600;font-size:16px;", "{host}" }
                        span { style: "font-size:11px;padding:2px 8px;border-radius:4px;background:{mode_color};color:#fff;font-weight:500;",
                            "{mode}"
                        }
                        if is_stale {
                            span { style: "font-size:11px;padding:2px 8px;border-radius:4px;background:var(--warning);color:#fff;font-weight:500;",
                                "Stale"
                            }
                        } else {
                            span {}
                        }
                    }
                    div { style: "font-size:13px;color:var(--text-secondary);margin-bottom:8px;",
                        if let Some(ref platform) = entry.platform {
                            span { "{platform}" }
                            span { style: "margin:0 4px;color:var(--text-muted);", "•" }
                        }
                        if let Some(ref device) = entry.device_family {
                            span { "{device}" }
                            span { style: "margin:0 4px;color:var(--text-muted);", "•" }
                        }
                        if let Some(ref version) = entry.version {
                            span { style: "font-family:var(--font-mono);", "v{version}" }
                        }
                    }
                    if let Some(ref model_identifier) = entry.model_identifier {
                        if !model_identifier.is_empty() {
                            div { style: "font-size:12px;color:var(--text-muted);margin-bottom:8px;font-family:var(--font-mono);",
                                "Model: {model_identifier}"
                            }
                        }
                    }
                    if !roles.is_empty() {
                        div { style: "display:flex;flex-wrap:wrap;gap:4px;margin-bottom:8px;",
                            for role in roles.iter() {
                                span { class: "chip chip--muted", "{role}" }
                            }
                        }
                    }
                    if !scopes.is_empty() {
                        div { style: "display:flex;flex-wrap:wrap;gap:4px;",
                            if scopes.len() > 3 {
                                span { class: "chip chip--muted", "{scopes.len()} scopes" }
                            } else {
                                for scope in scopes.iter() {
                                    span { class: "chip chip--muted", "{scope}" }
                                }
                            }
                        }
                    }
                }
                div { style: "text-align:right;font-size:12px;color:var(--text-muted);",
                    div { style: "margin-bottom:2px;", "Last seen: {age}" }
                    div { style: "margin-bottom:2px;", "Last input: {last_input}" }
                    if let Some(ref reason) = entry.reason {
                        if !reason.is_empty() {
                            div { "Reason: {reason}" }
                        }
                    }
                }
            }

            // Instance operations
            div { style: "display:flex;gap:8px;align-items:center;margin-top:12px;padding-top:12px;border-top:1px solid var(--border);",
                if confirm_disconnect() {
                    div { style: "display:flex;gap:8px;align-items:center;",
                        span { style: "font-size:13px;color:var(--warning);font-weight:500;", "Disconnect this instance?" }
                        button {
                            class: "tool-btn tool-btn--danger",
                            onclick: {
                                let ws_dc = ws.clone();
                                let h = host_for_disconnect.clone();
                                move |_| {
                                    let ws = ws_dc.clone();
                                    let host = h.clone();
                                    spawn(async move {
                                        let _ = ws.call::<serde_json::Value>(
                                            "system-disconnect",
                                            Some(json!({ "host": host })),
                                        ).await;
                                        refresh_tick += 1;
                                    });
                                    confirm_disconnect.set(false);
                                }
                            },
                            "Yes, disconnect"
                        }
                        button {
                            class: "tool-btn",
                            onclick: move |_| confirm_disconnect.set(false),
                            "Cancel"
                        }
                    }
                } else if confirm_kick() {
                    div { style: "display:flex;gap:8px;align-items:center;",
                        span { style: "font-size:13px;color:var(--danger);font-weight:500;", "Kick this instance?" }
                        button {
                            class: "tool-btn tool-btn--danger",
                            onclick: {
                                let ws_k = ws.clone();
                                let h = host_for_kick.clone();
                                move |_| {
                                    let ws = ws_k.clone();
                                    let host = h.clone();
                                    spawn(async move {
                                        let _ = ws.call::<serde_json::Value>(
                                            "system-kick",
                                            Some(json!({ "host": host })),
                                        ).await;
                                        refresh_tick += 1;
                                    });
                                    confirm_kick.set(false);
                                }
                            },
                            "Yes, kick"
                        }
                        button {
                            class: "tool-btn",
                            onclick: move |_| confirm_kick.set(false),
                            "Cancel"
                        }
                    }
                } else {
                    button {
                        class: "tool-btn",
                        onclick: move |_| {
                            confirm_kick.set(false);
                            confirm_disconnect.set(true);
                        },
                        "Disconnect"
                    }
                    button {
                        class: "tool-btn tool-btn--danger",
                        onclick: move |_| {
                            confirm_disconnect.set(false);
                            confirm_kick.set(true);
                        },
                        "Kick"
                    }
                }
            }
        }
    }
}
