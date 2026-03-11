/// Replace the current browser URL without triggering a Dioxus router navigation.
///
/// This updates the browser URL bar for bookmarking/sharing purposes while keeping
/// the current component mounted (no re-render). The Dioxus router's internal state
/// is NOT updated, so the component continues to run as-is.
///
/// When the user navigates away and then presses the browser Back button, the Dioxus
/// router will see this replaced URL and route to the appropriate component.
pub fn replace_url(path: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsValue;
        if let Some(window) = web_sys::window() {
            if let Ok(history) = window.history() {
                let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(path));
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = path;
    }
}
