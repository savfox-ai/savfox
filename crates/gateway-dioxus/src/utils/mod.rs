pub mod debounce;
pub mod deep_link;

/// Async sleep for `ms` milliseconds using the browser `setTimeout` API (WASM).
///
/// Shared helper to avoid duplicating the `setTimeout` + oneshot channel
/// boilerplate across components (toast, copy button, login polling, ...).
pub async fn sleep_ms(ms: i32) {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    let cb = wasm_bindgen::prelude::Closure::once(move || {
        let _ = tx.send(());
    });
    if let Some(w) = web_sys::window() {
        use wasm_bindgen::JsCast;
        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            ms,
        );
    }
    cb.forget();
    let _ = rx.await;
}

pub mod download;
pub mod focus_trap;
pub mod icons;
pub mod model_visibility;
pub mod notifications;
pub mod provider_catalog;
pub mod provider_registry;
pub mod storage;
pub mod text;
pub mod time;
