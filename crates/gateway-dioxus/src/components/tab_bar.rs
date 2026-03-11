use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
pub struct TabItem {
    pub id: String,
    pub label: String,
}

#[component]
pub fn TabBar(tabs: Vec<TabItem>, active: String, on_change: EventHandler<String>) -> Element {
    rsx! {
        div { class: "tab-bar",
            for tab in tabs.iter() {
                {
                    let is_active = tab.id == active;
                    let class = if is_active { "tab-bar__item active" } else { "tab-bar__item" };
                    let id = tab.id.clone();
                    rsx! {
                        button {
                            key: "{tab.id}",
                            class: "{class}",
                            onclick: move |_| on_change(id.clone()),
                            "{tab.label}"
                        }
                    }
                }
            }
        }
    }
}
