use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::{MemoryEntry, MemoryLayer, MemoryLayersResponse, MemoryListResponse};
use crate::api::ws::WsRpc;

#[component]
pub fn Memory() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);

    let mut selected_slug = use_signal(|| Option::<String>::None);
    let mut edit_content = use_signal(String::new);
    let mut edit_tags = use_signal(String::new);
    let mut show_create = use_signal(|| false);
    let new_slug = use_signal(String::new);
    let new_layer = use_signal(|| "global".to_string());
    let new_content = use_signal(String::new);
    let new_tags = use_signal(String::new);
    let mut filter_layer = use_signal(String::new);
    let mut search_query = use_signal(String::new);

    // Fetch entries + layers.
    let ws_entries = ws.clone();
    let entries_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_entries.clone();
        async move {
            ws.call::<MemoryListResponse>("memory.list", Some(json!({ "include_content": true })))
                .await
                .map(|r| r.entries)
                .unwrap_or_default()
        }
    });

    let ws_layers = ws.clone();
    let layers_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_layers.clone();
        async move {
            ws.call::<MemoryLayersResponse>("memory.layers", None)
                .await
                .map(|r| r.layers)
                .unwrap_or_default()
        }
    });

    let is_loading = entries_data.read().is_none();
    let entries: Vec<MemoryEntry> = entries_data.read().as_ref().cloned().unwrap_or_default();
    let layers: Vec<MemoryLayer> = layers_data.read().as_ref().cloned().unwrap_or_default();

    // Filtered entries.
    let filtered: Vec<MemoryEntry> = entries
        .iter()
        .filter(|e| {
            let fl = filter_layer();
            if !fl.is_empty() && e.layer != fl {
                return false;
            }
            let q = search_query().to_lowercase();
            if !q.is_empty() {
                return e.slug.to_lowercase().contains(&q)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(&q))
                    || e.body.as_deref().unwrap_or("").to_lowercase().contains(&q);
            }
            true
        })
        .cloned()
        .collect();

    let selected_entry: Option<MemoryEntry> =
        selected_slug().and_then(|slug| entries.iter().find(|e| e.slug == slug).cloned());

    rsx! {
        div { style: "display:flex;height:100%;",
            // ── Left panel: entry list ──
            div { style: "width:320px;min-width:320px;border-right:1px solid var(--border);display:flex;flex-direction:column;",
                // Toolbar
                div { style: "padding:12px 16px;border-bottom:1px solid var(--border);display:flex;flex-direction:column;gap:8px;",
                    div { style: "display:flex;justify-content:space-between;align-items:center;",
                        h2 { style: "font-size:16px;font-weight:600;", "Memory" }
                        div { style: "display:flex;gap:6px;",
                            button {
                                onclick: move |_| refresh_tick += 1,
                                style: "{TOOL_BTN}",
                                "Refresh"
                            }
                            button {
                                onclick: move |_| { show_create.set(true); selected_slug.set(None); },
                                style: "{TOOL_BTN}background:var(--accent);color:#fff;border:none;",
                                "+ New"
                            }
                        }
                    }
                    input {
                        r#type: "text",
                        placeholder: "Search memories...",
                        value: "{search_query}",
                        oninput: move |e| search_query.set(e.value()),
                        style: "{INPUT}",
                    }
                    select {
                        value: "{filter_layer}",
                        onchange: move |e: Event<FormData>| filter_layer.set(e.value()),
                        style: "{INPUT}padding:4px 8px;",
                        option { value: "", "All layers" }
                        for l in layers.iter() {
                            option { key: "{l.layer}", value: "{l.layer}", "{l.layer}" }
                        }
                    }
                }

                // Entry list
                div { style: "flex:1;overflow:auto;",
                    if is_loading {
                        p { style: "padding:16px;color:var(--text-muted);font-size:14px;", "Loading..." }
                    } else if filtered.is_empty() {
                        p { style: "padding:16px;color:var(--text-muted);font-size:14px;", "No memory entries" }
                    } else {
                        for entry in filtered.iter() {
                            {
                                let is_sel = selected_slug() == Some(entry.slug.clone());
                                let bg = if is_sel { "var(--bg-hover)" } else { "transparent" };
                                let lc = layer_color(&entry.layer);
                                let e = entry.clone();
                                rsx! {
                                    div {
                                        key: "{entry.layer}-{entry.slug}",
                                        onclick: move |_| {
                                            edit_content.set(e.body.clone().unwrap_or_default());
                                            edit_tags.set(e.tags.join(", "));
                                            selected_slug.set(Some(e.slug.clone()));
                                            show_create.set(false);
                                        },
                                        style: "padding:10px 16px;border-bottom:1px solid var(--border);cursor:pointer;background:{bg};",
                                        div { style: "display:flex;justify-content:space-between;align-items:center;",
                                            span { style: "font-weight:500;font-size:14px;", "{entry.slug}" }
                                            span {
                                                style: "font-size:11px;padding:1px 6px;border-radius:4px;background:{lc};color:#fff;",
                                                "{entry.layer}"
                                            }
                                        }
                                        if !entry.tags.is_empty() {
                                            div { style: "margin-top:4px;display:flex;gap:4px;flex-wrap:wrap;",
                                                for tag in entry.tags.iter() {
                                                    span {
                                                        key: "{tag}",
                                                        style: "font-size:11px;padding:1px 5px;border-radius:3px;background:var(--bg-tertiary);color:var(--text-secondary);",
                                                        "{tag}"
                                                    }
                                                }
                                            }
                                        }
                                        div { style: "font-size:12px;color:var(--text-muted);margin-top:2px;",
                                            "P{entry.priority} "
                                            if entry.pinned { "(pinned) " }
                                            "{entry.body_bytes}B"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Right panel: editor ──
            div { style: "flex:1;display:flex;flex-direction:column;",
                if show_create() {
                    { render_create_form(ws.clone(), refresh_tick, show_create, new_slug, new_layer, new_content, new_tags) }
                } else if let Some(ref entry) = selected_entry {
                    { render_edit_view(ws.clone(), refresh_tick, entry, selected_slug, edit_content, edit_tags) }
                } else {
                    div { style: "display:flex;align-items:center;justify-content:center;height:100%;color:var(--text-muted);font-size:14px;",
                        "Select a memory entry or create a new one"
                    }
                }
            }
        }
    }
}

fn render_create_form(
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    mut show_create: Signal<bool>,
    mut new_slug: Signal<String>,
    mut new_layer: Signal<String>,
    mut new_content: Signal<String>,
    mut new_tags: Signal<String>,
) -> Element {
    rsx! {
        div { style: "padding:24px;",
            h3 { style: "font-size:18px;margin-bottom:16px;", "New Memory Entry" }
            div { style: "display:flex;flex-direction:column;gap:12px;max-width:600px;",
                div {
                    label { style: "{LABEL}", "Slug" }
                    input {
                        value: "{new_slug}",
                        oninput: move |e| new_slug.set(e.value()),
                        placeholder: "my-memory-note",
                        style: "{INPUT}",
                    }
                }
                div {
                    label { style: "{LABEL}", "Layer" }
                    select {
                        value: "{new_layer}",
                        onchange: move |e: Event<FormData>| new_layer.set(e.value()),
                        style: "{INPUT}",
                        option { value: "global", "Global" }
                        option { value: "project", "Project" }
                        option { value: "agent", "Agent" }
                    }
                }
                div {
                    label { style: "{LABEL}", "Tags (comma-separated)" }
                    input {
                        value: "{new_tags}",
                        oninput: move |e| new_tags.set(e.value()),
                        placeholder: "rust, patterns",
                        style: "{INPUT}",
                    }
                }
                div {
                    label { style: "{LABEL}", "Content (Markdown)" }
                    textarea {
                        value: "{new_content}",
                        oninput: move |e| new_content.set(e.value()),
                        placeholder: "# My Notes...",
                        rows: 12,
                        style: "{INPUT}resize:vertical;font-family:monospace;font-size:13px;",
                    }
                }
                div { style: "display:flex;gap:8px;",
                    button {
                        onclick: move |_| {
                            let slug = new_slug().trim().to_lowercase().replace(char::is_whitespace, "-");
                            let layer = new_layer();
                            let content = new_content();
                            let tags: Vec<String> = new_tags().split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
                            if slug.is_empty() || content.is_empty() { return; }
                            let ws = ws.clone();
                            spawn(async move {
                                let _ = ws.call::<serde_json::Value>("memory.create", Some(json!({
                                    "layer": layer, "slug": slug, "content": content, "tags": tags,
                                }))).await;
                                show_create.set(false);
                                new_slug.set(String::new());
                                new_layer.set("global".into());
                                new_content.set(String::new());
                                new_tags.set(String::new());
                                refresh_tick += 1;
                            });
                        },
                        style: "{TOOL_BTN}background:var(--accent);color:#fff;border:none;padding:8px 20px;",
                        "Create"
                    }
                    button {
                        onclick: move |_| show_create.set(false),
                        style: "{TOOL_BTN}padding:8px 20px;",
                        "Cancel"
                    }
                }
            }
        }
    }
}

fn render_edit_view(
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    entry: &MemoryEntry,
    mut selected_slug: Signal<Option<String>>,
    mut edit_content: Signal<String>,
    mut edit_tags: Signal<String>,
) -> Element {
    let lc = layer_color(&entry.layer);
    let slug_save = entry.slug.clone();
    let slug_del = entry.slug.clone();
    let ws_save = ws.clone();
    let ws_del = ws;

    rsx! {
        div { style: "display:flex;flex-direction:column;height:100%;",
            div { style: "padding:12px 16px;border-bottom:1px solid var(--border);display:flex;justify-content:space-between;align-items:center;",
                div {
                    span { style: "font-weight:600;", "{entry.slug}" }
                    span {
                        style: "margin-left:8px;font-size:11px;padding:1px 6px;border-radius:4px;background:{lc};color:#fff;",
                        "{entry.layer}"
                    }
                }
                div { style: "display:flex;gap:6px;",
                    button {
                        onclick: move |_| {
                            let slug = slug_save.clone();
                            let content = edit_content();
                            let tags: Vec<String> = edit_tags().split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
                            let ws = ws_save.clone();
                            spawn(async move {
                                let _ = ws.call::<serde_json::Value>("memory.update", Some(json!({
                                    "slug": slug, "content": content, "tags": tags,
                                }))).await;
                                refresh_tick += 1;
                            });
                        },
                        style: "{TOOL_BTN}background:var(--success);color:#fff;border:none;",
                        "Save"
                    }
                    button {
                        onclick: move |_| {
                            let slug = slug_del.clone();
                            let ws = ws_del.clone();
                            spawn(async move {
                                let _ = ws.call::<serde_json::Value>("memory.delete", Some(json!({ "slug": slug }))).await;
                                selected_slug.set(None);
                                refresh_tick += 1;
                            });
                        },
                        style: "{TOOL_BTN}color:var(--danger);border-color:var(--danger);",
                        "Delete"
                    }
                }
            }
            div { style: "padding:12px 16px;",
                label { style: "{LABEL}", "Tags" }
                input {
                    value: "{edit_tags}",
                    oninput: move |e| edit_tags.set(e.value()),
                    style: "{INPUT}",
                }
            }
            textarea {
                value: "{edit_content}",
                oninput: move |e| edit_content.set(e.value()),
                style: "flex:1;margin:0 16px 16px;padding:12px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);color:var(--text-primary);font-family:monospace;font-size:13px;line-height:1.6;resize:none;outline:none;",
            }
        }
    }
}

fn layer_color(layer: &str) -> &'static str {
    match layer {
        "global" => "#6366f1",
        "project" => "#22c55e",
        "agent" => "#f59e0b",
        "session" => "#ef4444",
        _ => "#666",
    }
}

const TOOL_BTN: &str = "padding:4px 12px;background:transparent;color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:12px;";
const INPUT: &str = "width:100%;padding:8px 12px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);color:var(--text-primary);outline:none;font-size:14px;";
const LABEL: &str =
    "display:block;font-size:12px;font-weight:600;color:var(--text-secondary);margin-bottom:4px;";
