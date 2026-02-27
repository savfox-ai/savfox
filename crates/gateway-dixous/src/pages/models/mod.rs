pub mod connect_provider;


use dioxus::prelude::*;

use crate::api::types::ModelsResponse;
use crate::api::ws::WsRpc;
use crate::route::Route;
use crate::utils::model_visibility::{
    ModelKey, is_model_visible, load_model_preferences, save_model_preferences,
    set_model_visibility,
};
use crate::utils::provider_catalog::{build_provider_catalog, model_display_name};

async fn delay_ms(ms: i32) {
    let mut cb = |resolve: js_sys::Function, _reject: js_sys::Function| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    };
    let promise = js_sys::Promise::new(&mut cb);
    wasm_bindgen_futures::JsFuture::from(promise).await.ok();
}

#[component]
pub fn Models() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let nav = use_navigator();

    let mut search = use_signal(String::new);
    let mut model_prefs = use_signal(load_model_preferences);

    let ws_models = ws.clone();
    let ws_connected_models = ws_connected;
    let mut models_data = use_resource(move || {
        let connected = ws_connected_models();
        let ws = ws_models.clone();
        async move {
            if !connected {
                return None;
            }
            // Retry up to 3 times with delays to handle race condition
            // where WebSocket connects but server isn't ready yet
            for attempt in 0..3 {
                if let Ok(resp) = ws.call::<ModelsResponse>("models.list", None).await {
                    return Some(resp.models);
                }
                // Small delay before retry (except on last attempt)
                if attempt < 2 {
                    delay_ms(100).await;
                }
            }
            None
        }
    });

    let ws_is_connected = ws_connected();
    let models_read = models_data.read();
    let loading = !ws_is_connected || models_read.is_none();
    let models_failed = ws_is_connected && matches!(models_read.as_ref(), Some(None));
    let models_snapshot = models_read
        .as_ref()
        .and_then(|models| models.as_ref())
        .cloned()
        .unwrap_or_default();

    let catalog = build_provider_catalog(&models_snapshot);

    let query = search().trim().to_lowercase();
    let prefs_snapshot = model_prefs();

    let mut grouped_models: Vec<(String, Vec<(ModelKey, String, String, String, bool)>)> = vec![];
    let mut total_models = 0usize;
    let mut visible_models = 0usize;

    for provider in catalog.all.iter() {
        let mut provider_rows = vec![];
        for model in provider.models.values() {
            total_models += 1;
            let key = ModelKey::new(provider.id.clone(), model.model_id.clone());
            let is_visible = is_model_visible(&prefs_snapshot, &key);
            if is_visible {
                visible_models += 1;
            }

            let display = model_display_name(model);
            let search_blob = format!(
                "{} {} {} {}",
                provider.id.to_lowercase(),
                provider.name.to_lowercase(),
                model.model_id.to_lowercase(),
                display.to_lowercase(),
            );
            if !query.is_empty() && !search_blob.contains(&query) {
                continue;
            }

            provider_rows.push((
                key,
                model.full_id.clone(),
                display,
                model.model_id.clone(),
                is_visible,
            ));
        }

        provider_rows.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.1.cmp(&b.1)));
        if !provider_rows.is_empty() {
            grouped_models.push((provider.name.clone(), provider_rows));
        }
    }

    rsx! {
        div { class: "models-page",
            div { class: "models-shell",
                div { class: "models-header",
                    div { class: "models-header__text",
                        h2 { class: "models-title", "Manage models" }
                        p { class: "models-subtitle",
                            "Customize which models appear in the model selector."
                        }
                    }
                    button {
                        class: "models-connect-btn",
                        onclick: move |_| {
                            nav.push(Route::ConnectProvider {});
                        },
                        span { class: "models-connect-btn__icon", "+" }
                        span { "Connect provider" }
                    }
                }

                div { class: "models-search",
                    span { class: "models-search__icon", "⌕" }
                    input {
                        class: "models-search__input",
                        value: "{search}",
                        oninput: move |e: Event<FormData>| search.set(e.value()),
                        placeholder: "Search models",
                        spellcheck: false,
                        autocorrect: "off",
                        autocomplete: "off",
                        autocapitalize: "off",
                    }
                }

                div { class: "models-count",
                    "{visible_models} visible / {total_models} total"
                }

                div { class: "models-groups",
                    if loading {
                        div { class: "models-state models-state--muted", "Loading models..." }
                    } else if models_failed {
                        div { class: "models-state models-state--muted models-state--stack",
                            span { "Failed to load model settings." }
                            button {
                                class: "models-retry-btn",
                                onclick: move |_| {
                                    models_data.restart();
                                },
                                "Retry"
                            }
                        }
                    } else if grouped_models.is_empty() {
                        div { class: "models-state models-state--muted", "No matching models" }
                    } else {
                        for (provider_name, provider_models) in grouped_models.iter() {
                            {
                                let provider_title = provider_name.clone();
                                rsx! {
                                    section { class: "models-group",
                                        h3 { class: "models-group__title", "{provider_title}" }
                                        div { class: "models-group__list",
                                            for row in provider_models.iter() {
                                                {
                                                    let model_key = row.0.clone();
                                                    let model_full_id = row.1.clone();
                                                    let model_name = row.2.clone();
                                                    let model_id = row.3.clone();
                                                    let is_visible = row.4;
                                                    let show_meta = model_name != model_id;
                                                    let row_class = if is_visible {
                                                        "models-row"
                                                    } else {
                                                        "models-row models-row--off"
                                                    };
                                                    let toggle_class = if is_visible {
                                                        "models-toggle models-toggle--on"
                                                    } else {
                                                        "models-toggle"
                                                    };
                                                    rsx! {
                                                        div {
                                                            key: "{model_full_id}",
                                                            class: "{row_class}",
                                                            div { class: "models-row__text",
                                                                div { class: "models-row__name", "{model_name}" }
                                                                if show_meta {
                                                                    div { class: "models-row__meta", "{model_id}" }
                                                                }
                                                            }
                                                            button {
                                                                class: "{toggle_class}",
                                                                title: if is_visible { "Hide model" } else { "Show model" },
                                                                aria_label: if is_visible { "Hide model" } else { "Show model" },
                                                                onclick: move |_| {
                                                                    let mut next = model_prefs();
                                                                    set_model_visibility(&mut next, &model_key, !is_visible);
                                                                    save_model_preferences(&next);
                                                                    model_prefs.set(next);
                                                                },
                                                                span { class: "models-toggle__thumb" }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        style { {MODELS_STYLES} }
    }
}

const MODELS_STYLES: &str = r#"
    .models-page {
        height: 100%;
        overflow: auto;
        padding: 20px 18px 24px;
    }

    .models-shell {
        width: min(760px, 100%);
        margin: 0 auto;
        display: flex;
        flex-direction: column;
        gap: 14px;
    }

    .models-header {
        display: flex;
        align-items: flex-start;
        justify-content: space-between;
        gap: 12px;
    }

    .models-header__text {
        min-width: 0;
        display: flex;
        flex-direction: column;
        gap: 6px;
    }

    .models-title {
        margin: 0;
        font-size: 26px;
        font-weight: 700;
        color: var(--text-primary);
        letter-spacing: -0.02em;
    }

    .models-subtitle {
        margin: 0;
        font-size: 14px;
        color: var(--text-muted);
    }

    .models-subtitle--hint {
        font-size: 12px;
        color: color-mix(in srgb, var(--text-muted) 85%, var(--accent) 15%);
    }

    .models-connect-btn {
        flex-shrink: 0;
        height: 34px;
        border-radius: 10px;
        border: 1px solid var(--border);
        background: color-mix(in srgb, var(--bg-tertiary) 88%, var(--bg-secondary) 12%);
        color: var(--text-primary);
        padding: 0 12px;
        display: inline-flex;
        align-items: center;
        gap: 7px;
        font-size: 13px;
        font-weight: 600;
        cursor: pointer;
    }

    .models-connect-btn:hover {
        border-color: var(--accent);
    }

    .models-connect-btn__icon {
        font-size: 14px;
        line-height: 1;
        color: var(--text-muted);
    }

    .models-search {
        height: 36px;
        border-radius: 10px;
        border: 1px solid var(--border);
        background: var(--bg-tertiary);
        display: flex;
        align-items: center;
        gap: 8px;
        padding: 0 10px;
    }

    .models-search__icon {
        flex-shrink: 0;
        font-size: 11px;
        color: var(--text-muted);
    }

    .models-search__input {
        flex: 1;
        min-width: 0;
        border: none;
        background: transparent;
        outline: none;
        color: var(--text-primary);
        font-size: 13px;
    }

    .models-search__input::placeholder {
        color: var(--text-muted);
    }

    .models-count {
        font-size: 12px;
        color: var(--text-muted);
    }

    .models-groups {
        display: flex;
        flex-direction: column;
        gap: 18px;
    }

    .models-group {
        display: flex;
        flex-direction: column;
        gap: 8px;
    }

    .models-group__title {
        margin: 0;
        font-size: 28px;
        line-height: 1.1;
        color: color-mix(in srgb, var(--accent) 50%, var(--text-secondary) 50%);
        letter-spacing: -0.02em;
        font-weight: 650;
    }

    .models-group__list {
        border-radius: 12px;
        border: 1px solid var(--border);
        background: var(--bg-secondary);
        overflow: hidden;
    }

    .models-row {
        min-height: 46px;
        padding: 8px 12px;
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 10px;
        border-bottom: 1px solid var(--border);
    }

    .models-row:last-child {
        border-bottom: none;
    }

    .models-row__text {
        min-width: 0;
        display: flex;
        flex-direction: column;
        gap: 2px;
    }

    .models-row__name {
        min-width: 0;
        font-size: 17px;
        font-weight: 600;
        color: var(--text-primary);
        white-space: nowrap;
        text-overflow: ellipsis;
        overflow: hidden;
    }

    .models-row__meta {
        font-size: 11px;
        color: var(--text-muted);
        line-height: 1.2;
        word-break: break-all;
    }

    .models-row--off .models-row__name {
        opacity: 0.54;
    }

    .models-toggle {
        width: 34px;
        height: 20px;
        border-radius: 999px;
        border: 1px solid color-mix(in srgb, var(--border) 72%, #000 28%);
        background: color-mix(in srgb, var(--bg-tertiary) 82%, #000 18%);
        padding: 1px;
        display: inline-flex;
        align-items: center;
        cursor: pointer;
        transition: background 0.15s ease, border-color 0.15s ease;
    }

    .models-toggle__thumb {
        width: 14px;
        height: 14px;
        border-radius: 50%;
        background: #f2f2f2;
        transform: translateX(0);
        transition: transform 0.15s ease;
    }

    .models-toggle--on {
        background: color-mix(in srgb, var(--text-primary) 16%, var(--bg-hover) 84%);
        border-color: color-mix(in srgb, var(--text-primary) 38%, var(--border) 62%);
    }

    .models-toggle--on .models-toggle__thumb {
        transform: translateX(14px);
        background: #fff;
    }

    .models-state {
        min-height: 120px;
        border: 1px dashed var(--border);
        border-radius: 12px;
        display: flex;
        align-items: center;
        justify-content: center;
        font-size: 13px;
        padding: 14px;
    }

    .models-state--muted {
        color: var(--text-muted);
    }

    .models-state--stack {
        flex-direction: column;
        gap: 10px;
    }

    .models-retry-btn {
        border: 1px solid var(--border);
        background: var(--bg-secondary);
        color: var(--text-primary);
        border-radius: 8px;
        padding: 6px 12px;
        font-size: 12px;
        cursor: pointer;
    }

    .models-retry-btn:hover {
        border-color: var(--accent);
    }

    @media (max-width: 640px) {
        .models-page {
            padding: 14px 10px 18px;
        }

        .models-header {
            flex-direction: column;
        }

        .models-connect-btn {
            width: 100%;
            justify-content: center;
        }

        .models-title {
            font-size: 22px;
        }

        .models-group__title {
            font-size: 22px;
        }

        .models-row__name {
            font-size: 15px;
        }
    }
"#;
