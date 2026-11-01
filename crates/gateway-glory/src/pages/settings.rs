use glory::prelude::*;

use super::common::*;
use crate::i18n::{self, Locale};
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let theme = ctx.scope.cage(preference("savfox_theme", "system"));
    let notifications = ctx
        .scope
        .cage(preference("savfox_notifications", "false") == "true");
    let model = ctx.scope.cage(preference("savfox_default_model", ""));
    let gateway = ctx.scope.cage(preference("savfox_gateway_url", ""));
    let language = ctx.scope.cage(ctx.locale.get().code().to_owned());
    let saved = ctx.scope.cage(String::new());
    crate::components::watch_theme(ctx, theme);
    ctx.scope
        .effect(move || ui::set(language, locale.get().code().to_owned()));
    let models = query(ctx, "models.list", None);
    let context = ctx.clone();
    let save = ui::action(ui::label_for(locale, "Save preferences"), move || {
        let url = gateway.get();
        if !url.is_empty() && !(url.starts_with("ws://") || url.starts_with("wss://")) {
            ui::set(
                saved,
                ui::translate(locale.get(), "Gateway URL must start with ws:// or wss://"),
            );
            return;
        }
        let connection_changed = url != preference("savfox_gateway_url", "");
        for (key, value) in [
            ("savfox_theme", theme.get()),
            ("savfox_notifications", notifications.get().to_string()),
            ("savfox_default_model", model.get()),
            ("savfox_gateway_url", gateway.get()),
        ] {
            save_preference(key, &value);
        }
        let locale = Locale::from_code(&language.get());
        i18n::save_locale(locale);
        ui::set(context.locale, locale);
        apply_theme(&theme.get());
        #[cfg(target_arch = "wasm32")]
        if notifications.get() {
            crate::utils::notifications::request_notification_permission();
        }
        ui::set(
            saved,
            ui::translate(
                locale,
                if connection_changed {
                    "Preferences saved. Reload the page to reconnect to the new gateway."
                } else {
                    "Preferences saved"
                },
            ),
        );
    });
    let model_items = models.value.map(|v| {
        array(v, &["models"])
            .into_iter()
            .map(|v| (string(&v, &["id"]), string(&v, &["name", "id"])))
            .collect::<Vec<_>>()
    });
    ui::localized_page(
        ctx,
        "nav.settings",
        "Preferences for this browser. Server policies are managed in System configuration.",
        vec![
            Box::new(text().content(saved)),
            card(
                ui::label_for(locale, "Appearance"),
                vec![
                    choice_for(
                        locale,
                        "Theme",
                        theme,
                        &[("system", "System"), ("dark", "Dark"), ("light", "Light")],
                    ),
                    choice_for(
                        locale,
                        "Language",
                        language,
                        &[("en", "English"), ("zh-CN", "简体中文")],
                    ),
                    toggle(
                        ui::label_for(locale, "Desktop notifications"),
                        notifications,
                    ),
                ],
            ),
            card(
                ui::label_for(locale, "Connection and defaults"),
                vec![
                    ui::field(
                        ui::label_for(locale, "Gateway WebSocket URL (empty uses this host)"),
                        gateway,
                        false,
                    ),
                    select_source(
                        ctx,
                        "Default model for new chats (this browser)",
                        model,
                        model_items,
                    ),
                    save,
                ],
            ),
        ],
    )
}
fn preference(key: &str, fallback: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s| s.get_item(key).ok().flatten())
            .unwrap_or_else(|| fallback.to_owned())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = key;
        fallback.to_owned()
    }
}
fn save_preference(key: &str, value: &str) {
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(key, value);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (key, value);
}
fn apply_theme(theme: &str) {
    #[cfg(target_arch = "wasm32")]
    if let Some(window) = web_sys::window() {
        let _ = theme;
        if let Ok(event) = web_sys::Event::new("savfox:preferences") {
            let _ = window.dispatch_event(&event);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = theme;
}
