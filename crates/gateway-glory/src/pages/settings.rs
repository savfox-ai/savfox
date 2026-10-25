use glory::prelude::*;
use serde_json::json;

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
    let filter = ctx.scope.cage(String::new());
    let saved = ctx.scope.cage(String::new());
    let state = query(ctx, "log.get_level", None);
    let models = query(ctx, "models.list", None);
    let level = ctx.scope.cage("info".to_owned());
    let value = state.value;
    ctx.scope.effect(move || {
        let value = value.get();
        if let Some(value) = value["level"].as_str() {
            ui::set(level, value.to_owned());
        }
    });
    let context = ctx.clone();
    let save = ui::action(ui::label_for(locale, "Save preferences"), move || {
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
        ui::set(saved, "Preferences saved".to_owned());
    });
    let context = ctx.clone();
    let log_state = state.clone();
    let save_log = ui::action(ui::label_for(locale, "Apply logging"), move || {
        let value = if filter.get().trim().is_empty() {
            level.get()
        } else {
            filter.get()
        };
        mutate(
            &context,
            &log_state,
            "log.set_level",
            json!({"level":value}),
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
        "Appearance, language, defaults and local connection settings",
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
                    select_source(ctx, "Default model", model, model_items),
                    save,
                ],
            ),
            card(
                ui::label_for(locale, "Logging"),
                vec![
                    status(ctx, &state),
                    choice_for(
                        locale,
                        "Log level",
                        level,
                        &[
                            ("error", "Error"),
                            ("warn", "Warning"),
                            ("info", "Info"),
                            ("debug", "Debug"),
                            ("trace", "Trace"),
                        ],
                    ),
                    ui::field(
                        ui::label_for(locale, "Custom RUST_LOG filter"),
                        filter,
                        false,
                    ),
                    save_log,
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
    if let Some(document) = web_sys::window().and_then(|w| w.document()) {
        if let Some(root) = document.document_element() {
            let _ = root.set_attribute("data-theme", theme);
        }
        if let Some(body) = document.body() {
            let _ = body.set_attribute("data-theme", theme);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = theme;
}
