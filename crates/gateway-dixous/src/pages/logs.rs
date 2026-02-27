use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::types::{LogEntry, LogsResponse};
use crate::api::ws::WsRpc;

#[component]
pub fn Logs() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut search_text = use_signal(String::new);
    let mut auto_follow = use_signal(|| true);
    let mut show_jump = use_signal(|| false);

    // Multi-select level toggles
    let mut level_info = use_signal(|| true);
    let mut level_warn = use_signal(|| true);
    let mut level_error = use_signal(|| true);
    let mut level_debug = use_signal(|| false);
    let mut level_trace = use_signal(|| false);

    let ws_logs = ws.clone();
    let logs_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_logs.clone();
        async move {
            ws.call::<LogsResponse>("logs.tail", None)
                .await
                .map(|r| r.entries)
                .unwrap_or_default()
        }
    });

    let entries: Vec<LogEntry> = logs_data.read().as_ref().cloned().unwrap_or_default();
    let is_loading = logs_data.read().is_none();

    // Auto-follow scroll effect
    let entries_len = entries.len();
    use_effect(move || {
        let _len = entries_len;
        if auto_follow() {
            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                if let Some(el) = doc.get_element_by_id("log-list") {
                    el.set_scroll_top(el.scroll_height());
                }
            }
        }
    });

    // Scroll listener for show_jump button
    use_effect(move || {
        let _af = auto_follow();
        let cb = Closure::wrap(Box::new(move |_: web_sys::Event| {
            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                if let Some(el) = doc.get_element_by_id("log-list") {
                    let at_bottom = el.scroll_top() + el.client_height() >= el.scroll_height() - 40;
                    show_jump.set(!at_bottom);
                }
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Some(el) = doc.get_element_by_id("log-list") {
                let _ = el.add_event_listener_with_callback("scroll", cb.as_ref().unchecked_ref());
            }
        }
        cb.forget();
    });

    let query = search_text().to_lowercase();
    let filtered: Vec<&LogEntry> = entries
        .iter()
        .filter(|e| {
            // Level multi-select filter
            let level = e.level.as_deref().unwrap_or("info").to_lowercase();
            let level_ok = match level.as_str() {
                "info" => level_info(),
                "warn" | "warning" => level_warn(),
                "error" => level_error(),
                "debug" => level_debug(),
                "trace" => level_trace(),
                _ => true,
            };
            if !level_ok {
                return false;
            }
            // Search filter
            if query.is_empty() {
                return true;
            }
            e.message.to_lowercase().contains(&query)
                || e.source
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&query)
        })
        .collect();

    let entry_count = filtered.len();
    let total_count = entries.len();

    rsx! {
        div { class: "logs-page",
            // Toolbar
            div { class: "logs-toolbar",
                div { class: "logs-toolbar__top",
                    div { class: "logs-toolbar__title-row",
                        h2 { class: "logs-title", "Logs" }
                        span { class: "logs-count-badge",
                            if entry_count < total_count {
                                "{entry_count}/{total_count}"
                            } else {
                                "{total_count} entries"
                            }
                        }
                    }
                    div { class: "logs-toolbar__actions",
                        button {
                            class: if auto_follow() { "logs-btn logs-btn--active" } else { "logs-btn" },
                            title: "Auto-scroll to latest entries",
                            onclick: move |_| auto_follow.toggle(),
                            "Auto-follow"
                        }
                        button {
                            class: "logs-btn",
                            title: "Download filtered logs as JSONL",
                            onclick: {
                                let entries = entries.clone();
                                move |_| export_logs_jsonl(&entries)
                            },
                            "Export"
                        }
                        button {
                            class: "logs-btn",
                            onclick: move |_| refresh_tick += 1,
                            "Refresh"
                        }
                    }
                }
                div { class: "logs-toolbar__filters",
                    // Level toggle buttons with colors
                    div { class: "logs-level-toggles",
                        button {
                            class: if level_info() { "logs-level-btn logs-level-btn--info active" } else { "logs-level-btn logs-level-btn--info" },
                            onclick: move |_| level_info.toggle(),
                            "INFO"
                        }
                        button {
                            class: if level_warn() { "logs-level-btn logs-level-btn--warn active" } else { "logs-level-btn logs-level-btn--warn" },
                            onclick: move |_| level_warn.toggle(),
                            "WARN"
                        }
                        button {
                            class: if level_error() { "logs-level-btn logs-level-btn--error active" } else { "logs-level-btn logs-level-btn--error" },
                            onclick: move |_| level_error.toggle(),
                            "ERROR"
                        }
                        button {
                            class: if level_debug() { "logs-level-btn logs-level-btn--debug active" } else { "logs-level-btn logs-level-btn--debug" },
                            onclick: move |_| level_debug.toggle(),
                            "DEBUG"
                        }
                        button {
                            class: if level_trace() { "logs-level-btn logs-level-btn--trace active" } else { "logs-level-btn logs-level-btn--trace" },
                            onclick: move |_| level_trace.toggle(),
                            "TRACE"
                        }
                    }
                    input {
                        r#type: "text",
                        placeholder: "Search logs...",
                        value: "{search_text}",
                        oninput: move |e| search_text.set(e.value()),
                        class: "logs-search",
                    }
                }
            }

            // Log list
            div { id: "log-list", class: "logs-list",
                if is_loading {
                    div { class: "logs-empty", "Loading logs..." }
                } else if filtered.is_empty() {
                    div { class: "logs-empty", "No log entries match the current filters" }
                } else {
                    for (i, entry) in filtered.iter().enumerate() {
                        {
                            let level = entry.level.as_deref().unwrap_or("info");
                            let level_lower = level.to_lowercase();
                            let level_class = match level_lower.as_str() {
                                "error" => "logs-entry logs-entry--error",
                                "warn" | "warning" => "logs-entry logs-entry--warn",
                                "debug" => "logs-entry logs-entry--debug",
                                "trace" => "logs-entry logs-entry--trace",
                                _ => "logs-entry logs-entry--info",
                            };
                            let ts = entry.timestamp.as_deref().unwrap_or("");
                            let src = entry.source.as_deref().unwrap_or("");
                            let level_upper = level.to_uppercase();
                            rsx! {
                                div { key: "{i}", class: "{level_class}",
                                    if !ts.is_empty() {
                                        span { class: "logs-entry__ts", "{ts}" }
                                    }
                                    span { class: "logs-entry__level", "{level_upper}" }
                                    if !src.is_empty() {
                                        span { class: "logs-entry__source", "[{src}]" }
                                    }
                                    span { class: "logs-entry__msg", "{entry.message}" }
                                }
                            }
                        }
                    }
                }

                // Jump-to-latest floating button
                if show_jump() {
                    button {
                        class: "logs-jump-btn",
                        onclick: move |_| {
                            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                                if let Some(el) = doc.get_element_by_id("log-list") {
                                    el.set_scroll_top(el.scroll_height());
                                }
                            }
                        },
                        "Jump to latest"
                    }
                }
            }
        }
        style { {LOGS_STYLES} }
    }
}

