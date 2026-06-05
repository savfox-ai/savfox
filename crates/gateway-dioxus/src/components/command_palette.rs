use std::sync::{LazyLock, Once};

use dioxus::prelude::*;

use crate::route::Route;

/// A single command entry in the palette.
#[derive(Clone)]
struct PaletteCommand {
    label: &'static str,
    shortcut: &'static str,
    route: Option<Route>,
}

/// All palette commands, built once. Every field is `'static`, so the list can
/// live in a `LazyLock` instead of being rebuilt on every render.
static COMMANDS: LazyLock<Vec<PaletteCommand>> = LazyLock::new(|| {
    vec![
        PaletteCommand {
            label: "Go to Overview",
            shortcut: "",
            route: Some(Route::Overview {}),
        },
        PaletteCommand {
            label: "Go to Session",
            shortcut: "Ctrl+/",
            route: Some(Route::Sessions {}),
        },
        PaletteCommand {
            label: "Go to Agents",
            shortcut: "",
            route: Some(Route::Agents {}),
        },
        PaletteCommand {
            label: "Go to Channels",
            shortcut: "",
            route: Some(Route::Channels {}),
        },
        PaletteCommand {
            label: "Go to Models",
            shortcut: "",
            route: Some(Route::Models {}),
        },
        PaletteCommand {
            label: "Go to Config",
            shortcut: "Ctrl+,",
            route: Some(Route::Config {}),
        },
        PaletteCommand {
            label: "Go to Cron Jobs",
            shortcut: "",
            route: Some(Route::Cron {}),
        },
        PaletteCommand {
            label: "Go to Logs",
            shortcut: "Ctrl+Shift+L",
            route: Some(Route::Logs {}),
        },
        PaletteCommand {
            label: "Go to Usage",
            shortcut: "",
            route: Some(Route::Usage {}),
        },
        PaletteCommand {
            label: "Go to Approvals",
            shortcut: "",
            route: Some(Route::Approvals {}),
        },
        PaletteCommand {
            label: "Go to Nodes",
            shortcut: "",
            route: Some(Route::Nodes {}),
        },
        PaletteCommand {
            label: "Go to Skills",
            shortcut: "",
            route: Some(Route::Skills {}),
        },
        PaletteCommand {
            label: "Go to Debug",
            shortcut: "",
            route: Some(Route::Debug {}),
        },
        PaletteCommand {
            label: "Create Agent",
            shortcut: "",
            route: Some(Route::AgentsNew {}),
        },
        PaletteCommand {
            label: "New Cron Job",
            shortcut: "",
            route: Some(Route::CronNew {}),
        },
    ]
});

