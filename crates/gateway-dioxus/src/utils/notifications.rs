//! Browser Notification API utility functions.
//!
//! Wraps the Web Notifications API via `web_sys` so the rest of the app can
//! request permission, check support, and fire desktop notifications without
//! touching raw JS interop.

use wasm_bindgen::prelude::*;
use web_sys::{Notification, NotificationOptions, NotificationPermission};

/// Check whether the browser supports the Notification API.
pub fn is_notification_supported() -> bool {
    // `Notification` is only available if the global `window.Notification`
    // constructor exists. We probe via js_sys::eval to avoid a hard panic when
    // the feature isn't present (e.g. in some WebView contexts).
    js_sys::eval("typeof Notification !== 'undefined'")
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Request notification permission from the user.
///
/// Calls `Notification.requestPermission()` which returns a Promise.
/// This is a fire-and-forget helper — the resolved permission string is
/// logged to the console but not surfaced to the caller because the
/// current permission can always be read synchronously via
/// `Notification.permission`.
pub fn request_notification_permission() {
    if !is_notification_supported() {
        return;
    }

    // `Notification.requestPermission()` returns a Promise<NotificationPermission>.
    // We convert it to a Rust future and spawn it.
    let promise = Notification::request_permission().unwrap_or_else(|_| {
        // Fallback: return a resolved promise with "denied"
        js_sys::Promise::resolve(&JsValue::from_str("denied"))
    });

    let future = wasm_bindgen_futures::JsFuture::from(promise);
    wasm_bindgen_futures::spawn_local(async move {
        match future.await {
            Ok(val) => {
                let perm = val.as_string().unwrap_or_default();
                web_sys::console::log_1(&JsValue::from_str(&format!(
                    "[notifications] permission: {perm}"
                )));
            }
            Err(e) => {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "[notifications] requestPermission error: {e:?}"
                )));
            }
        }
    });
}

/// Send a browser notification if permission has been granted.
///
/// Does nothing when the API is unsupported or permission is not `"granted"`.
pub fn send_notification(title: &str, body: &str) {
    if !is_notification_supported() {
        return;
    }

    let perm = Notification::permission();
    if perm != NotificationPermission::Granted {
        return;
    }

    let mut opts = NotificationOptions::new();
    opts.set_body(body);
    // icon could be set here if we want: opts.icon("/assets/savfox.svg");

    let _ = Notification::new_with_options(title, &opts);
}
