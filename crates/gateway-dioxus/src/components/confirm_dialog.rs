use dioxus::prelude::*;

/// A confirmation dialog for destructive actions.
#[component]
pub fn ConfirmDialog(
    title: String,
    message: String,
    #[props(default = "Confirm".to_string())] confirm_label: String,
    #[props(default = "Cancel".to_string())] cancel_label: String,
    #[props(default = false)] danger: bool,
    on_confirm: EventHandler<()>,
    on_cancel: EventHandler<()>,
) -> Element {
    // Keyboard handler for Escape/Enter
    let on_confirm_clone = on_confirm.clone();
    let on_cancel_clone = on_cancel.clone();

    rsx! {
        div {
            class: "confirm-backdrop",
            role: "dialog",
            aria_modal: "true",
            aria_label: "{title}",
            tabindex: "-1",
            onmounted: move |e| {
                let _ = e.data().set_focus(true);
            },
            onkeydown: move |e: KeyboardEvent| {
                match e.key() {
                    Key::Escape => on_cancel_clone.call(()),
                    Key::Enter => on_confirm_clone.call(()),
                    Key::Tab => {
                        // Focus trap: cycle Tab within the dialog buttons.
                        // The `confirm-backdrop` has tabindex="-1" so it catches
                        // focus, and the two buttons are the only focusable children.
                        // When Tab reaches the last button it cycles back to the first.
                        e.prevent_default();
                        crate::utils::focus_trap::cycle_focus_in_dialog();
                    }
                    _ => {}
                }
            },
            onclick: move |_| on_cancel.call(()),
            div {
                class: "confirm-dialog",
                onclick: move |e| e.stop_propagation(),
                div { class: "confirm-dialog__header",
                    h3 { class: "confirm-dialog__title", "{title}" }
                }
                div { class: "confirm-dialog__body", "{message}" }
                div { class: "confirm-dialog__actions",
                    button {
                        class: "confirm-dialog__btn confirm-dialog__btn--cancel",
                        onclick: move |_| on_cancel.call(()),
                        "{cancel_label}"
                    }
                    button {
                        class: if danger { "confirm-dialog__btn confirm-dialog__btn--danger" } else { "confirm-dialog__btn confirm-dialog__btn--confirm" },
                        onclick: move |_| on_confirm.call(()),
                        "{confirm_label}"
                    }
                }
            }
        }
    }
}
