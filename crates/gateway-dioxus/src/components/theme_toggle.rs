use dioxus::document::eval;
use dioxus::prelude::*;

/// Toggle between dark and light themes.
#[component]
pub fn ThemeToggle() -> Element {
    let mut is_dark = use_signal(|| true);

    let toggle = move |_| {
        let new_val = !is_dark();
        is_dark.set(new_val);
        // Apply theme to document body via JS eval
        let theme = if new_val { "dark" } else { "light" };
        let js = format!("document.documentElement.setAttribute('data-theme', '{theme}')");
        eval(&js);
    };

    rsx! {
        button {
            class: "theme-toggle",
            style: "background: none; border: 1px solid var(--border, #444); border-radius: 6px; padding: 4px 8px; cursor: pointer; font-size: 1.1em; color: var(--text-primary, #ddd);",
            onclick: toggle,
            title: if is_dark() { "Switch to light mode" } else { "Switch to dark mode" },
            if is_dark() { "\u{2600}\u{fe0f}" } else { "\u{1f319}" }
        }
    }
}