#[component]
pub fn CommandPalette(open: bool, on_close: EventHandler<()>) -> Element {
    inject_palette_styles_once();
    let mut query = use_signal(String::new);
    let mut selected = use_signal(|| 0usize);
    let nav = use_navigator();

    // Indices into the static `COMMANDS` list that match the current query.
    // Memoizing the `Vec<usize>` (which is `PartialEq`) avoids re-filtering all
    // commands on every render and sidesteps needing `PartialEq` on the command.
    let filtered: Memo<Vec<usize>> = use_memo(move || {
        let q = query().to_lowercase();
        COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, c)| q.is_empty() || c.label.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    });

    if !open {
        return rsx! {};
    }

    let indices = filtered();
    let count = indices.len();

    let mut select_and_close = move |list_idx: usize| {
        if let Some(&cmd_idx) = filtered().get(list_idx) {
            if let Some(ref route) = COMMANDS[cmd_idx].route {
                nav.push(route.clone());
            }
        }
        on_close.call(());
        query.set(String::new());
        selected.set(0);
    };

    rsx! {
        div {
            class: "palette-overlay",
            role: "dialog",
            aria_modal: "true",
            aria_label: "Command palette",
            onclick: move |_| {
                on_close.call(());
                query.set(String::new());
                selected.set(0);
            },
            div {
                class: "palette-dialog",
                onclick: move |e| e.stop_propagation(),
                input {
                    class: "palette-input",
                    r#type: "text",
                    placeholder: "Type a command...",
                    aria_label: "Search commands",
                    role: "combobox",
                    aria_expanded: "true",
                    aria_controls: "palette-results-list",
                    aria_autocomplete: "list",
                    value: "{query}",
                    autofocus: true,
                    oninput: move |e| {
                        query.set(e.value());
                        selected.set(0);
                    },
                    onkeydown: move |e| {
                        match e.key() {
                            Key::Escape => {
                                on_close.call(());
                                query.set(String::new());
                                selected.set(0);
                            }
                            Key::ArrowDown if count > 0 => {
                                selected.set((selected() + 1) % count);
                            }
                            Key::ArrowUp if count > 0 => {
                                selected.set(selected().checked_sub(1).unwrap_or(count - 1));
                            }
                            Key::Enter => {
                                select_and_close(selected());
                            }
                            _ => {}
                        }
                    },
                }
                div { id: "palette-results-list", class: "palette-results", role: "listbox",
                    if indices.is_empty() {
                        div { class: "palette-empty", "No matching commands" }
                    }
                    for (i, &cmd_idx) in indices.iter().enumerate() {
                        {
                            let cmd = &COMMANDS[cmd_idx];
                            let is_selected = i == selected();
                            let cls = if is_selected { "palette-item palette-item--selected" } else { "palette-item" };
                            let shortcut = cmd.shortcut;
                            rsx! {
                                div {
                                    key: "{i}",
                                    class: "{cls}",
                                    role: "option",
                                    aria_selected: "{is_selected}",
                                    onclick: move |_| select_and_close(i),
                                    span { class: "palette-item__label", "{cmd.label}" }
                                    if !shortcut.is_empty() {
                                        span { class: "palette-item__shortcut", "{shortcut}" }
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

/// Injects the command-palette stylesheet into the document head exactly once,
/// instead of emitting an inline `<style>` block on every render.
fn inject_palette_styles_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Ok(el) = doc.create_element("style") {
                el.set_inner_html(PALETTE_STYLES);
                if let Some(head) = doc.head() {
                    let _ = head.append_child(&el);
                }
            }
        }
    });
}

const PALETTE_STYLES: &str = r#"
    .palette-overlay {
        position: fixed;
        inset: 0;
        background: rgba(0, 0, 0, 0.5);
        z-index: 200;
        display: flex;
        align-items: flex-start;
        justify-content: center;
        padding-top: 15vh;
    }

    .palette-dialog {
        width: 480px;
        max-width: 90vw;
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: 12px;
        box-shadow: 0 16px 48px rgba(0, 0, 0, 0.4);
        overflow: hidden;
    }

    .palette-input {
        width: 100%;
        padding: 14px 18px;
        background: transparent;
        border: none;
        border-bottom: 1px solid var(--border);
        color: var(--text-primary);
        font-size: 15px;
        outline: none;
    }

    .palette-input::placeholder {
        color: var(--text-muted);
    }

    .palette-results {
        max-height: 320px;
        overflow-y: auto;
        padding: 6px 0;
    }

    .palette-empty {
        padding: 16px 18px;
        color: var(--text-muted);
        font-size: 13px;
        text-align: center;
    }

    .palette-item {
        display: flex;
        align-items: center;
        justify-content: space-between;
        padding: 8px 18px;
        cursor: pointer;
        transition: background 0.1s;
    }

    .palette-item:hover,
    .palette-item--selected {
        background: var(--bg-hover);
    }

    .palette-item__label {
        font-size: 14px;
        color: var(--text-primary);
    }

    .palette-item__shortcut {
        font-size: 11px;
        color: var(--text-muted);
        padding: 2px 6px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-radius: 4px;
        font-family: var(--font-mono);
    }
"#;
