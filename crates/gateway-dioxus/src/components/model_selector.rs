use dioxus::prelude::*;

use crate::api::types::{ModelInfo, ModelsResponse};
use crate::api::ws::WsRpc;
use crate::i18n::use_i18n;
use crate::route::Route;
use crate::utils::model_visibility::{
    ModelKey, is_model_visible, load_model_preferences, push_recent_model, save_model_preferences,
    set_model_visibility,
};
use crate::utils::provider_catalog::{
    build_provider_catalog, find_model_by_full_id, first_default_full_id, model_display_name,
};

const DEFAULT_MODEL_STORAGE_KEY: &str = "savfox_default_model";

fn fallback_model_display_name(full_id: &str) -> String {
    let trimmed = full_id.trim();
    if trimmed.is_empty() {
        // Fallback; the translated label is used by the component
        return "Default Model".to_string();
    }
    if let Some((_, model_id)) = trimmed.split_once('/') {
        let model_id = model_id.trim();
        if !model_id.is_empty() {
            return model_id.to_string();
        }
    }
    trimmed.to_string()
}

fn model_info_display_name(model: &ModelInfo) -> String {
    model
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            model
                .model_slug
                .as_deref()
                .map(str::trim)
                .filter(|code| !code.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| fallback_model_display_name(&model.id))
}

fn load_saved_default_model_id() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        return web_sys::window()
            .and_then(|window| window.local_storage().ok())
            .flatten()
            .and_then(|storage| storage.get_item(DEFAULT_MODEL_STORAGE_KEY).ok())
            .flatten()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

