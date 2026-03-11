use dioxus::prelude::*;

use crate::route::Route;

/// A single command entry in the palette.
#[derive(Clone)]
struct PaletteCommand {
    label: &'static str,
    shortcut: &'static str,
    route: Option<Route>,
}

fn all_commands() -> Vec<PaletteCommand> {
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
}

#[component]
pub fn CommandPalette(open: bool, on_close: EventHandler<()>) -> Element {
    let mut query = use_signal(String::new);
    let mut selected = use_signal(|| 0usize);
    let nav = use_navigator();

    if !open {
        return rsx! {};
    }

    let q = query().to_lowercase();
    let commands: Vec<PaletteCommand> = all_commands()
        .into_iter()
        .filter(|c| q.is_empty() || c.label.to_lowercase().contains(&q))
        .collect();

    let count = commands.len();

    let mut select_and_close = move |idx: usize| {
        let cmds = all_commands()
            .into_iter()
            .filter(|c| {
                let q = query().to_lowercase();
                q.is_empty() || c.label.to_lowercase().contains(&q)
            })
            .collect::<Vec<_>>();
        if let Some(cmd) = cmds.get(idx) {
            if let Some(ref route) = cmd.route {
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
                            Key::ArrowDown => {
                                if count > 0 {
                                    selected.set((selected() + 1) % count);
                                }
                            }
                            Key::ArrowUp => {
                                if count > 0 {
                                    selected.set(selected().checked_sub(1).unwrap_or(count - 1));
                                }
                            }
                            Key::Enter => {
                                select_and_close(selected());
                            }
                            _ => {}
                        }
                    },
                }
                div { id: "palette-results-list", class: "palette-results", role: "listbox",
                    if commands.is_empty() {
                        div { class: "palette-empty", "No matching commands" }
                    }
                    for (i, cmd) in commands.iter().enumerate() {
                        {
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
        style { {PALETTE_STYLES} }
    }
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
