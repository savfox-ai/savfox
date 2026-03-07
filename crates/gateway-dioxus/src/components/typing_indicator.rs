use dioxus::prelude::*;

/// Animated typing indicator shown while the AI assistant is generating a response.
#[component]
pub fn TypingIndicator(visible: bool) -> Element {
    if !visible {
        return rsx! {};
    }

    rsx! {
        div {
            class: "typing-indicator",
            style: "display: flex; align-items: center; gap: 4px; padding: 8px 12px; opacity: 0.7;",
            span {
                class: "typing-dot",
                style: "width: 6px; height: 6px; border-radius: 50%; background: var(--text-secondary, #888); animation: typing-bounce 1.4s ease-in-out infinite; animation-delay: 0s;",
            }
            span {
                class: "typing-dot",
                style: "width: 6px; height: 6px; border-radius: 50%; background: var(--text-secondary, #888); animation: typing-bounce 1.4s ease-in-out infinite; animation-delay: 0.2s;",
            }
            span {
                class: "typing-dot",
                style: "width: 6px; height: 6px; border-radius: 50%; background: var(--text-secondary, #888); animation: typing-bounce 1.4s ease-in-out infinite; animation-delay: 0.4s;",
            }
            span {
                style: "margin-left: 8px; font-size: 0.8em; color: var(--text-secondary, #888);",
                "Thinking..."
            }
        }
    }
}
