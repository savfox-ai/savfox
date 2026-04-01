use dioxus::prelude::*;

#[component]
pub fn ToggleSwitch(
    label: String,
    description: Option<String>,
    checked: bool,
    on_toggle: EventHandler<bool>,
) -> Element {
    let next = !checked;
    rsx! {
        div {
            class: "cfg-toggle-row",
            role: "switch",
            aria_checked: "{checked}",
            aria_label: "{label}",
            tabindex: "0",
            onclick: move |_| on_toggle(next),
            onkeydown: move |e| {
                if e.key() == Key::Enter || e.key() == Key::Character(" ".to_string()) {
                    on_toggle(next);
                }
            },
            div { class: "cfg-toggle-row__content",
                div { class: "cfg-field__label", "{label}" }
                if let Some(ref desc) = description {
                    div { class: "cfg-field__help", "{desc}" }
                }
            }
            label { class: "cfg-toggle", aria_hidden: "true",
                onclick: move |e| e.stop_propagation(),
                input { r#type: "checkbox", checked: "{checked}", tabindex: "-1", onchange: move |_| on_toggle(next) }
                span { class: "cfg-toggle__track" }
            }
        }
    }
}
