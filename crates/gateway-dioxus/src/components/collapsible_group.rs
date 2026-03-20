use dioxus::prelude::*;
use lucide_dioxus::{ChevronDown, ChevronRight};

#[component]
pub fn CollapsibleGroup(
    title: String,
    count: Option<usize>,
    initially_open: Option<bool>,
    is_open: Option<bool>,
    on_toggle: Option<EventHandler<bool>>,
    children: Element,
) -> Element {
    let mut open = use_signal(|| initially_open.unwrap_or(true));

    // If externally controlled, sync signal
    let effective_open = is_open.unwrap_or_else(|| open());

    let count_label = count.map(|c| format!(" ({c})")).unwrap_or_default();

    rsx! {
        div { class: "collapsible-group",
            button {
                class: "collapsible-group__header",
                aria_expanded: "{effective_open}",
                aria_label: "Toggle {title} section",
                onclick: move |_| {
                    let new_state = !effective_open;
                    open.set(new_state);
                    if let Some(ref handler) = on_toggle {
                        handler.call(new_state);
                    }
                },
                span { class: "collapsible-group__chevron", aria_hidden: "true",
                    if effective_open { ChevronDown { size: 14 } } else { ChevronRight { size: 14 } }
                }
                span { class: "collapsible-group__title", "{title}{count_label}" }
            }
            if effective_open {
                div { class: "collapsible-group__body", role: "region",
                    {children}
                }
            }
        }
    }
}
