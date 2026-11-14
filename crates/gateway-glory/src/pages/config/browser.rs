//! Browser-scoped options share the configuration editor, never the server document.
use super::*;

const STORAGE_KEYS: &[(&str, &str)] = &[
    ("theme", "savfox_theme"),
    ("notifications", "savfox_notifications"),
    ("default_model", "savfox_default_model"),
    ("gateway_url", "savfox_gateway_url"),
];

pub(super) fn load(locale: crate::i18n::Locale) -> Value {
    json!({"browser":{
        "theme":preference("savfox_theme", "system"),
        "language":locale.code(),
        "notifications":preference("savfox_notifications", "false") == "true",
        "default_model":preference("savfox_default_model", ""),
        "gateway_url":preference("savfox_gateway_url", "")
    }})
}

pub(super) fn connection_changed(current: &Value, baseline: &Value) -> bool {
    current["browser"]["gateway_url"].as_str().unwrap_or("")
        != baseline["browser"]["gateway_url"].as_str().unwrap_or("")
}

pub(super) fn validation_error(document: &Value) -> Option<&'static str> {
    let url = document["browser"]["gateway_url"].as_str().unwrap_or("");
    if !url.is_empty() && !(url.starts_with("ws://") || url.starts_with("wss://")) {
        Some("Gateway URL must start with ws:// or wss://")
    } else {
        None
    }
}

pub(super) fn save(ctx: &AppContext, document: &Value) -> std::result::Result<(), String> {
    let settings = &document["browser"];
    #[cfg(target_arch = "wasm32")]
    {
        let storage = web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .ok_or_else(|| "Browser storage is unavailable. Settings were not saved.".to_owned())?;
        for (field, key) in STORAGE_KEYS {
            let value = &settings[*field];
            let value = if *field == "notifications" {
                value.as_bool().unwrap_or(false).to_string()
            } else {
                value.as_str().unwrap_or("").to_owned()
            };
            storage
                .set_item(key, &value)
                .map_err(|_| "Browser settings could not be saved.".to_owned())?;
        }
        if settings["notifications"].as_bool().unwrap_or(false) {
            crate::utils::notifications::request_notification_permission();
        }
        if let Some(window) = web_sys::window()
            && let Ok(event) = web_sys::Event::new("savfox:preferences")
        {
            let _ = window.dispatch_event(&event);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = STORAGE_KEYS;
    let locale = crate::i18n::Locale::from_code(settings["language"].as_str().unwrap_or("en"));
    crate::i18n::save_locale(locale);
    ui::set(ctx.locale, locale);
    #[cfg(target_arch = "wasm32")]
    if let Some(document) = web_sys::window().and_then(|window| window.document()) {
        document.set_title(&crate::i18n::t(locale, "app.title"));
        if let Some(root) = document.document_element() {
            let _ = root.set_attribute("lang", locale.code());
        }
    }
    Ok(())
}

/// Absorb saved preferences without losing unsaved editor drafts.
pub(super) fn synchronize(ctx: &AppContext, draft: Cage<Value>, baseline: Cage<Value>) {
    let theme = ctx.scope.cage(preference("savfox_theme", "system"));
    crate::components::watch_theme(ctx, theme);
    let locale = ctx.locale;
    ctx.scope.effect(move || {
        for (key, value) in [
            ("theme", theme.get()),
            ("language", locale.get().code().to_owned()),
        ] {
            let old = baseline.get_untracked()["browser"][key].clone();
            if old != json!(value) {
                let path = ["browser".to_owned(), key.to_owned()];
                if draft.get_untracked()["browser"][key] == old {
                    update(draft, &path, json!(value));
                }
                update(baseline, &path, json!(value));
            }
        }
    });
}

pub(super) fn fields(
    ctx: &AppContext,
    section: &str,
    search: &str,
    draft: Cage<Value>,
    errors: Cage<HashMap<String, String>>,
    models: &Value,
) -> Vec<Box<dyn Widget>> {
    let mut schema = json!({"properties":{
        "theme":{"type":"string","title":"Theme","enum":["system","dark","light"],"x-enum-labels":{"system":"System","dark":"Dark","light":"Light"}},
        "language":{"type":"string","title":"Language","enum":["en","zh-CN"],"x-enum-labels":{"en":"English","zh-CN":"简体中文"}},
        "notifications":{"type":"boolean","title":"Desktop notifications"},
        "gateway_url":{"type":"string","title":"Gateway WebSocket URL (empty uses this host)"},
        "default_model":{"type":"string","title":"Default model for new chats (this browser)"}
    }});
    if section == "context" || !search.is_empty() {
        // Keep the selected model available even if its account is temporarily unavailable.
        let mut values = vec![json!("")];
        let mut labels = json!({"":"Use global default model"});
        for model in array(models, &["models"]) {
            let id = string(&model, &["id"]);
            if !id.is_empty() {
                values.push(json!(id));
                labels[&id] = json!(string(&model, &["name", "id"]));
            }
        }
        let selected = draft.get_untracked()["browser"]["default_model"].clone();
        if selected.as_str().is_some_and(|value| !value.is_empty()) && !values.contains(&selected) {
            values.push(selected);
        }
        schema["properties"]["default_model"]["enum"] = json!(values);
        schema["properties"]["default_model"]["x-enum-labels"] = labels;
    }
    let fields = [
        "theme",
        "language",
        "notifications",
        "gateway_url",
        "default_model",
    ]
    .iter()
    .map(|key| (*key, &schema["properties"][*key]))
    .filter(|(key, value)| {
        let group = match *key {
            "gateway_url" => "gateway",
            "default_model" => "context",
            _ => "interface",
        };
        (!search.is_empty() || section == group)
            && super::editor::matches_search(key, value, &schema, search, ctx.locale.get())
    })
    .map(|(key, value)| {
        super::editor::field(
            ctx,
            draft,
            vec!["browser".into(), key.to_owned()],
            value,
            &schema,
            errors,
            0,
        )
    })
    .collect::<Vec<_>>();
    let locale = ctx.locale;
    if fields.is_empty() {
        Vec::new()
    } else {
        vec![card(
            ui::label(ctx, "This browser"),
            vec![
                paragraph(ui::label(
                    ctx,
                    "These options affect only this browser and take effect when saved.",
                )),
                Box::new(col().fill(fields)),
                Box::new(
                    text()
                        .class("error-message")
                        .content(draft.map(move |document| {
                            validation_error(document)
                                .map(|message| ui::translate(locale.get(), message))
                                .unwrap_or_default()
                        })),
                ),
            ],
        )]
    }
}

fn preference(key: &str, fallback: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .and_then(|storage| storage.get_item(key).ok().flatten())
            .unwrap_or_else(|| fallback.to_owned())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = key;
        fallback.to_owned()
    }
}
