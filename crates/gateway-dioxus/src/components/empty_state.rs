use dioxus::prelude::*;

/// Empty state placeholder with icon, message, and optional action.
#[component]
pub fn EmptyState(
    icon: Element,
    message: String,
    #[props(default = None)] action_label: Option<String>,
    #[props(default = None)] on_action: Option<EventHandler<()>>,
) -> Element {
    rsx! {
        div { class: "empty-state",
            div { class: "empty-state__icon", {icon} }
            p { class: "empty-state__message", "{message}" }
            if let (Some(label), Some(handler)) = (action_label.as_ref(), on_action.as_ref()) {
                {
                    let handler = handler.clone();
                    rsx! {
                        button {
                            class: "empty-state__action",
                            onclick: move |_| handler.call(()),
                            "{label}"
                        }
                    }
                }
            }
        }
    }
}

/// Error state with retry button.
#[component]
pub fn ErrorState(
    message: String,
    #[props(default = None)] details: Option<String>,
    #[props(default = None)] on_retry: Option<EventHandler<()>>,
) -> Element {
    rsx! {
        div { class: "error-state",
            div { class: "error-state__icon", "!!" }
            p { class: "error-state__message", "{message}" }
            if let Some(ref detail) = details {
                pre { class: "error-state__details", "{detail}" }
            }
            if let Some(ref handler) = on_retry {
                {
                    let handler = handler.clone();
                    rsx! {
                        button {
                            class: "error-state__retry",
                            onclick: move |_| handler.call(()),
                            "Retry"
                        }
                    }
                }
            }
        }
    }
}
