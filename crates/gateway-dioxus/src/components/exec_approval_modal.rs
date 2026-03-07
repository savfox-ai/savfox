use dioxus::prelude::*;
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::types::ExecApprovalFull;
use crate::api::ws::WsRpc;

/// Format remaining seconds as a human-readable countdown string.
fn format_countdown(remaining_s: u64) -> String {
    if remaining_s >= 3600 {
        format!("{}h {}m", remaining_s / 3600, (remaining_s % 3600) / 60)
    } else if remaining_s >= 60 {
        format!("{}m {}s", remaining_s / 60, remaining_s % 60)
    } else {
        format!("{}s", remaining_s)
    }
}

/// Compute the countdown display text from a deadline timestamp (ms since epoch).
fn compute_countdown(expires_ms: u64) -> String {
    let now = js_sys::Date::now() as u64;
    if expires_ms > now {
        format_countdown((expires_ms - now) / 1000)
    } else {
        "Expired".to_string()
    }
}

#[component]
pub fn ExecApprovalModal(
    approvals: Vec<ExecApprovalFull>,
    on_dismiss: EventHandler<()>,
) -> Element {
    let ws = use_context::<WsRpc>();
    let mut current_index = use_signal(|| 0usize);
    let mut countdown_text = use_signal(|| String::from("--"));
    // Track the JS interval handle without making it a reactive dependency.
    let interval_handle = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(None::<i32>)));

    let idx = current_index().min(approvals.len().saturating_sub(1));
    let expires = approvals.get(idx).and_then(|a| a.expires_at_ms);

    // Countdown timer — clears the previous interval before creating a new one.
    let interval_handle_effect = interval_handle.clone();
    use_effect(move || {
        // Tear down any previously-running interval.
        if let Some(handle) = interval_handle_effect.replace(None) {
            if let Some(w) = web_sys::window() {
                w.clear_interval_with_handle(handle);
            }
        }

        let Some(exp) = expires else {
            countdown_text.set("--".into());
            return;
        };

        // Show the value immediately so we don't flash "--" for one second.
        countdown_text.set(compute_countdown(exp));

        let cb = Closure::wrap(Box::new(move || {
            countdown_text.set(compute_countdown(exp));
        }) as Box<dyn FnMut()>);

        if let Some(w) = web_sys::window() {
            if let Ok(id) = w.set_interval_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                1000,
            ) {
                interval_handle_effect.set(Some(id));
            }
        }
        cb.forget(); // prevent GC; cleaned up via clear_interval above
    });

    // Clean up the interval when the component is unmounted.
    let interval_handle_drop = interval_handle.clone();
    use_drop(move || {
        if let Some(handle) = interval_handle_drop.replace(None) {
            if let Some(w) = web_sys::window() {
                w.clear_interval_with_handle(handle);
            }
        }
    });

    if approvals.is_empty() {
        return rsx! {};
    }

    let approval = &approvals[idx];
    let queue_count = approvals.len();

    let id_allow = approval.id.clone();
    let id_always = approval.id.clone();
    let id_deny = approval.id.clone();
    let ws_allow = ws.clone();
    let ws_always = ws.clone();
    let ws_deny = ws.clone();

    rsx! {
        div { class: "modal-backdrop",
            role: "dialog",
            aria_modal: "true",
            aria_labelledby: "approval-dialog-title",
            aria_live: "polite",
            div { class: "modal-card",
                // Header
                div { class: "modal-card__header",
                    h3 { id: "approval-dialog-title", "Exec Approval Required" }
                    div { style: "display:flex;align-items:center;gap:8px;",
                        if queue_count > 1 {
                            span { class: "chip chip--info", "{queue_count} pending" }
                        }
                        span { class: "countdown", "{countdown_text}" }
                    }
                }

                // Command display
                div { class: "modal-card__body",
                    if let Some(ref cmd) = approval.command {
                        pre { class: "modal-card__command", "{cmd}" }
                    }

                    div { class: "modal-card__meta",
                        if let Some(ref host) = approval.host {
                            div { class: "modal-card__meta-row",
                                span { class: "modal-card__meta-label", "Host" }
                                span { "{host}" }
                            }
                        }
                        if let Some(ref agent) = approval.agent_id {
                            div { class: "modal-card__meta-row",
                                span { class: "modal-card__meta-label", "Agent" }
                                span { "{agent}" }
                            }
                        }
                        if let Some(ref session) = approval.session_id {
                            div { class: "modal-card__meta-row",
                                span { class: "modal-card__meta-label", "Session" }
                                code { style: "font-size:12px;", "{session}" }
                            }
                        }
                        if let Some(ref cwd) = approval.cwd {
                            div { class: "modal-card__meta-row",
                                span { class: "modal-card__meta-label", "CWD" }
                                code { style: "font-size:12px;", "{cwd}" }
                            }
                        }
                        if let Some(ref security) = approval.security {
                            div { class: "modal-card__meta-row",
                                span { class: "modal-card__meta-label", "Security" }
                                span { "{security}" }
                            }
                        }
                        if let Some(ref ask) = approval.ask {
                            div { class: "modal-card__meta-row",
                                span { class: "modal-card__meta-label", "Ask Policy" }
                                span { "{ask}" }
                            }
                        }
                    }
                }

                // Actions
                div { class: "modal-card__actions",
                    button {
                        class: "btn primary",
                        onclick: move |_| {
                            let id = id_allow.clone();
                            let ws = ws_allow.clone();
                            spawn(async move {
                                let _ = ws.call::<serde_json::Value>(
                                    "approvals.respond",
                                    Some(json!({ "id": id, "action": "allow_once" })),
                                ).await;
                            });
                        },
                        "Allow Once"
                    }
                    button {
                        class: "btn",
                        style: "background:var(--success);color:#fff;border-color:var(--success);",
                        onclick: move |_| {
                            let id = id_always.clone();
                            let ws = ws_always.clone();
                            spawn(async move {
                                let _ = ws.call::<serde_json::Value>(
                                    "approvals.respond",
                                    Some(json!({ "id": id, "action": "always_allow" })),
                                ).await;
                            });
                        },
                        "Always Allow"
                    }
                    button {
                        class: "btn",
                        style: "color:var(--danger);border-color:var(--danger);",
                        onclick: move |_| {
                            let id = id_deny.clone();
                            let ws = ws_deny.clone();
                            spawn(async move {
                                let _ = ws.call::<serde_json::Value>(
                                    "approvals.respond",
                                    Some(json!({ "id": id, "action": "deny" })),
                                ).await;
                            });
                        },
                        "Deny"
                    }
                }

                // Queue navigation
                if queue_count > 1 {
                    div { style: "display:flex;justify-content:center;gap:8px;padding:8px 0;",
                        button {
                            class: "btn btn--sm",
                            disabled: idx == 0,
                            onclick: move |_| { if current_index() > 0 { current_index -= 1; } },
                            "Prev"
                        }
                        span { style: "font-size:12px;color:var(--text-muted);line-height:32px;",
                            "{idx + 1} / {queue_count}"
                        }
                        button {
                            class: "btn btn--sm",
                            disabled: idx + 1 >= queue_count,
                            onclick: move |_| { current_index += 1; },
                            "Next"
                        }
                    }
                }
            }
        }
    }
}
