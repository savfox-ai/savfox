use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::{ApprovalRequest, ApprovalSettings, ApprovalsResponse};
use crate::api::ws::WsRpc;

#[component]
pub fn Approvals() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);

    let ws_settings = ws.clone();
    let settings_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_settings.clone();
        async move {
            ws.call::<ApprovalSettings>("exec.approvals.get", None)
                .await
                .ok()
        }
    });

    let ws_pending = ws.clone();
    let pending_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_pending.clone();
        async move {
            ws.call::<ApprovalsResponse>("exec.approval.request", None)
                .await
                .map(|r| r.pending)
                .unwrap_or_default()
        }
    });

    let settings_read = settings_data.read();
    let settings = settings_read.as_ref().and_then(|s| s.as_ref());
    let pending: Vec<ApprovalRequest> = pending_data.read().as_ref().cloned().unwrap_or_default();

    let enabled = settings.and_then(|s| s.enabled).unwrap_or(false);
    let auto_safe = settings.and_then(|s| s.auto_approve_safe).unwrap_or(false);
    let timeout = settings
        .and_then(|s| s.timeout_secs)
        .map(|t| format!("{t}s"))
        .unwrap_or_else(|| "-".into());

    rsx! {
        div { style: "padding:24px;max-width:900px;",
            div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:24px;",
                h2 { style: "font-size:20px;font-weight:600;", "Approvals" }
                button {
                    onclick: move |_| refresh_tick += 1,
                    style: "padding:6px 14px;background:var(--bg-tertiary);color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:13px;",
                    "Refresh"
                }
            }

            // Settings card
            div { style: "{CARD}",
                h3 { style: "{CARD_TITLE}", "Settings" }
                div { style: "display:flex;flex-direction:column;gap:8px;",
                    { setting_row("Approvals Enabled", &enabled.to_string(), "exec.approvals.set", "enabled", !enabled, ws.clone(), refresh_tick) }
                    { setting_row("Auto-approve Safe", &auto_safe.to_string(), "exec.approvals.set", "auto_approve_safe", !auto_safe, ws.clone(), refresh_tick) }
                    div { style: "display:flex;justify-content:space-between;padding:6px 0;",
                        span { style: "color:var(--text-secondary);font-size:14px;", "Timeout" }
                        span { style: "font-family:monospace;font-size:13px;", "{timeout}" }
                    }
                }
            }

            // Pending approvals
            div { style: "{CARD}",
                h3 { style: "{CARD_TITLE}", "Pending Approvals" }
                if pending.is_empty() {
                    p { style: "color:var(--text-muted);font-size:14px;", "No pending approvals" }
                } else {
                    div { style: "display:flex;flex-direction:column;gap:8px;",
                        for req in pending.iter() {
                            {
                                let id_approve = req.id.clone();
                                let id_reject = req.id.clone();
                                let ws_approve = ws.clone();
                                let ws_reject = ws.clone();
                                let command = req.command.as_deref().unwrap_or("-").to_string();
                                let node = req.node.as_deref().unwrap_or("-").to_string();
                                let ts = req.timestamp.as_deref().unwrap_or("").to_string();
                                rsx! {
                                    div {
                                        key: "{req.id}",
                                        style: "padding:12px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);",
                                        div { style: "display:flex;justify-content:space-between;align-items:flex-start;margin-bottom:8px;",
                                            div {
                                                div { style: "font-weight:500;font-size:14px;margin-bottom:2px;",
                                                    code { "{command}" }
                                                }
                                                div { style: "font-size:12px;color:var(--text-muted);",
                                                    "Node: {node}"
                                                    if !ts.is_empty() { " | {ts}" }
                                                }
                                            }
                                            div { style: "display:flex;gap:6px;",
                                                button {
                                                    onclick: move |_| {
                                                        let id = id_approve.clone();
                                                        let ws = ws_approve.clone();
                                                        spawn(async move {
                                                            let _ = ws.call::<serde_json::Value>("exec.approval.resolve", Some(json!({ "id": id, "approved": true }))).await;
                                                            refresh_tick += 1;
                                                        });
                                                    },
                                                    style: "{ACTION_BTN}background:var(--success);color:#fff;border:none;",
                                                    "Approve"
                                                }
                                                button {
                                                    onclick: move |_| {
                                                        let id = id_reject.clone();
                                                        let ws = ws_reject.clone();
                                                        spawn(async move {
                                                            let _ = ws.call::<serde_json::Value>("exec.approval.resolve", Some(json!({ "id": id, "approved": false }))).await;
                                                            refresh_tick += 1;
                                                        });
                                                    },
                                                    style: "{ACTION_BTN}color:var(--danger);border-color:var(--danger);",
                                                    "Reject"
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
        }
    }
}

fn setting_row(
    label: &str,
    value: &str,
    method: &str,
    field: &str,
    toggle_val: bool,
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
) -> Element {
    let method = method.to_string();
    let field = field.to_string();
    let bg = if value == "true" {
        "var(--success)"
    } else {
        "transparent"
    };
    let color = if value == "true" {
        "#fff"
    } else {
        "var(--text-secondary)"
    };
    rsx! {
        div { style: "display:flex;justify-content:space-between;align-items:center;padding:6px 0;",
            span { style: "color:var(--text-secondary);font-size:14px;", "{label}" }
            button {
                onclick: move |_| {
                    let ws = ws.clone();
                    let m = method.clone();
                    let f = field.clone();
                    spawn(async move {
                        let _ = ws.call::<serde_json::Value>(&m, Some(json!({ f: toggle_val }))).await;
                        refresh_tick += 1;
                    });
                },
                style: "padding:3px 12px;background:{bg};color:{color};border:1px solid var(--border);border-radius:var(--radius);font-size:12px;font-family:monospace;",
                "{value}"
            }
        }
    }
}

const CARD: &str = "background:var(--bg-secondary);border:1px solid var(--border);border-radius:var(--radius);padding:20px;margin-bottom:16px;";
const CARD_TITLE: &str = "font-size:14px;font-weight:600;margin-bottom:12px;color:var(--text-secondary);text-transform:uppercase;letter-spacing:0.05em;";
const ACTION_BTN: &str = "padding:4px 12px;background:transparent;color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:12px;";
