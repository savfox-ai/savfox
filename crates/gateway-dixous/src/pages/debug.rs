use dioxus::prelude::*;
use serde_json::json;

use crate::api::client::fetch_json;
use crate::api::ws::WsRpc;

#[derive(Clone, Debug, serde::Deserialize)]
struct HealthResponse {
    status: String,
    version: String,
}

#[derive(Clone, Debug, PartialEq)]
struct RpcExample {
    label: &'static str,
    method: &'static str,
    params: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
struct RpcCallHistoryEntry {
    ts_ms: u64,
    method: String,
    params: String,
    success: bool,
    preview: String,
}

const RPC_EXAMPLES: &[RpcExample] = &[
    RpcExample {
        label: "status",
        method: "status",
        params: "{}",
    },
    RpcExample {
        label: "models.list",
        method: "models.list",
        params: "{}",
    },
    RpcExample {
        label: "sessions.list",
        method: "sessions.list",
        params: "{}",
    },
    RpcExample {
        label: "system-presence",
        method: "system-presence",
        params: "{}",
    },
    RpcExample {
        label: "config.get",
        method: "config.get",
        params: "{}",
    },
];

#[component]
pub fn Debug() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut call_method = use_signal(String::new);
    let mut call_params = use_signal(String::new);
    let mut call_result = use_signal(|| Option::<String>::None);
    let mut call_error = use_signal(|| Option::<String>::None);
    let mut editor_error = use_signal(|| Option::<String>::None);
    let mut calling = use_signal(|| false);
    let mut call_history = use_signal(Vec::<RpcCallHistoryEntry>::new);

