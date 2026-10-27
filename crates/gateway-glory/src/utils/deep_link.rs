pub fn replace_url(path: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsValue;
        if let Some(window) = web_sys::window()
            && let Ok(history) = window.history()
        {
            let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(path));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = path;
    }
}
