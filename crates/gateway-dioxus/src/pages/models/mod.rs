pub mod connect_provider;

use std::collections::HashMap;

use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::{ModelInfo, ModelsResponse};
use crate::api::ws::WsRpc;
use crate::components::empty_state::EmptyState;
use crate::components::skeleton::*;
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
    let mut expanded_model = use_signal(|| Option::<String>::None);

    // Per-provider test connection state: provider_id -> (testing, Option<(ok, message)>)
    let mut test_states = use_signal(|| HashMap::<String, (bool, Option<(bool, String)>)>::new());

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

    // Build lookup from full_id -> ModelInfo for expanded details
    let model_info_map: std::collections::HashMap<String, &ModelInfo> =
        models_snapshot.iter().map(|m| (m.id.clone(), m)).collect();

    let query = search().trim().to_lowercase();
    let prefs_snapshot = model_prefs();

    let mut grouped_models: Vec<(
        String,
        String,
        String,
        Vec<(ModelKey, String, String, String, bool)>,
    )> = vec![];
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
            // When account_slug is present, show just the base provider name
            // (e.g. "OpenAI") since the account label is rendered separately.
            let title = if !provider.account_slug.is_empty() {
                provider
                    .name
                    .split(" / ")
                    .next()
                    .unwrap_or(&provider.name)
                    .to_string()
            } else {
                provider.name.clone()
            };
            grouped_models.push((
                provider.id.clone(),
                title,
                provider.account_slug.clone(),
                provider_rows,
            ));
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
                        div { class: "models-state models-state--muted",
                            SkeletonLines { count: 3 }
                        }
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
                    } else if grouped_models.is_empty() && total_models == 0 && query.is_empty() {
                        EmptyState {
                            icon: "M".to_string(),
                            message: "No models configured. Connect a provider to get started.".to_string(),
                            action_label: "Connect Provider".to_string(),
                            on_action: move |_| {
                                nav.push(Route::ConnectProvider {});
                            },
                        }
                    } else if grouped_models.is_empty() {
                        div { class: "models-state models-state--muted", "No matching models" }
                    } else {
                        for (provider_id, provider_name, account_slug, provider_models) in grouped_models.iter() {
                            {
                                let provider_title = provider_name.clone();
                                let account_label = account_slug.clone();
                                let pid = provider_id.clone();
                                let pid_for_btn = provider_id.clone();
                                let test_state = test_states().get(&pid).cloned().unwrap_or((false, None));
                                let is_testing = test_state.0;
                                let test_result = test_state.1.clone();
                                let ws_test = ws.clone();
                                rsx! {
                                    section { class: "models-group",
                                        div { class: "models-group__header",
                                            div { class: "models-group__title-row",
                                                h3 { class: "models-group__title", "{provider_title}" }
                                                if !account_label.is_empty() {
                                                    span { class: "models-group__account", "{account_label}" }
                                                }
                                            }
                                            div { class: "models-group__test",
                                                if let Some((ok, ref msg)) = test_result {
                                                    span {
                                                        class: if ok { "models-test-result models-test-result--ok" } else { "models-test-result models-test-result--err" },
                                                        if ok { "\u{2713} " } else { "\u{2717} " }
                                                        "{msg}"
                                                    }
                                                }
                                                button {
                                                    class: "models-test-btn",
                                                    disabled: is_testing,
                                                    onclick: move |_| {
                                                        let pid = pid_for_btn.clone();
                                                        let ws = ws_test.clone();
                                                        // Mark as testing
                                                        let mut states = test_states();
                                                        states.insert(pid.clone(), (true, None));
                                                        test_states.set(states);
                                                        spawn(async move {
                                                            let result = ws.call::<serde_json::Value>(
                                                                "models.test",
                                                                Some(json!({ "provider": pid })),
                                                            ).await;
                                                            let mut states = test_states();
                                                            match result {
                                                                Ok(v) => {
                                                                    let ok = v.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
                                                                    let msg = v.get("message").and_then(|v| v.as_str())
                                                                        .unwrap_or(if ok { "Connection successful" } else { "Connection failed" })
                                                                        .to_string();
                                                                    states.insert(pid.clone(), (false, Some((ok, msg))));
                                                                }
                                                                Err(e) => {
                                                                    states.insert(pid.clone(), (false, Some((false, format!("{e}")))));
                                                                }
                                                            }
                                                            test_states.set(states);
                                                        });
                                                    },
                                                    if is_testing { "Testing..." } else { "Test" }
                                                }
                                            }
                                        }
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
                                                    let is_expanded = expanded_model() == Some(model_full_id.clone());
                                                    let expand_id = model_full_id.clone();

                                                    // Gather extra info for expanded details
                                                    let info = model_info_map.get(&model_full_id);
                                                    let reasoning_levels: Vec<String> = info
                                                        .and_then(|m| m.supported_reasoning_levels.as_ref())
                                                        .map(|levels| levels.iter().map(|l| l.effort.clone()).collect())
                                                        .unwrap_or_default();
                                                    let max_tokens = info.and_then(|m| m.max_tokens);
                                                    let temperature = info.and_then(|m| m.temperature);
                                                    let default_reasoning = info.and_then(|m| m.default_reasoning_level.clone());
                                                    let base_url = info.and_then(|m| m.base_url.clone());

                                                    rsx! {
                                                        div {
                                                            key: "{model_full_id}",
                                                            class: "{row_class}",
                                                            div {
                                                                class: "models-row__text",
                                                                role: "button",
                                                                tabindex: "0",
                                                                onclick: move |_| {
                                                                    if expanded_model() == Some(expand_id.clone()) {
                                                                        expanded_model.set(None);
                                                                    } else {
                                                                        expanded_model.set(Some(expand_id.clone()));
                                                                    }
                                                                },
                                                                div { class: "models-row__name",
                                                                    span { "{model_name}" }
                                                                    span {
                                                                        class: "models-row__chevron",
                                                                        if is_expanded { "▾" } else { "▸" }
                                                                    }
                                                                }
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
                                                        if is_expanded {
                                                            div { class: "models-row__details",
                                                                if !reasoning_levels.is_empty() {
                                                                    div { class: "models-detail",
                                                                        span { class: "models-detail__label", "Reasoning levels" }
                                                                        span { class: "models-detail__value", "{reasoning_levels.join(\", \")}" }
                                                                    }
                                                                }
                                                                if let Some(ref def_reasoning) = default_reasoning {
                                                                    div { class: "models-detail",
                                                                        span { class: "models-detail__label", "Default reasoning" }
                                                                        span { class: "models-detail__value", "{def_reasoning}" }
                                                                    }
                                                                }
                                                                if let Some(tokens) = max_tokens {
                                                                    div { class: "models-detail",
                                                                        span { class: "models-detail__label", "Max tokens" }
                                                                        span { class: "models-detail__value", "{tokens}" }
                                                                    }
                                                                }
                                                                if let Some(temp) = temperature {
                                                                    div { class: "models-detail",
                                                                        span { class: "models-detail__label", "Temperature" }
                                                                        span { class: "models-detail__value", "{temp}" }
                                                                    }
                                                                }
                                                                if let Some(ref url) = base_url {
                                                                    div { class: "models-detail",
                                                                        span { class: "models-detail__label", "Base URL" }
                                                                        span { class: "models-detail__value models-detail__value--mono", "{url}" }
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

    .models-group__header {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 10px;
    }

    .models-group__title-row {
        display: flex;
        align-items: baseline;
        gap: 10px;
        min-width: 0;
    }

    .models-group__title {
        margin: 0;
        font-size: 28px;
        line-height: 1.1;
        color: color-mix(in srgb, var(--accent) 50%, var(--text-secondary) 50%);
        letter-spacing: -0.02em;
        font-weight: 650;
        flex-shrink: 0;
    }

    .models-group__account {
        font-size: 16px;
        font-weight: 600;
        color: var(--accent);
        background: var(--accent-bg);
        padding: 2px 10px;
        border-radius: 6px;
        white-space: nowrap;
    }

    .models-group__test {
        display: flex;
        align-items: center;
        gap: 8px;
        min-width: 0;
    }

    .models-test-btn {
        padding: 4px 12px;
        font-size: 12px;
        font-weight: 500;
        border: 1px solid var(--border);
        border-radius: var(--radius);
        background: transparent;
        color: var(--text-secondary);
        cursor: pointer;
        transition: border-color 0.15s, color 0.15s;
        flex-shrink: 0;
    }

    .models-test-btn:hover:not(:disabled) {
        border-color: var(--accent);
        color: var(--text-primary);
    }

    .models-test-btn:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    .models-test-result {
        font-size: 12px;
        font-weight: 500;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        min-width: 0;
    }

    .models-test-result--ok {
        color: var(--success);
    }

    .models-test-result--err {
        color: var(--danger);
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
        cursor: pointer;
        flex: 1;
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
        background: var(--accent);
        border-color: var(--accent);
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

    .models-row__chevron {
        margin-left: 6px;
        font-size: 12px;
        color: var(--text-muted);
        display: inline-block;
        transition: transform 0.15s ease;
    }

    .models-row__details {
        width: 100%;
        padding: 8px 12px 12px;
        border-bottom: 1px solid var(--border);
        background: color-mix(in srgb, var(--bg-tertiary) 50%, var(--bg-secondary) 50%);
        display: flex;
        flex-direction: column;
        gap: 6px;
    }

    .models-row__details:last-child {
        border-bottom: none;
    }

    .models-detail {
        display: flex;
        align-items: baseline;
        gap: 8px;
        font-size: 12px;
    }

    .models-detail__label {
        color: var(--text-muted);
        min-width: 120px;
        flex-shrink: 0;
        font-weight: 500;
    }

    .models-detail__value {
        color: var(--text-secondary);
        word-break: break-all;
    }

    .models-detail__value--mono {
        font-family: var(--font-mono);
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