    let ws_status = ws.clone();
    let status_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_status.clone();
        async move { ws.call::<serde_json::Value>("status", None).await.ok() }
    });

    let health_data = use_resource(move || {
        let _t = refresh_tick();
        async move {
            fetch_json::<HealthResponse>("GET", "/health", None)
                .await
                .ok()
        }
    });

    let ws_models = ws.clone();
    let models_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_models.clone();
        async move { ws.call::<serde_json::Value>("models.list", None).await.ok() }
    });

    let status_json = status_data
        .read()
        .as_ref()
        .and_then(|s| s.as_ref())
        .map(|s| serde_json::to_string_pretty(s).unwrap_or_default())
        .unwrap_or_else(|| "No status data".to_string());

    let health_json = health_data
        .read()
        .as_ref()
        .and_then(|h| h.as_ref())
        .map(|h| {
            serde_json::to_string_pretty(&json!({"status": h.status, "version": h.version}))
                .unwrap_or_default()
        })
        .unwrap_or_else(|| "No health data".to_string());

    let models_json = models_data
        .read()
        .as_ref()
        .and_then(|m| m.as_ref())
        .map(|m| serde_json::to_string_pretty(m).unwrap_or_default())
        .unwrap_or_else(|| "No models data".to_string());

    let history = call_history();

    let mut do_call = {
        let ws = ws.clone();
        move || {
            let method = call_method().trim().to_string();
            let params_str = call_params();

            if method.is_empty() {
                call_error.set(Some("Method is required".to_string()));
                return;
            }

            let params = match parse_params_input(&params_str) {
                Ok(params) => params,
                Err(err) => {
                    editor_error.set(Some(err));
                    return;
                }
            };

            let ws = ws.clone();
            calling.set(true);
            call_result.set(None);
            call_error.set(None);
            editor_error.set(None);
            spawn(async move {
                match ws.call::<serde_json::Value>(&method, params).await {
                    Ok(result) => {
                        let pretty = serde_json::to_string_pretty(&result).unwrap_or_default();
                        call_result.set(Some(pretty.clone()));
                        let mut next = call_history();
                        prepend_history_entry(
                            &mut next,
                            RpcCallHistoryEntry {
                                ts_ms: js_sys::Date::now() as u64,
                                method: method.clone(),
                                params: params_str.clone(),
                                success: true,
                                preview: compact_preview(&pretty),
                            },
                        );
                        call_history.set(next);
                    }
                    Err(e) => {
                        let msg = format!("Error: {e}");
                        call_error.set(Some(msg.clone()));
                        let mut next = call_history();
                        prepend_history_entry(
                            &mut next,
                            RpcCallHistoryEntry {
                                ts_ms: js_sys::Date::now() as u64,
                                method: method.clone(),
                                params: params_str.clone(),
                                success: false,
                                preview: compact_preview(&msg),
                            },
                        );
                        call_history.set(next);
                    }
                }
                calling.set(false);
            });
        }
    };

    rsx! {
        div { style: "padding:24px;",
            div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:24px;",
                h2 { style: "font-size:20px;font-weight:600;", "Debug" }
                button {
                    onclick: move |_| refresh_tick += 1,
                    style: "padding:6px 14px;background:var(--bg-tertiary);color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:13px;",
                    "Refresh"
                }
            }

            div { style: "display:grid;grid-template-columns:1fr 1fr;gap:16px;",
                // Snapshots
                div { style: CARD,
                    h3 { style: CARD_TITLE, "Snapshots" }
                    div { style: "margin-bottom:12px;",
                        div { style: "font-size:12px;color:var(--text-muted);margin-bottom:4px;", "Status" }
                        pre { style: CODE_BLOCK, "{status_json}" }
                    }
                    div { style: "margin-bottom:12px;",
                        div { style: "font-size:12px;color:var(--text-muted);margin-bottom:4px;", "Health" }
                        pre { style: CODE_BLOCK, "{health_json}" }
                    }
                }

                // Manual RPC
                div { style: CARD,
                    h3 { style: CARD_TITLE, "Manual RPC" }
                    p { style: "font-size:13px;color:var(--text-muted);margin-bottom:12px;", "Send a raw gateway method with JSON params" }
                    div { style: "margin-bottom:12px;",
                        div { style: "font-size:12px;font-weight:600;color:var(--text-secondary);margin-bottom:6px;", "Examples" }
                        div { style: "display:flex;flex-wrap:wrap;gap:6px;",
                            for ex in RPC_EXAMPLES.iter() {
                                {
                                    let method = ex.method.to_string();
                                    let params = ex.params.to_string();
                                    rsx! {
                                        button {
                                            key: "{ex.label}",
                                            onclick: move |_| {
                                                call_method.set(method.clone());
                                                call_params.set(params.clone());
                                                editor_error.set(None);
                                            },
                                            style: SECONDARY_BUTTON,
                                            "{ex.label}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { style: "margin-bottom:12px;",
                        label { style: LABEL, "Method" }
                        input {
                            value: "{call_method}",
                            oninput: move |e| call_method.set(e.value()),
                            placeholder: "system-presence",
                            style: INPUT,
                        }
                    }
                    div { style: "margin-bottom:12px;",
                        div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:4px;",
                            label { style: LABEL, "Params (JSON)" }
                            div { style: "display:flex;gap:6px;",
                                button {
                                    onclick: move |_| {
                                        let current = call_params();
                                        if current.trim().is_empty() {
                                            call_params.set("{}".to_string());
                                            editor_error.set(None);
                                            return;
                                        }
                                        match serde_json::from_str::<serde_json::Value>(&current) {
                                            Ok(value) => {
                                                call_params.set(serde_json::to_string_pretty(&value).unwrap_or_else(|_| current.clone()));
                                                editor_error.set(None);
                                            }
                                            Err(err) => editor_error.set(Some(format!("Invalid JSON: {err}"))),
                                        }
                                    },
                                    style: SECONDARY_BUTTON,
                                    "Format JSON"
                                }
                                button {
                                    onclick: move |_| {
                                        call_params.set("{}".to_string());
                                        editor_error.set(None);
                                    },
                                    style: SECONDARY_BUTTON,
                                    "Reset"
                                }
                            }
                        }
                        textarea {
                            value: "{call_params}",
                            oninput: move |e| {
                                call_params.set(e.value());
                                editor_error.set(None);
                            },
                            placeholder: "{{ }}",
                            rows: 6,
                            style: "{INPUT}font-family:monospace;font-size:12px;resize:vertical;",
                        }
                        if let Some(ref err) = editor_error() {
                            div { style: "margin-top:8px;color:var(--danger);font-size:12px;", "{err}" }
                        }
                    }
                    button {
                        disabled: calling() || call_method().trim().is_empty(),
                        onclick: move |_| do_call(),
                        style: "padding:8px 20px;background:var(--accent);color:#fff;border:none;border-radius:var(--radius);font-size:13px;",
                        if calling() { "Calling..." } else { "Call" }
                    }
                    if let Some(ref err) = call_error() {
                        div { style: "margin-top:12px;padding:8px 12px;background:rgba(239,68,68,0.1);border:1px solid var(--danger);border-radius:var(--radius);color:var(--danger);font-size:13px;",
                            "{err}"
                        }
                    }
                    if let Some(ref result) = call_result() {
                        pre { style: "{CODE_BLOCK}margin-top:12px;max-height:300px;", "{result}" }
                    }
                    if !history.is_empty() {
                        div { style: "margin-top:14px;",
                            div { style: "font-size:12px;font-weight:600;color:var(--text-secondary);margin-bottom:6px;", "Recent Calls" }
                            div { style: "display:flex;flex-direction:column;gap:6px;max-height:230px;overflow:auto;",
                                for (idx, item) in history.iter().enumerate() {
                                    {
                                        let method = item.method.clone();
                                        let params = item.params.clone();
                                        let status_color = if item.success { "var(--success)" } else { "var(--danger)" };
                                        let age = format_history_age(item.ts_ms);
                                        rsx! {
                                            button {
                                                key: "{idx}-{item.ts_ms}",
                                                onclick: move |_| {
                                                    call_method.set(method.clone());
                                                    call_params.set(params.clone());
                                                    editor_error.set(None);
                                                },
                                                style: "text-align:left;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);padding:8px 10px;color:var(--text-primary);cursor:pointer;",
                                                div { style: "display:flex;justify-content:space-between;align-items:center;gap:8px;",
                                                    div { style: "font-size:12px;font-weight:600;overflow:hidden;text-overflow:ellipsis;", "{item.method}" }
                                                    div { style: "display:flex;align-items:center;gap:8px;flex-shrink:0;",
                                                        span { style: "font-size:11px;color:{status_color};", if item.success { "ok" } else { "error" } }
                                                        span { style: "font-size:11px;color:var(--text-muted);", "{age}" }
                                                    }
                                                }
                                                div { style: "margin-top:4px;font-size:11px;color:var(--text-muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;", "{item.preview}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Models
            div { style: "{CARD}margin-top:16px;",
                h3 { style: CARD_TITLE, "Models Catalog" }
                pre { style: CODE_BLOCK, "{models_json}" }
            }
        }
    }
}

const CARD: &str = "background:var(--bg-secondary);border:1px solid var(--border);border-radius:var(--radius);padding:20px;";
const CARD_TITLE: &str = "font-size:14px;font-weight:600;margin-bottom:12px;color:var(--text-secondary);text-transform:uppercase;letter-spacing:0.05em;";
const INPUT: &str = "width:100%;padding:8px 12px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);color:var(--text-primary);outline:none;font-size:14px;";
const LABEL: &str =
    "display:block;font-size:12px;font-weight:600;color:var(--text-secondary);margin-bottom:4px;";
const CODE_BLOCK: &str = "background:var(--bg-tertiary);padding:12px;border-radius:var(--radius);font-size:12px;overflow:auto;white-space:pre-wrap;word-break:break-all;color:var(--text-secondary);";
const SECONDARY_BUTTON: &str = "padding:4px 10px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);color:var(--text-secondary);font-size:12px;cursor:pointer;";

fn parse_params_input(input: &str) -> Result<Option<serde_json::Value>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    serde_json::from_str::<serde_json::Value>(trimmed)
        .map(Some)
        .map_err(|err| format!("Invalid JSON params: {err}"))
}

fn prepend_history_entry(history: &mut Vec<RpcCallHistoryEntry>, entry: RpcCallHistoryEntry) {
    history.insert(0, entry);
    history.truncate(20);
}

fn compact_preview(input: &str) -> String {
    input.replace('\n', " ").chars().take(120).collect()
}

fn format_history_age(ts_ms: u64) -> String {
    let now = js_sys::Date::now() as u64;
    let elapsed = now.saturating_sub(ts_ms) / 1000;
    if elapsed < 60 {
        format!("{elapsed}s ago")
    } else if elapsed < 3600 {
        format!("{}m ago", elapsed / 60)
    } else {
        format!("{}h ago", elapsed / 3600)
    }
}
