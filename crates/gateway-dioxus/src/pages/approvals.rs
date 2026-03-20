use std::collections::HashSet;

use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::{ApprovalRequest, ApprovalSettings, ApprovalsResponse};
use crate::api::ws::WsRpc;
use crate::components::empty_state::EmptyState;

#[component]
pub fn Approvals() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut selected = use_signal(|| HashSet::<String>::new());

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

    // Derive selection state
    let selected_count = selected.read().len();
    let all_ids: Vec<String> = pending.iter().map(|r| r.id.clone()).collect();
    let all_selected = !pending.is_empty() && selected_count == pending.len();
    let has_selection = selected_count > 0;

    rsx! {
        div { class: "page-content",
            div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:24px;",
                h2 { style: "font-size:20px;font-weight:600;", "Approvals" }
                button {
                    onclick: move |_| refresh_tick += 1,
                    class: "tool-btn",
                    "Refresh"
                }
            }

            // Settings card
            div { class: "ui-card",
                h3 { class: "ui-card__title", "Settings" }
                div { style: "display:flex;flex-direction:column;gap:8px;",
                    { setting_row("Approvals Enabled", &enabled.to_string(), "exec.approvals.set", "enabled", !enabled, ws.clone(), refresh_tick) }
                    { setting_row("Auto-approve Safe", &auto_safe.to_string(), "exec.approvals.set", "auto_approve_safe", !auto_safe, ws.clone(), refresh_tick) }
                    div { style: "display:flex;justify-content:space-between;padding:6px 0;",
                        span { style: "color:var(--text-secondary);font-size:14px;", "Timeout" }
                        span { style: "font-family:var(--font-mono);font-size:13px;", "{timeout}" }
                    }
                }
            }

            // Pending approvals
            div { class: "ui-card",
                div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:12px;",
                    h3 { class: "ui-card__title", style: "margin-bottom:0;", "Pending Approvals" }

                    if has_selection {
                        div { style: "display:flex;gap:8px;align-items:center;",
                            span { style: "font-size:13px;color:var(--text-secondary);font-weight:500;",
                                "{selected_count} selected"
                            }
                            button {
                                class: "tool-btn tool-btn--success",
                                onclick: {
                                    let ws_batch = ws.clone();
                                    let mut selected_clone = selected;
                                    move |_| {
                                        let ids: Vec<String> = selected_clone.read().iter().cloned().collect();
                                        let ws = ws_batch.clone();
                                        spawn(async move {
                                            for id in ids {
                                                let _ = ws.call::<serde_json::Value>(
                                                    "exec.approval.resolve",
                                                    Some(json!({ "id": id, "approved": true })),
                                                ).await;
                                            }
                                            selected_clone.set(HashSet::new());
                                            refresh_tick += 1;
                                        });
                                    }
                                },
                                "Approve All"
                            }
                            button {
                                class: "tool-btn tool-btn--danger",
                                onclick: {
                                    let ws_batch = ws.clone();
                                    let mut selected_clone = selected;
                                    move |_| {
                                        let ids: Vec<String> = selected_clone.read().iter().cloned().collect();
                                        let ws = ws_batch.clone();
                                        spawn(async move {
                                            for id in ids {
                                                let _ = ws.call::<serde_json::Value>(
                                                    "exec.approval.resolve",
                                                    Some(json!({ "id": id, "approved": false })),
                                                ).await;
                                            }
                                            selected_clone.set(HashSet::new());
                                            refresh_tick += 1;
                                        });
                                    }
                                },
                                "Reject All"
                            }
                        }
                    } else {
                        div {}
                    }
                }

                if pending.is_empty() {
                    EmptyState {
                        icon: rsx! { "!" },
                        message: "No pending approvals".to_string(),
                    }
                } else {
                    div { style: "display:flex;flex-direction:column;gap:8px;",
                        // Select All header row
                        div { style: "display:flex;align-items:center;gap:8px;padding:4px 0;border-bottom:1px solid var(--border);margin-bottom:4px;",
                            input {
                                r#type: "checkbox",
                                checked: all_selected,
                                onchange: {
                                    let all_ids_clone = all_ids.clone();
                                    move |_| {
                                        if all_selected {
                                            selected.set(HashSet::new());
                                        } else {
                                            selected.set(all_ids_clone.iter().cloned().collect());
                                        }
                                    }
                                },
                                style: "cursor:pointer;width:16px;height:16px;accent-color:var(--accent);",
                            }
                            span { style: "font-size:13px;color:var(--text-secondary);font-weight:500;", "Select All" }
                        }

                        for req in pending.iter() {
                            {
                                let id_approve = req.id.clone();
                                let id_reject = req.id.clone();
                                let id_select = req.id.clone();
                                let ws_approve = ws.clone();
                                let ws_reject = ws.clone();
                                let command = req.command.as_deref().unwrap_or("-").to_string();
                                let node = req.node.as_deref().unwrap_or("-").to_string();
                                let ts = req.timestamp.as_deref().unwrap_or("").to_string();
                                let is_checked = selected.read().contains(&req.id);
                                rsx! {
                                    div {
                                        key: "{req.id}",
                                        style: "display:flex;align-items:flex-start;gap:8px;padding:12px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);",
                                        input {
                                            r#type: "checkbox",
                                            checked: is_checked,
                                            onchange: move |_| {
                                                let mut set = selected.read().clone();
                                                if set.contains(&id_select) {
                                                    set.remove(&id_select);
                                                } else {
                                                    set.insert(id_select.clone());
                                                }
                                                selected.set(set);
                                            },
                                            style: "cursor:pointer;width:16px;height:16px;margin-top:2px;accent-color:var(--accent);flex-shrink:0;",
                                        }
                                        div { style: "flex:1;",
                                            div { style: "display:flex;justify-content:space-between;align-items:flex-start;margin-bottom:8px;",
                                                div {
                                                    div { style: "font-weight:500;font-size:14px;margin-bottom:2px;",
                                                        code { {render_command_preview(&command)} }
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
                                                        class: "tool-btn tool-btn--success",
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
                                                        class: "tool-btn tool-btn--danger",
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
                style: "padding:3px 12px;background:{bg};color:{color};border:1px solid var(--border);border-radius:var(--radius);font-size:12px;font-family:var(--font-mono);",
                "{value}"
            }
        }
    }
}

const DANGER_KEYWORDS: &[&str] = &["rm", "delete", "drop", "truncate", "destroy"];

fn render_command_preview(command: &str) -> Element {
    let mut fragments: Vec<Element> = Vec::new();
    let words: Vec<&str> = command.split_whitespace().collect();
    for (i, word) in words.iter().enumerate() {
        if i > 0 {
            fragments.push(rsx! { " " });
        }
        let lower = word.to_lowercase();
        let is_danger = DANGER_KEYWORDS
            .iter()
            .any(|kw| lower == *kw || lower.starts_with(&format!("{kw} ")) || lower.contains(kw));
        if is_danger {
            fragments.push(rsx! {
                span { style: "color:var(--danger);font-weight:600;", "{word}" }
            });
        } else {
            fragments.push(rsx! { "{word}" });
        }
    }
    rsx! { {fragments.into_iter()} }
}
