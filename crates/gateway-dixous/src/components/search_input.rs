use dioxus::prelude::*;

#[component]
pub fn SearchInput(
    value: String,
    on_change: EventHandler<String>,
    placeholder: Option<String>,
    match_count: Option<usize>,
) -> Element {
    let ph = placeholder.unwrap_or_else(|| "Search...".to_string());
    rsx! {
        div { class: "search-input-wrap",
            input {
                class: "search-input",
                r#type: "text",
                value: "{value}",
                placeholder: "{ph}",
                oninput: move |e| on_change(e.value()),
            }
            if let Some(count) = match_count {
                span { class: "search-input__count", "{count} matches" }
            }
        }
    }
}
