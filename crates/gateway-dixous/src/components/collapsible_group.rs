use dioxus::prelude::*;

#[component]
pub fn CollapsibleGroup(
    title: String,
    count: Option<usize>,
    initially_open: Option<bool>,
    children: Element,
) -> Element {
    let mut open = use_signal(|| initially_open.unwrap_or(true));
    let chevron = if open() { "▾" } else { "▸" };
    let count_label = count.map(|c| format!(" ({c})")).unwrap_or_default();

    rsx! {
        div { class: "collapsible-group",
            button {
                class: "collapsible-group__header",
                aria_expanded: "{open}",
                aria_label: "Toggle {title} section",
                onclick: move |_| open.toggle(),
                span { class: "collapsible-group__chevron", aria_hidden: "true", "{chevron}" }
                span { class: "collapsible-group__title", "{title}{count_label}" }
            }
            if open() {
                div { class: "collapsible-group__body", role: "region",
                    {children}
                }
            }
        }
    }
}
