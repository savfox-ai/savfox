use dioxus::prelude::*;
use lucide_dioxus::ScrollText;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::types::{LogEntry, LogsResponse};
use crate::api::ws::WsRpc;
use crate::components::copy_button::CopyButton;
use crate::components::empty_state::EmptyState;
use crate::components::skeleton::*;
use crate::utils::debounce::use_debounced_signal;
use crate::utils::download::trigger_download;

/// Extract just the time portion (HH:MM:SS) from a timestamp string.
/// Falls back to the original string if no `T` separator is found.
fn format_short_ts(ts: &str) -> String {
    // Handles ISO-8601 like "2024-03-11T14:32:01Z" or "2024-03-11T14:32:01.123+00:00"
    if let Some(time_part) = ts.split('T').nth(1) {
        // Take only HH:MM:SS (first 8 chars of the time portion)
        time_part.chars().take(8).collect()
    } else {
        ts.to_string()
    }
}

#[component]
pub fn Logs() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut search = use_debounced_signal(300);
    let mut auto_follow = use_signal(|| true);
    let mut show_jump = use_signal(|| false);
    let max_entries = use_signal(|| 500usize);
    let mut expanded_entry = use_signal(|| None::<usize>);

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

    // Scroll listener for the show_jump button. Bind exactly once: the callback
    // never reads `auto_follow`, so re-binding it on every toggle only leaked a
    // forgotten closure and stacked duplicate listeners. The flag guards against
    // the effect re-running for any reason.
    let mut scroll_listener_bound = use_signal(|| false);
    use_effect(move || {
        if *scroll_listener_bound.peek() {
            return;
        }
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
                scroll_listener_bound.set(true);
            }
        }
        cb.forget();
    });

    // Cache the filtered + capped display order as indices into `entries`.
    // `LogEntry` is not `PartialEq`, so we memoize the comparable index list
    // instead of cloned entries. Recomputes only when the log data, search
    // query, level toggles, or cap change.
    let display_order = use_memo(move || {
        let entries = logs_data.read().as_ref().cloned().unwrap_or_default();
        let query = search.value().to_lowercase();
        let filtered: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
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
            .map(|(i, _)| i)
            .collect();

        let total_filtered = filtered.len();
        let max = max_entries();
        let display: Vec<usize> = if total_filtered > max {
            filtered.into_iter().skip(total_filtered - max).collect()
        } else {
            filtered
        };
        (display, total_filtered)
    });
    let display_guard = display_order.read();
    let display_indices = &display_guard.0;
    let total_filtered = display_guard.1;
    let entry_count = display_indices.len();
    let total_count = entries.len();
    let max = max_entries();
    let capped = total_filtered > max;

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
                            title: "Export filtered logs as JSON",
                            onclick: {
                                let filtered_for_export: Vec<LogEntry> = display_indices
                                    .iter()
                                    .map(|&i| entries[i].clone())
                                    .collect();
                                move |_| export_logs_json(&filtered_for_export)
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
                        value: "{search.raw()}",
                        oninput: move |e| {
                            search.set_raw(e.value());
                        },
                        class: "logs-search",
                    }
                }
            }

            // Log list
            div { id: "log-list", class: "logs-list",
                if capped {
                    div { class: "logs-cap-notice",
                        "Showing last {max} of {total_filtered} entries"
                    }
                }
                if is_loading {
                    div { class: "logs-empty",
                        SkeletonLines { count: 5 }
                    }
                } else if display_indices.is_empty() {
                    EmptyState {
                        icon: rsx! { ScrollText { size: 20 } },
                        message: "No log entries match the current filters".to_string(),
                    }
                } else {
                    for (i, idx) in display_indices.iter().copied().enumerate() {
                        {
                            let entry = &entries[idx];
                            let level = entry.level.as_deref().unwrap_or("info");
                            let level_lower = level.to_lowercase();
                            let is_expanded = expanded_entry() == Some(i);
                            let level_class = match level_lower.as_str() {
                                "error" => "logs-entry logs-entry--error",
                                "warn" | "warning" => "logs-entry logs-entry--warn",
                                "debug" => "logs-entry logs-entry--debug",
                                "trace" => "logs-entry logs-entry--trace",
                                _ => "logs-entry logs-entry--info",
                            };
                            let ts = entry.timestamp.as_deref().unwrap_or("");
                            let short_ts = format_short_ts(ts);
                            let src = entry.source.as_deref().unwrap_or("");
                            let level_upper = level.to_uppercase();
                            let msg = entry.message.clone();
                            rsx! {
                                div {
                                    key: "{i}",
                                    class: if is_expanded { "{level_class} logs-entry--expanded" } else { "{level_class}" },
                                    role: "button",
                                    tabindex: "0",
                                    onclick: move |_| {
                                        if expanded_entry() == Some(i) {
                                            expanded_entry.set(None);
                                        } else {
                                            expanded_entry.set(Some(i));
                                        }
                                    },
                                    if !ts.is_empty() {
                                        span { class: "logs-entry__ts", title: "{ts}", "{short_ts}" }
                                    }
                                    span { class: "logs-entry__level", "{level_upper}" }
                                    if !src.is_empty() {
                                        span { class: "logs-entry__source", "[{src}]" }
                                    }
                                    if is_expanded {
                                        span { class: "logs-entry__msg logs-entry__msg--full", "{msg}" }
                                        CopyButton { text: msg.clone() }
                                    } else {
                                        span { class: "logs-entry__msg", "{msg}" }
                                    }
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

fn export_logs_json(entries: &[LogEntry]) {
    let json_entries: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "timestamp": e.timestamp,
                "level": e.level,
                "message": e.message,
                "source": e.source,
            })
        })
        .collect();

    let content = serde_json::to_string_pretty(&json_entries).unwrap_or_else(|_| "[]".to_string());

    // Build filename with current timestamp: savfox-logs-{timestamp}.json
    let now = chrono::Utc::now();
    let filename = format!("savfox-logs-{}.json", now.format("%Y%m%d-%H%M%S"));

    trigger_download(&filename, &content, "application/json");
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
        font-family: var(--font-mono);
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
        font-family: var(--font-mono);
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
        cursor: pointer;
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
        min-width: 64px;
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
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        flex: 1;
        min-width: 0;
    }

    .logs-entry__msg--full {
        white-space: pre-wrap;
        word-break: break-all;
        overflow: visible;
        text-overflow: unset;
    }

    .logs-entry--expanded {
        background: var(--bg-hover);
        flex-wrap: wrap;
    }

    .logs-cap-notice {
        padding: 4px 16px;
        font-size: 11px;
        color: var(--text-muted);
        background: var(--bg-secondary);
        border-bottom: 1px solid var(--border);
        text-align: center;
    }

    /* ── Jump button ── */
    .logs-jump-btn {
        position: absolute;
        bottom: 16px;
        left: 50%;
        transform: translateX(-50%);
        padding: 6px 16px;
        background: var(--button-cta-surface);
        color: #fff;
        border: none;
        border-radius: 20px;
        font-size: 12px;
        cursor: pointer;
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-cta-shadow);
        z-index: 10;
        transition: opacity 0.2s, background 0.15s ease, box-shadow 0.15s ease;
    }

    .logs-jump-btn:hover {
        opacity: 1;
        background: var(--button-cta-surface-hover);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-cta-shadow-hover);
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
