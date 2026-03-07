use dioxus::prelude::*;

/// A focus mode wrapper that hides the sidebar and maximizes the content area.
#[component]
pub fn FocusModeToggle(active: Signal<bool>) -> Element {
    let toggle = move |_| {
        active.set(!active());
    };

    rsx! {
        button {
            class: "focus-mode-toggle",
            style: "background: none; border: 1px solid var(--border, #444); border-radius: 6px; padding: 4px 8px; cursor: pointer; color: var(--text-primary, #ddd); font-size: 0.85em;",
            onclick: toggle,
            title: if active() { "Exit focus mode" } else { "Enter focus mode" },
            if active() { "Exit Focus" } else { "Focus" }
        }
    }
}