fn export_logs_jsonl(entries: &[LogEntry]) {
    let mut content = String::new();
    for e in entries {
        let line = serde_json::json!({
            "timestamp": e.timestamp,
            "level": e.level,
            "message": e.message,
            "source": e.source,
        });
        content.push_str(&line.to_string());
        content.push('\n');
    }

    if let Some(window) = web_sys::window() {
        let blob_opts = web_sys::BlobPropertyBag::new();
        blob_opts.set_type("application/x-ndjson");
        if let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(
            &js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(&content)),
            &blob_opts,
        ) {
            if let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) {
                if let Some(doc) = window.document() {
                    if let Ok(a) = doc.create_element("a") {
                        let _ = a.set_attribute("href", &url);
                        let _ = a.set_attribute("download", "savfox-logs.jsonl");
                        let _ = a.set_attribute("style", "display:none");
                        if let Some(body) = doc.body() {
                            let _ = body.append_child(&a);
                            if let Some(el) = a.dyn_ref::<web_sys::HtmlElement>() {
                                el.click();
                            }
                            let _ = body.remove_child(&a);
                        }
                        let _ = web_sys::Url::revoke_object_url(&url);
                    }
                }
            }
        }
    }
}

const LOGS_STYLES: &str = r#"
    .logs-page {
        display: flex;
        flex-direction: column;
        height: 100%;
    }

    .logs-toolbar {
        padding: 12px 16px;
        border-bottom: 1px solid var(--border);
        background: var(--bg-secondary);
        flex-shrink: 0;
        display: flex;
        flex-direction: column;
        gap: 10px;
    }

    .logs-toolbar__top {
        display: flex;
        justify-content: space-between;
        align-items: center;
    }

    .logs-toolbar__title-row {
        display: flex;
        align-items: center;
        gap: 10px;
    }

    .logs-title {
        font-size: 16px;
        font-weight: 600;
    }

    .logs-count-badge {
        font-size: 11px;
        padding: 2px 8px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-radius: 10px;
        color: var(--text-muted);
        font-family: monospace;
    }

    .logs-toolbar__actions {
        display: flex;
        gap: 6px;
        align-items: center;
    }

    .logs-toolbar__filters {
        display: flex;
        gap: 10px;
        align-items: center;
    }

    .logs-btn {
        padding: 4px 12px;
        background: transparent;
        color: var(--text-muted);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        font-size: 12px;
        cursor: pointer;
        transition: background 0.15s, color 0.15s;
    }

    .logs-btn:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
    }

    .logs-btn--active {
        background: var(--accent);
        color: #fff;
        border-color: var(--accent);
    }

    .logs-level-toggles {
        display: flex;
        gap: 4px;
    }

    .logs-level-btn {
        padding: 3px 10px;
        font-size: 11px;
        font-weight: 600;
        border-radius: var(--radius);
        cursor: pointer;
        transition: all 0.15s ease;
        letter-spacing: 0.03em;
        opacity: 0.45;
        border: 1px solid transparent;
        background: transparent;
    }

    .logs-level-btn:hover {
        opacity: 0.7;
    }

    .logs-level-btn.active {
        opacity: 1;
    }

    .logs-level-btn--info { color: var(--accent); }
    .logs-level-btn--info.active { background: var(--accent); color: #fff; border-color: var(--accent); }

    .logs-level-btn--warn { color: var(--warning); }
    .logs-level-btn--warn.active { background: var(--warning); color: #fff; border-color: var(--warning); }

    .logs-level-btn--error { color: var(--danger); }
    .logs-level-btn--error.active { background: var(--danger); color: #fff; border-color: var(--danger); }

    .logs-level-btn--debug { color: var(--text-muted); }
    .logs-level-btn--debug.active { background: var(--bg-tertiary); color: var(--text-primary); border-color: var(--border); }

    .logs-level-btn--trace { color: var(--text-muted); }
    .logs-level-btn--trace.active { background: var(--bg-tertiary); color: var(--text-secondary); border-color: var(--border); }

    .logs-search {
        flex: 1;
        padding: 6px 12px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        color: var(--text-primary);
        outline: none;
        font-size: 13px;
    }

    .logs-list {
        flex: 1;
        overflow: auto;
        padding: 4px 0;
        font-family: monospace;
        font-size: 12px;
        position: relative;
    }

    .logs-empty {
        padding: 32px 16px;
        color: var(--text-muted);
        text-align: center;
        font-family: system-ui, sans-serif;
        font-size: 14px;
    }

    .logs-entry {
        padding: 2px 16px;
        display: flex;
        gap: 8px;
        line-height: 1.6;
        border-left: 3px solid transparent;
        transition: background 0.1s;
    }

    .logs-entry:hover {
        background: var(--bg-hover);
    }

    .logs-entry--info { border-left-color: var(--accent); }
    .logs-entry--warn { border-left-color: var(--warning); background: rgba(255, 193, 7, 0.03); }
    .logs-entry--error { border-left-color: var(--danger); background: rgba(220, 53, 69, 0.04); }
    .logs-entry--debug { border-left-color: var(--bg-tertiary); }
    .logs-entry--trace { border-left-color: transparent; opacity: 0.65; }

    .logs-entry__ts {
        color: var(--text-muted);
        white-space: nowrap;
        min-width: 140px;
    }

    .logs-entry__level {
        font-weight: 700;
        min-width: 48px;
        font-size: 10px;
        padding: 1px 0;
    }

    .logs-entry--info .logs-entry__level { color: var(--accent); }
    .logs-entry--warn .logs-entry__level { color: var(--warning); }
    .logs-entry--error .logs-entry__level { color: var(--danger); }
    .logs-entry--debug .logs-entry__level { color: var(--text-muted); }
    .logs-entry--trace .logs-entry__level { color: var(--text-muted); }

    .logs-entry__source {
        color: var(--text-muted);
        white-space: nowrap;
    }

    .logs-entry__msg {
        color: var(--text-primary);
        white-space: pre-wrap;
        word-break: break-all;
    }

    /* ── Jump button ── */
    .logs-jump-btn {
        position: absolute;
        bottom: 16px;
        left: 50%;
        transform: translateX(-50%);
        padding: 6px 16px;
        background: var(--accent);
        color: #fff;
        border: none;
        border-radius: 20px;
        font-size: 12px;
        cursor: pointer;
        box-shadow: 0 2px 12px rgba(0, 0, 0, 0.3);
        z-index: 10;
        transition: opacity 0.2s;
    }

    .logs-jump-btn:hover {
        opacity: 0.9;
    }

    @media screen and (max-width: 768px) {
        .logs-toolbar__filters {
            flex-direction: column;
            align-items: stretch;
        }

        .logs-level-toggles {
            flex-wrap: wrap;
        }

        .logs-entry__ts {
            display: none;
        }
    }
"#;
