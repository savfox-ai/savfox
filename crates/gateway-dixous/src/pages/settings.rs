use dioxus::prelude::*;
use serde_json::json;

use crate::api::ws::WsRpc;

/// Settings page — theme, notifications, default model, gateway URL.
/// Preferences are stored in localStorage.
#[component]
pub fn Settings() -> Element {
    let ws = use_context::<WsRpc>();
    let mut theme = use_signal(|| "system".to_string());
    let mut notifications = use_signal(|| true);
    let mut default_model = use_signal(|| String::new());
    let mut gateway_url = use_signal(|| String::new());
    let mut saved_msg = use_signal(|| Option::<String>::None);
    let mut models_list = use_signal(|| Vec::<String>::new());

    // Load settings from localStorage on mount
    use_effect(move || {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window() {
                if let Ok(Some(storage)) = window.local_storage() {
                    if let Ok(Some(v)) = storage.get_item("savfox_theme") {
                        theme.set(v);
                    }
                    if let Ok(Some(v)) = storage.get_item("savfox_notifications") {
                        notifications.set(v == "true");
                    }
                    if let Ok(Some(v)) = storage.get_item("savfox_default_model") {
                        default_model.set(v);
                    }
                    if let Ok(Some(v)) = storage.get_item("savfox_gateway_url") {
                        gateway_url.set(v);
                    }
                }
            }
        }
    });

    // Fetch available models
    let ws_models = ws.clone();
    use_effect(move || {
        let ws = ws_models.clone();
        spawn(async move {
            if let Ok(resp) = ws
                .call::<serde_json::Value>("models.list", Some(json!({})))
                .await
            {
                if let Some(models) = resp.get("models").and_then(|v| v.as_array()) {
                    let ids: Vec<String> = models
                        .iter()
                        .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
                        .collect();
                    models_list.set(ids);
                }
            }
        });
    });

    let save_settings = move |_| {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window() {
                if let Ok(Some(storage)) = window.local_storage() {
                    let _ = storage.set_item("savfox_theme", &theme());
                    let _ = storage.set_item("savfox_notifications", &notifications().to_string());
                    let _ = storage.set_item("savfox_default_model", &default_model());
                    let _ = storage.set_item("savfox_gateway_url", &gateway_url());
                }
            }
        }
        saved_msg.set(Some("Settings saved".to_string()));
    };

    rsx! {
        div { class: "page-content",
            div { class: "page-header",
                h2 { "Settings" }
                if let Some(msg) = saved_msg() {
                    span { class: "badge badge-success", "{msg}" }
                }
            }

            div { class: "card",
                h3 { class: "card-title", "Appearance" }
                div { class: "form-group",
                    label { "Theme" }
                    select {
                        class: "form-select",
                        value: "{theme}",
                        onchange: move |e| theme.set(e.value()),
                        option { value: "system", "System (auto)" }
                        option { value: "dark", "Dark" }
                        option { value: "light", "Light" }
                    }
                }
            }

            div { class: "card",
                h3 { class: "card-title", "Notifications" }
                div { class: "form-group",
                    label {
                        input {
                            r#type: "checkbox",
                            checked: notifications(),
                            onchange: move |e| notifications.set(e.checked()),
                        }
                        " Enable desktop notifications"
                    }
                }
            }

            div { class: "card",
                h3 { class: "card-title", "Default Model" }
                div { class: "form-group",
                    label { "Model" }
                    select {
                        class: "form-select",
                        value: "{default_model}",
                        onchange: move |e| default_model.set(e.value()),
                        option { value: "", "-- None --" }
                        for model_id in models_list() {
                            option { value: "{model_id}", "{model_id}" }
                        }
                    }
                }
            }

            div { class: "card",
                h3 { class: "card-title", "Connection" }
                div { class: "form-group",
                    label { "Gateway URL" }
                    input {
                        class: "form-input",
                        r#type: "text",
                        placeholder: "ws://localhost:18881/ws",
                        value: "{gateway_url}",
                        oninput: move |e| gateway_url.set(e.value()),
                    }
                    small { class: "form-help", "Leave empty to use the default (same host)." }
                }
            }

            div { class: "card",
                h3 { class: "card-title", "Keyboard Shortcuts" }
                div { class: "shortcuts-list",
                    div { class: "shortcut-row",
                        span { class: "shortcut-key", "Ctrl+K" }
                        span { class: "shortcut-desc", "Open command palette" }
                    }
                    div { class: "shortcut-row",
                        span { class: "shortcut-key", "Ctrl+N" }
                        span { class: "shortcut-desc", "New session" }
                    }
                    div { class: "shortcut-row",
                        span { class: "shortcut-key", "Ctrl+/" }
                        span { class: "shortcut-desc", "Focus chat input" }
                    }
                    div { class: "shortcut-row",
                        span { class: "shortcut-key", "Escape" }
                        span { class: "shortcut-desc", "Close modal / palette" }
                    }
                }
            }

            div { class: "form-actions",
                button {
                    class: "btn btn-primary",
                    onclick: save_settings,
                    "Save Settings"
                }
            }
        }
    }
}
