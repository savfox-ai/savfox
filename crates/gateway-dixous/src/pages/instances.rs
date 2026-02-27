use dioxus::prelude::*;

use crate::api::ws::WsRpc;

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

    let is_loading = presence_data.read().is_none();
    // Use js_sys::Date for WASM (std::time::SystemTime panics on wasm32)
    let now = (js_sys::Date::now() / 1000.0) as u64;

    rsx! {
        div { style: "padding:24px;max-width:1000px;",
            div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:24px;",
                div {
                    h2 { style: "font-size:20px;font-weight:600;", "Instances" }
                    p { style: "font-size:13px;color:var(--text-secondary);margin-top:4px;", "Presence beacons from the gateway and connected clients" }
                }
                button {
                    onclick: move |_| refresh_tick += 1,
                    style: "padding:6px 14px;background:var(--bg-tertiary);color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:13px;",
                    "Refresh"
                }
            }

            if is_loading {
                div { style: "text-align:center;padding:40px;color:var(--text-muted);", "Loading instances..." }
            } else if entries.is_empty() {
                div { style: "{CARD}text-align:center;padding:40px;",
                    div { style: "font-size:16px;color:var(--text-muted);margin-bottom:8px;", "No instances reported yet" }
                    p { style: "font-size:13px;color:var(--text-muted);", "Connected clients will appear here when they send presence beacons." }
                }
            } else {
                div { style: "display:flex;flex-direction:column;gap:12px;",
                    for entry in entries.iter() {
                        { render_instance_card(entry, now) }
                    }
                }
            }
        }
    }
}

fn render_instance_card(entry: &PresenceEntry, now: u64) -> Element {
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

    let mode_color = match mode.as_str() {
        "cli" | "dashboard" => "var(--accent)",
        "gateway" => "var(--success)",
        "bridge" => "var(--warning)",
        _ => "var(--text-secondary)",
    };

    rsx! {
        div { style: CARD,
            div { style: "display:flex;justify-content:space-between;align-items:flex-start;",
                div { style: "flex:1;",
                    div { style: "display:flex;align-items:center;gap:8px;margin-bottom:4px;",
                        span { style: "font-weight:600;font-size:16px;", "{host}" }
                        span { style: "font-size:11px;padding:2px 8px;border-radius:4px;background:{mode_color};color:#fff;font-weight:500;",
                            "{mode}"
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
                            span { style: "font-family:monospace;", "v{version}" }
                        }
                    }
                    if let Some(ref model_identifier) = entry.model_identifier {
                        if !model_identifier.is_empty() {
                            div { style: "font-size:12px;color:var(--text-muted);margin-bottom:8px;font-family:monospace;",
                                "Model: {model_identifier}"
                            }
                        }
                    }
                    if !roles.is_empty() {
                        div { style: "display:flex;flex-wrap:wrap;gap:4px;margin-bottom:8px;",
                            for role in roles.iter() {
                                span { style: CHIP, "{role}" }
                            }
                        }
                    }
                    if !scopes.is_empty() {
                        div { style: "display:flex;flex-wrap:wrap;gap:4px;",
                            if scopes.len() > 3 {
                                span { style: CHIP, "{scopes.len()} scopes" }
                            } else {
                                for scope in scopes.iter() {
                                    span { style: CHIP, "{scope}" }
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
        }
    }
}

const CARD: &str = "background:var(--bg-secondary);border:1px solid var(--border);border-radius:var(--radius);padding:16px;";
const CHIP: &str = "font-size:11px;padding:2px 8px;border-radius:4px;background:var(--bg-tertiary);color:var(--text-secondary);";