#[component]
pub fn ModelSelector(value: String, on_change: EventHandler<String>) -> Element {
    let (_locale_sig, t) = use_i18n();
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let nav = use_navigator();
    let mut dropdown_open = use_signal(|| false);
    let mut search = use_signal(String::new);
    let mut model_prefs = use_signal(load_model_preferences);

    let ws_models = ws.clone();
    let ws_connected_models = ws_connected;
    let mut models = use_resource(move || {
        let connected = ws_connected_models();
        let ws = ws_models.clone();
        async move {
            if !connected {
                return None;
            }
            ws.call::<ModelsResponse>("models.list", None)
                .await
                .ok()
                .map(|r| r.models)
        }
    });

    // Watch for WebSocket connection changes and refetch models when connected
    let mut prev_connected = use_signal(|| false);
    use_effect(move || {
        let now_connected = ws_connected();
        if now_connected && !prev_connected() {
            prev_connected.set(true);
            models.restart();
        } else if !now_connected {
            prev_connected.set(false);
        }
    });

    let ws_is_connected = ws_connected();
    let models_read = models.read();
    let loading = !ws_is_connected || models_read.is_none();
    let models_failed = ws_is_connected && matches!(models_read.as_ref(), Some(None));
    let models_snapshot = models_read
        .as_ref()
        .and_then(|result| result.as_ref())
        .cloned()
        .unwrap_or_default();

    let catalog = build_provider_catalog(&models_snapshot);
    let default_full_id = first_default_full_id(&catalog);
    let saved_default_full_id = load_saved_default_model_id();
    let default_model_name = default_full_id
        .as_deref()
        .and_then(|full_id| {
            find_model_by_full_id(&catalog, full_id).map(|(_, model)| model_display_name(model))
        })
        .or_else(|| {
            saved_default_full_id.as_deref().map(|full_id| {
                find_model_by_full_id(&catalog, full_id)
                    .map(|(_, model)| model_display_name(model))
                    .unwrap_or_else(|| fallback_model_display_name(full_id))
            })
        })
        .or_else(|| {
            models_snapshot
                .iter()
                .find(|model| model.is_default.unwrap_or(false))
                .or_else(|| models_snapshot.first())
                .map(model_info_display_name)
        })
        .unwrap_or_else(|| t("model_selector.default_model"));
    let default_suffix = t("model_selector.default_suffix");
    let default_model_label = format!("{default_model_name} {default_suffix}");

    let selected_label = if value == "default" {
        default_model_label.clone()
    } else {
        find_model_by_full_id(&catalog, &value)
            .map(|(provider, model)| {
                let provider_name = provider.name.clone();
                format!("{provider_name} / {}", model_display_name(model))
            })
            .unwrap_or_else(|| value.clone())
    };

    let query = search().trim().to_lowercase();
    let mut grouped_models: Vec<(String, Vec<(String, ModelKey, String, String)>)> = vec![];
    for provider in catalog.all.iter() {
        let mut filtered = vec![];
        for model in provider.models.values() {
            let key = ModelKey::new(provider.id.clone(), model.model_id.clone());
            if !is_model_visible(&model_prefs(), &key) && value != model.full_id {
                continue;
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
            filtered.push((model.full_id.clone(), key, display, model.model_id.clone()));
        }
        filtered.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
        if !filtered.is_empty() {
            grouped_models.push((provider.name.clone(), filtered));
        }
    }

    let default_search_blob = format!("default {}", default_model_label.to_lowercase());
    let default_row_visible = query.is_empty() || default_search_blob.contains(&query);
    let mut visible_count = grouped_models
        .iter()
        .map(|(_, models)| models.len())
        .sum::<usize>();
    if default_row_visible {
        visible_count += 1;
    }

    let ms_choose = t("model_selector.choose_model");
    let ms_search = t("model_selector.search_models");
    let ms_connect = t("model_selector.connect_provider");
    let ms_manage_providers = t("model_selector.manage_providers");
    let ms_manage = t("model_selector.manage");
    let ms_loading = t("model_selector.loading_models");
    let ms_failed = t("model_selector.failed_to_load");
    let ms_retry = t("model_selector.retry");
    let ms_no_matching = t("model_selector.no_matching");
    let ms_default_group = t("model_selector.default_group");
    let ms_follow_default = t("model_selector.follow_system_default");

    rsx! {
        div { class: "model-selector",
            button {
                class: if dropdown_open() {
                    "model-selector__trigger model-selector__trigger--open"
                } else {
                    "model-selector__trigger"
                },
                title: "{ms_choose}",
                onclick: move |_| {
                    if dropdown_open() {
                        dropdown_open.set(false);
                        search.set(String::new());
                    } else {
                        dropdown_open.set(true);
                    }
                },
                span { class: "model-selector__trigger-label", "{selected_label}" }
                span { class: "model-selector__trigger-caret", "v" }
            }

            if dropdown_open() {
                div {
                    class: "model-selector__backdrop",
                    onclick: move |_| {
                        dropdown_open.set(false);
                        search.set(String::new());
                    },
                }
                div { class: "model-selector__menu", onclick: |e| e.stop_propagation(),
                    div { class: "model-selector__toolbar",
                        input {
                            class: "model-selector__search",
                            value: "{search}",
                            oninput: move |e: Event<FormData>| search.set(e.value()),
                            placeholder: "{ms_search}",
                            aria_label: "{ms_search}",
                        }
                        button {
                            class: "model-selector__tool-btn",
                            title: "{ms_connect}",
                            aria_label: "{ms_connect}",
                            onclick: move |e| {
                                e.stop_propagation();
                                dropdown_open.set(false);
                                search.set(String::new());
                                nav.push(Route::ConnectProvider {});
                            },
                            "+"
                        }
                        button {
                            class: "model-selector__tool-btn model-selector__tool-btn--manage",
                            title: "{ms_manage_providers}",
                            aria_label: "{ms_manage_providers}",
                            onclick: move |e| {
                                e.stop_propagation();
                                dropdown_open.set(false);
                                search.set(String::new());
                                nav.push(Route::ConfigSection { section: "models".to_string() });
                            },
                            "{ms_manage}"
                        }
                    }

                    div { class: "model-selector__list",
                        if loading {
                            div { class: "model-selector__empty", "{ms_loading}" }
                        } else if models_failed {
                            div { class: "model-selector__empty model-selector__empty--stack",
                                span { "{ms_failed}" }
                                button {
                                    class: "model-selector__retry",
                                    onclick: move |_| {
                                        models.restart();
                                    },
                                    "{ms_retry}"
                                }
                            }
                        } else if visible_count == 0 {
                            div { class: "model-selector__empty", "{ms_no_matching}" }
                        } else {
                            if default_row_visible {
                                div { class: "model-selector__group",
                                    div { class: "model-selector__group-title", "{ms_default_group}" }
                                    button {
                                        class: if value == "default" {
                                            "model-selector__item model-selector__item--active"
                                        } else {
                                            "model-selector__item"
                                        },
                                        onclick: move |_| {
                                            on_change("default".to_string());
                                            dropdown_open.set(false);
                                            search.set(String::new());
                                        },
                                        div { class: "model-selector__item-main",
                                            div { class: "model-selector__item-name", "{default_model_name}" }
                                            div { class: "model-selector__item-id", "{ms_follow_default}" }
                                        }
                                        if value == "default" {
                                            span { class: "model-selector__check", "v" }
                                        }
                                    }
                                }
                            }
                            for (provider, provider_models) in grouped_models.iter() {
                                div { class: "model-selector__group",
                                    div { class: "model-selector__group-title", "{provider}" }
                                    for model in provider_models.iter() {
                                        {
                                            let full_id = model.0.clone();
                                            let key = model.1.clone();
                                            let model_name = model.2.clone();
                                            let model_id = model.3.clone();
                                            let show_id = model_name != model_id;
                                            let item_class = if value == full_id {
                                                "model-selector__item model-selector__item--active"
                                            } else {
                                                "model-selector__item"
                                            };
                                            rsx! {
                                                button {
                                                    key: "{full_id}",
                                                    class: "{item_class}",
                                                    onclick: move |_| {
                                                        let mut prefs = model_prefs();
                                                        set_model_visibility(&mut prefs, &key, true);
                                                        push_recent_model(&mut prefs, &key, 5);
                                                        save_model_preferences(&prefs);
                                                        model_prefs.set(prefs);
                                                        on_change(full_id.clone());
                                                        dropdown_open.set(false);
                                                        search.set(String::new());
                                                    },
                                                    div { class: "model-selector__item-main",
                                                        div { class: "model-selector__item-name", "{model_name}" }
                                                        if show_id {
                                                            div { class: "model-selector__item-id", "{model_id}" }
                                                        }
                                                    }
                                                    if value == full_id {
                                                        span { class: "model-selector__check", "v" }
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
        style { {MODEL_SELECTOR_STYLES} }
    }
}

const MODEL_SELECTOR_STYLES: &str = r#"
    .model-selector {
        position: relative;
        min-width: 210px;
    }

    .model-selector__trigger {
        width: 100%;
        height: 38px;
        border-radius: 10px;
        border: 1px solid var(--border);
        background: color-mix(in srgb, var(--bg-tertiary) 90%, var(--bg-secondary) 10%);
        color: var(--text-primary);
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 8px;
        padding: 0 10px;
        font-size: 12px;
        cursor: pointer;
    }

    .model-selector__trigger:hover {
        border-color: var(--accent);
    }

    .model-selector__trigger--open {
        border-color: var(--accent);
        box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent) 40%, transparent 60%);
    }

    .model-selector__trigger-label {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        text-align: left;
    }

    .model-selector__trigger-caret {
        color: var(--text-muted);
        font-size: 11px;
        line-height: 1;
    }

    .model-selector__backdrop {
        position: fixed;
        inset: 0;
        z-index: 310;
        background: transparent;
    }

    .model-selector__menu {
        position: absolute;
        left: 0;
        bottom: calc(100% + 8px);
        width: min(420px, 86vw);
        max-height: 420px;
        border: 1px solid var(--border);
        border-radius: 12px;
        background: var(--bg-secondary);
        box-shadow: 0 14px 36px rgba(0, 0, 0, 0.35);
        display: flex;
        flex-direction: column;
        z-index: 311;
        overflow: hidden;
    }

    .model-selector__toolbar {
        display: flex;
        align-items: center;
        gap: 8px;
        padding: 10px;
        border-bottom: 1px solid var(--border);
    }

    .model-selector__search {
        flex: 1;
        min-width: 0;
        height: 34px;
        border-radius: 9px;
        border: 1px solid var(--border);
        background: var(--bg-tertiary);
        color: var(--text-primary);
        padding: 0 10px;
        font-size: 12px;
        outline: none;
    }

    .model-selector__search:focus {
        border-color: var(--accent);
    }

    .model-selector__tool-btn {
        height: 34px;
        min-width: 34px;
        border-radius: 9px;
        border: 1px solid var(--border);
        background: var(--bg-tertiary);
        color: var(--text-secondary);
        padding: 0 10px;
        font-size: 12px;
        cursor: pointer;
        white-space: nowrap;
    }

    .model-selector__tool-btn:hover {
        border-color: var(--accent);
        color: var(--text-primary);
    }

    .model-selector__tool-btn--manage {
        min-width: 74px;
    }

    .model-selector__list {
        overflow: auto;
        padding: 8px;
        display: flex;
        flex-direction: column;
        gap: 8px;
    }

    .model-selector__group {
        display: flex;
        flex-direction: column;
        gap: 4px;
    }

    .model-selector__group + .model-selector__group {
        margin-top: 4px;
        padding-top: 8px;
        border-top: 1px solid var(--border);
    }

    .model-selector__group-title {
        font-size: 11px;
        font-weight: 700;
        color: var(--text-secondary);
        text-transform: uppercase;
        letter-spacing: 0.05em;
        padding: 4px 8px;
        background: color-mix(in srgb, var(--bg-tertiary) 60%, transparent);
        border-radius: 6px;
        margin-bottom: 2px;
    }

    .model-selector__item {
        width: 100%;
        border: 1px solid transparent;
        border-radius: 10px;
        background: transparent;
        color: var(--text-primary);
        text-align: left;
        padding: 8px 10px;
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 8px;
        cursor: pointer;
    }

    .model-selector__item:hover {
        background: var(--bg-hover);
        border-color: var(--border);
    }

    .model-selector__item--active {
        border-color: var(--accent);
        background: color-mix(in srgb, var(--accent) 12%, var(--bg-hover) 88%);
    }

    .model-selector__item-main {
        min-width: 0;
        display: flex;
        flex-direction: column;
        gap: 2px;
    }

    .model-selector__item-name {
        font-size: 14px;
        line-height: 1.25;
    }

    .model-selector__item-id {
        font-size: 11px;
        color: var(--text-muted);
        line-height: 1.2;
        word-break: break-all;
    }

    .model-selector__check {
        font-size: 12px;
        color: var(--text-primary);
        opacity: 0.8;
    }

    .model-selector__empty {
        font-size: 12px;
        color: var(--text-muted);
        padding: 10px 6px;
    }

    .model-selector__empty--stack {
        display: flex;
        flex-direction: column;
        gap: 8px;
        align-items: flex-start;
    }

    .model-selector__retry {
        border: 1px solid var(--border);
        background: var(--bg-tertiary);
        color: var(--text-primary);
        border-radius: 8px;
        padding: 5px 10px;
        font-size: 11px;
        cursor: pointer;
    }

    .model-selector__retry:hover {
        border-color: var(--accent);
    }

    @media (max-width: 640px) {
        .model-selector {
            min-width: 160px;
        }

        .model-selector__trigger {
            height: 36px;
        }

        .model-selector__menu {
            width: min(94vw, 380px);
            max-height: 56vh;
        }
    }
"#;
