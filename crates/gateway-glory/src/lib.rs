//! Savfox browser client built from Glory semantic widgets and Sapid controls.
pub mod api;
pub mod chat;
pub mod components;
pub mod i18n;
pub mod pages;
pub mod route;
pub mod ui;
pub mod utils;

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::cell::{Cell, RefCell};

    use glory::web::WebApp;
    use wasm_bindgen::prelude::*;

    thread_local! {
        static APP: RefCell<Option<WebApp>> = const { RefCell::new(None) };
        static FLUSHING: Cell<bool> = const { Cell::new(false) };
    }

    pub fn flush() {
        FLUSHING.with(|guard| {
            if guard.replace(true) {
                return;
            }
            APP.with(|app| {
                if let Ok(app) = app.try_borrow()
                    && let Some(app) = app.as_ref()
                    && let Err(error) = app.app().flush()
                {
                    web_sys::console::error_1(&error.to_string().into());
                }
            });
            guard.set(false);
        });
    }

    #[wasm_bindgen(start)]
    pub fn start() -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        if let Some(window) = web_sys::window()
            && let Some(document) = window.document()
        {
            if let Some(element) = document.document_element() {
                let preference = window
                    .local_storage()
                    .ok()
                    .flatten()
                    .and_then(|storage| storage.get_item("savfox_theme").ok().flatten())
                    .unwrap_or_else(|| "system".into());
                let theme = if matches!(preference.as_str(), "light" | "dark" | "system") {
                    preference.as_str()
                } else {
                    "system"
                };
                let _ = element.set_attribute("data-theme", theme);
                let _ = element.set_attribute("lang", crate::i18n::detect_locale().code());
            }
            document.set_title(&crate::i18n::t(crate::i18n::detect_locale(), "app.title"));
        }
        let root = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("app"))
            .ok_or_else(|| JsValue::from_str("Missing #app mount point"))?;
        let app = glory::web::launch_with(root, crate::components::app)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        APP.with(|slot| *slot.borrow_mut() = Some(app));
        flush();
        Ok(())
    }
}
