/// Focus trap utility for modal dialogs.
///
/// Cycles focus among focusable elements within the currently active
/// `[role="dialog"]` element, keeping keyboard navigation contained.

/// Cycle focus to the next focusable element inside the active dialog.
///
/// This queries the DOM for the current `[role="dialog"]` and moves focus
/// to the next button/input/select/textarea within it, wrapping around.
pub fn cycle_focus_in_dialog() {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(document) = window.document() else {
            return;
        };

        // Find the active dialog.
        let Some(dialog) = document.query_selector("[role='dialog']").ok().flatten() else {
            return;
        };

        // Collect all focusable elements within the dialog.
        let selector = "button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])";
        let Ok(focusable) = dialog.query_selector_all(selector) else {
            return;
        };

        let len = focusable.length();
        if len == 0 {
            return;
        }

        // Find which element currently has focus.
        let active = document.active_element();
        let mut current_idx = None;
        for i in 0..len {
            if let Some(el) = focusable.item(i) {
                if let Some(ref act) = active {
                    if el == **act {
                        current_idx = Some(i);
                        break;
                    }
                }
            }
        }

        // Move to the next element, wrapping around.
        let next_idx = match current_idx {
            Some(i) => (i + 1) % len,
            None => 0,
        };

        if let Some(next) = focusable.item(next_idx) {
            if let Some(el) = next.dyn_ref::<web_sys::HtmlElement>() {
                let _ = el.focus();
            }
        }
    }
}
