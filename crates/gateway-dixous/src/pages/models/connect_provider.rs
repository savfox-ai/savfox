use std::collections::BTreeSet;

use dioxus::prelude::*;
use serde_json::{Value, json};

use crate::api::types::ModelsResponse;
use crate::api::ws::WsRpc;
use crate::utils::model_visibility::{
    ModelKey, is_model_visible, load_model_preferences, save_model_preferences,
    set_model_visibility,
};
use crate::utils::provider_registry::{
    canonical_provider_id, known_provider_ids, provider_api_key_env, provider_api_key_help_url,
    provider_default_base_url, provider_description, provider_display_name, provider_icon_text,
    provider_needs_api_key, provider_sort_rank,
};

const OPENAI_PROVIDER_ID: &str = "openai";

// ── Provider definitions ────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
struct ProviderInfo {
    id: String,
    name: String,
    description: String,
    icon: String,
    needs_api_key: bool,
    default_base_url: String,
    env_key: String,
    help_url: Option<String>,
}

fn list_wizard_providers() -> Vec<ProviderInfo> {
    let mut seen = BTreeSet::new();
    let mut items = vec![];

    for provider_id in known_provider_ids().iter() {
        let canonical = canonical_provider_id(provider_id);
        if matches!(canonical.as_str(), "synthetic" | "submodel") {
            continue;
        }
        if !seen.insert(canonical.clone()) {
            continue;
        }

        items.push(ProviderInfo {
            id: canonical.clone(),
            name: provider_display_name(&canonical),
            description: provider_description(&canonical),
            icon: provider_icon_text(&canonical),
            needs_api_key: provider_needs_api_key(&canonical),
            default_base_url: provider_default_base_url(&canonical)
                .unwrap_or("")
                .to_string(),
            env_key: provider_api_key_env(&canonical),
            help_url: provider_api_key_help_url(&canonical).map(ToString::to_string),
        });
    }

    items.sort_by(|a, b| {
        let a_other = a.id == "other";
        let b_other = b.id == "other";
        a_other
            .cmp(&b_other)
            .then_with(|| provider_sort_rank(&a.id).cmp(&provider_sort_rank(&b.id)))
            .then_with(|| a.name.cmp(&b.name))
    });

    items
}

fn wizard_models_list_params(
    selected_provider_id: Option<&str>,
    selected_base_url: &str,
    provider_default_base_url: Option<&str>,
    api_key: &str,
) -> Option<Value> {
    let provider_id = selected_provider_id
        .map(canonical_provider_id)
        .filter(|value| !value.trim().is_empty());
    let base_url = {
        let custom = selected_base_url.trim();
        if !custom.is_empty() {
            Some(custom.to_string())
        } else {
            provider_default_base_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        }
    };
    let api_key = api_key.trim();

    let mut payload = serde_json::Map::new();
    if let Some(provider) = provider_id {
        payload.insert("provider".to_string(), json!(provider));
    }
    if let Some(url) = base_url {
        payload.insert("base_url".to_string(), json!(url));
    }
    if !api_key.is_empty() {
        payload.insert("api_key".to_string(), json!(api_key));
    }

    if payload.is_empty() {
        None
    } else {
        Some(Value::Object(payload))
    }
}

// ── Main component ──────────────────────────────────────────────────

#[component]
pub fn ConnectProvider() -> Element {
    let ws = use_context::<WsRpc>();
    let _ws_connected = use_context::<Signal<bool>>();

    // Wizard step state (1-4)
    let mut current_step = use_signal(|| 1u32);
    let total_steps = 4u32;

    // Step 1: Provider
    let mut selected_provider = use_signal(|| Option::<String>::None);
    let mut provider_search = use_signal(String::new);
    let mut custom_base_url = use_signal(String::new);

    // Step 2: API key
    let mut api_key = use_signal(String::new);
    let mut test_status = use_signal(|| Option::<(bool, String)>::None);
    let mut testing = use_signal(|| false);

    // Step 3: Models selection (enabled set)
    let mut enabled_models = use_signal(std::collections::HashSet::<String>::new);
    let mut fetched_models = use_signal(|| Option::<Vec<ModelEntry>>::None);
    let mut models_loading = use_signal(|| false);
    let mut models_error = use_signal(|| Option::<String>::None);

    // Step 4: Apply
    let mut applying = use_signal(|| false);
    let mut apply_result = use_signal(|| Option::<(bool, String)>::None);

    let providers = list_wizard_providers();

    // Provider helper
    let provider =
        selected_provider().and_then(|id| providers.iter().find(|p| p.id == id).cloned());
    let needs_api_key = provider.as_ref().map(|p| p.needs_api_key).unwrap_or(true);
    let is_other = selected_provider().as_deref() == Some("other");

    // Can advance?
    let can_next = match current_step() {
        1 => selected_provider().is_some() && (!is_other || !custom_base_url().trim().is_empty()),
        2 => {
            !needs_api_key
                || !api_key().trim().is_empty()
                || test_status().map(|(ok, _)| ok).unwrap_or(false)
        }
        3 => !enabled_models().is_empty(),
        4 => true,
        _ => false,
    };
    let providers_for_nav = providers.clone();

    rsx! {
        div { class: "wiz-page",
            // ── Header ──
            div { class: "wiz-header",
                h2 { class: "wiz-header__title", "Connect Provider" }
                button {
                    onclick: move |_| {
                        current_step.set(1);
                        selected_provider.set(None);
                        provider_search.set(String::new());
                        custom_base_url.set(String::new());
                        api_key.set(String::new());
                        test_status.set(None);
                        enabled_models.write().clear();
                        fetched_models.set(None);
                        models_error.set(None);
                        apply_result.set(None);
                    },
                    class: "wiz-btn wiz-btn--ghost",
                    "Start Over"
                }
            }

            // ── Progress indicator ──
            div { class: "wiz-progress",
                for s in 1..=total_steps {
                    {
                        let class_name = if s < current_step() {
                            "wiz-progress__step wiz-progress__step--done"
                        } else if s == current_step() {
                            "wiz-progress__step wiz-progress__step--active"
                        } else {
                            "wiz-progress__step"
                        };
                        rsx! {
                            div { key: "{s}", class: "{class_name}",
                                div { class: "wiz-progress__circle",
                                    if s < current_step() { "V" } else { "{s}" }
                                }
                                div { class: "wiz-progress__label",
                                    match s {
                                        1 => "Provider",
                                        2 => "API Key",
                                        3 => "Models",
                                        4 => "Apply",
                                        _ => "",
                                    }
                                }
                            }
                            if s < total_steps {
                                div { class: if s < current_step() { "wiz-progress__line wiz-progress__line--done" } else { "wiz-progress__line" } }
                            }
                        }
                    }
                }
            }

            // ── Step content ──
            div { class: "wiz-content",
                match current_step() {
                    1 => rsx! {
                        {render_step_provider(
                            selected_provider,
                            custom_base_url,
                            provider_search,
                            providers.clone(),
                        )}
                    },
                    2 => rsx! {
                        {render_step_api_key(
                            &provider,
                            needs_api_key,
                            &custom_base_url(),
                            api_key,
                            test_status,
                            testing,
                            ws.clone(),
                        )}
                    },
                    3 => rsx! {
                        {render_step_model(
                            ws.clone(),
                            enabled_models,
                            fetched_models,
                            models_loading,
                            models_error,
                            selected_provider(),
                            wizard_models_list_params(
                                selected_provider().as_deref(),
                                &custom_base_url(),
                                provider.as_ref().map(|p| p.default_base_url.as_str()),
                                &api_key(),
                            ),
                        )}
                    },
                    4 => rsx! {
                        {render_step_summary(
                            &provider,
                            is_other,
                            &custom_base_url(),
                            &api_key(),
                            &enabled_models(),
                            &fetched_models(),
                            applying,
                            apply_result,
                            ws.clone(),
                        )}
                    },
                    _ => rsx! { div {} },
                }
            }

            // ── Navigation buttons ──
            div { class: "wiz-nav",
                if current_step() > 1 {
                    button {
                        onclick: move |_| {
                            let s = current_step();
                            if s > 1 {
                                current_step.set(s - 1);
                            }
                        },
                        class: "wiz-btn wiz-btn--secondary",
                        "Previous"
                    }
                } else {
                    // Spacer to keep Next on the right
                    div {}
                }

                if current_step() < total_steps {
                    button {
                        onclick: {
                            let ws_fetch = ws.clone();
                            let providers = providers_for_nav.clone();
                            move |_| {
                                let s = current_step();

                                // On advancing from Step 2 to Step 3, auto-fetch models
                                if s == 2 {
                                    models_loading.set(true);
                                    models_error.set(None);
                                    fetched_models.set(None);
                                    enabled_models.write().clear();
                                    let selected_provider_id = selected_provider()
                                        .as_deref()
                                        .map(canonical_provider_id);
                                    let selected_provider_value = selected_provider();
                                    let selected_base_url = custom_base_url();
                                    let api_key_value = api_key();
                                    let provider_default_base_url = selected_provider_value
                                        .as_deref()
                                        .and_then(|id| providers.iter().find(|item| item.id == id))
                                        .map(|item| item.default_base_url.clone());
                                    let request_params = wizard_models_list_params(
                                        selected_provider_value.as_deref(),
                                        &selected_base_url,
                                        provider_default_base_url.as_deref(),
                                        &api_key_value,
                                    );
                                    let ws = ws_fetch.clone();
                                    spawn(async move {
                                        let result = ws
                                            .call::<ModelsResponse>("models.list", request_params)
                                            .await;
                                        models_loading.set(false);
                                        match result {
                                            Ok(resp) => {
                                                let entries =
                                                    build_model_entries(resp, selected_provider_id);
                                                let enabled_ids = initial_enabled_models(&entries);
                                                enabled_models.set(enabled_ids);
                                                fetched_models.set(Some(entries));
                                            }
                                            Err(e) => {
                                                models_error.set(Some(format!("Failed to fetch models: {}", e)));
                                            }
                                        }
                                    });
                                }

                                if s < total_steps {
                                    // Skip API key step for providers that do not require a key.
                                    if s == 1 {
                                        let prov = selected_provider();
                                        let skip_key = prov
                                            .as_deref()
                                            .and_then(|id| providers.iter().find(|item| item.id == id))
                                            .map(|item| !item.needs_api_key)
                                            .unwrap_or(false);
                                        if skip_key {
                                            // Skip Step 2, go directly to Step 3 and fetch models
                                            models_loading.set(true);
                                            models_error.set(None);
                                            fetched_models.set(None);
                                            enabled_models.write().clear();
                                            let selected_provider_id = selected_provider()
                                                .as_deref()
                                                .map(canonical_provider_id);
                                            let selected_provider_value = selected_provider();
                                            let selected_base_url = custom_base_url();
                                            let api_key_value = api_key();
                                            let provider_default_base_url = selected_provider_value
                                                .as_deref()
                                                .and_then(|id| {
                                                    providers
                                                        .iter()
                                                        .find(|item| item.id == id)
                                                })
                                                .map(|item| item.default_base_url.clone());
                                            let request_params = wizard_models_list_params(
                                                selected_provider_value.as_deref(),
                                                &selected_base_url,
                                                provider_default_base_url.as_deref(),
                                                &api_key_value,
                                            );
                                            let ws = ws_fetch.clone();
                                            spawn(async move {
                                                let result = ws
                                                    .call::<ModelsResponse>(
                                                        "models.list",
                                                        request_params,
                                                    )
                                                    .await;
                                                models_loading.set(false);
                                                match result {
                                                    Ok(resp) => {
                                                        let entries =
                                                            build_model_entries(resp, selected_provider_id);
                                                        let enabled_ids =
                                                            initial_enabled_models(&entries);
                                                        enabled_models.set(enabled_ids);
                                                        fetched_models.set(Some(entries));
                                                    }
                                                    Err(e) => {
                                                        models_error.set(Some(format!("Failed to fetch models: {}", e)));
                                                    }
                                                }
                                            });
                                            current_step.set(3);
                                            return;
                                        }
                                    }
                                    current_step.set(s + 1);
                                }
                            }
                        },
                        disabled: !can_next,
                        class: "wiz-btn wiz-btn--primary",
                        "Next"
                    }
                }
            }
        }

        style { {WIZARD_STYLES} }
    }
}

// ── Model entry (local) ─────────────────────────────────────────────

#[derive(Clone, Debug)]
struct ModelEntry {
    id: String,
    name: String,
    provider: Option<String>,
}

fn model_provider_id(provider: Option<&str>, model_id: &str) -> String {
    let from_provider = provider.unwrap_or("").trim();
    if !from_provider.is_empty() {
        return canonical_provider_id(from_provider);
    }
    if let Some((prefix, _)) = model_id.split_once('/') {
        let normalized = canonical_provider_id(prefix);
        if !normalized.is_empty() {
            return normalized;
        }
    }
    "other".to_string()
}

fn build_model_entries(
    response: ModelsResponse,
    selected_provider_id: Option<String>,
) -> Vec<ModelEntry> {
    let mut entries = response
        .models
        .into_iter()
        .map(|model| {
            let provider_id = model_provider_id(model.provider.as_deref(), &model.id);
            ModelEntry {
                id: model.id.clone(),
                name: model.name.unwrap_or_else(|| model.id.clone()),
                provider: Some(provider_id),
            }
        })
        .collect::<Vec<_>>();

    if let Some(selected) = selected_provider_id.as_deref() {
        entries.retain(|item| item.provider.as_deref() == Some(selected));
    }

    entries
}

fn model_visibility_key(model: &ModelEntry) -> ModelKey {
    let provider_id = model_provider_id(model.provider.as_deref(), &model.id);
    let mut model_id = model.id.clone();
    if let Some((prefix, rest)) = model.id.split_once('/') {
        if canonical_provider_id(prefix) == provider_id {
            let rest = rest.trim();
            if !rest.is_empty() {
                model_id = rest.to_string();
            }
        }
    }
    ModelKey::new(provider_id, model_id)
}

fn initial_enabled_models(entries: &[ModelEntry]) -> std::collections::HashSet<String> {
    let prefs = load_model_preferences();
    let mut enabled = entries
        .iter()
        .filter_map(|entry| {
            let key = model_visibility_key(entry);
            if is_model_visible(&prefs, &key) {
                Some(entry.id.clone())
            } else {
                None
            }
        })
        .collect::<std::collections::HashSet<_>>();

    if enabled.is_empty() {
        enabled.extend(entries.iter().map(|entry| entry.id.clone()));
    }
    enabled
}

// ── Step 1: Choose Provider ─────────────────────────────────────────

fn render_step_provider(
    mut selected_provider: Signal<Option<String>>,
    mut custom_base_url: Signal<String>,
    mut provider_search: Signal<String>,
    providers: Vec<ProviderInfo>,
) -> Element {
    let current = selected_provider();
    let query = provider_search().trim().to_ascii_lowercase();
    let filtered = providers
        .into_iter()
        .filter(|provider| {
            if query.is_empty() {
                return true;
            }
            let haystack = format!(
                "{} {} {}",
                provider.id.to_ascii_lowercase(),
                provider.name.to_ascii_lowercase(),
                provider.description.to_ascii_lowercase(),
            );
            haystack.contains(&query)
        })
        .collect::<Vec<_>>();

    rsx! {
        div { class: "wiz-step",
            p { class: "wiz-step__desc",
                "Select the LLM provider you want to use with Savfox. You can always change this later in Settings."
            }
            div { class: "wiz-provider-search",
                div { class: "wiz-field",
                    label { class: "wiz-field__label", "Search providers" }
                    input {
                        r#type: "text",
                        class: "wiz-field__input",
                        placeholder: "Search providers (e.g. zhipu, openrouter, github)",
                        value: "{provider_search}",
                        oninput: move |e| provider_search.set(e.value()),
                    }
                }
            }

            div { class: "wiz-cards",
                for prov in filtered.iter() {
                    {
                        let is_selected = current.as_deref() == Some(prov.id.as_str());
                        let prov_id = prov.id.to_string();
                        let default_base_url = prov.default_base_url.to_string();
                        rsx! {
                            button {
                                key: "{prov.id}",
                                class: if is_selected { "wiz-card wiz-card--selected" } else { "wiz-card" },
                                onclick: move |_| {
                                    selected_provider.set(Some(prov_id.clone()));
                                    if prov_id == "other" {
                                        // Keep user's custom URL when switching to "other".
                                    } else if !default_base_url.is_empty() {
                                        custom_base_url.set(default_base_url.clone());
                                    } else {
                                        // Avoid stale URLs from previously selected providers.
                                        custom_base_url.set(String::new());
                                    }
                                },
                                div { class: "wiz-card__icon", "{prov.icon}" }
                                div { class: "wiz-card__body",
                                    div { class: "wiz-card__name", "{prov.name}" }
                                    div { class: "wiz-card__desc", "{prov.description}" }
                                }
                                if is_selected {
                                    div { class: "wiz-card__check", "V" }
                                }
                            }
                        }
                    }
                }
            }

            if filtered.is_empty() {
                div { class: "wiz-callout wiz-callout--warn",
                    p { class: "wiz-callout__text", "No providers match your search." }
                }
            }

            // Custom base URL for "Other"
            if current.as_deref() == Some("other") {
                div { class: "wiz-field",
                    label { class: "wiz-field__label", "Base URL *" }
                    input {
                        r#type: "text",
                        class: "wiz-field__input",
                        placeholder: "https://api.example.com/v1",
                        value: "{custom_base_url}",
                        oninput: move |e| custom_base_url.set(e.value()),
                    }
                    p { class: "wiz-field__hint", "Enter the base URL of your OpenAI-compatible API endpoint." }
                }
            }
        }
    }
}

// ── Step 2: API Key ─────────────────────────────────────────────────
fn render_step_api_key(
    provider: &Option<ProviderInfo>,
    needs_api_key: bool,
    selected_base_url: &str,
    mut api_key: Signal<String>,
    mut test_status: Signal<Option<(bool, String)>>,
    mut testing: Signal<bool>,
    ws: WsRpc,
) -> Element {
    let provider_id = provider.as_ref().map(|p| p.id.as_str()).unwrap_or_default();
    let provider_name = provider
        .as_ref()
        .map(|p| p.name.as_str())
        .unwrap_or("Provider");
    let provider_help_url = provider.as_ref().and_then(|p| p.help_url.as_deref());
    let is_openai = provider_id == "openai";

    if is_openai {
        return rsx! {
            div { class: "wiz-openai-login",
                crate::components::openai_login::OpenAILogin {
                    ws: ws.clone(),
                    compact: true,
                    on_success: move |_| {
                        test_status.set(Some((true, "OpenAI account connected".to_string())));
                    }
                }
            }
        };
    }

    if !needs_api_key {
        return rsx! {
            div { class: "wiz-callout wiz-callout--info",
                p { class: "wiz-callout__text",
                    "This provider does not require an API key. You can proceed to the next step."
                }
            }
        };
    }

    rsx! {
        p { class: "wiz-step__desc",
            "Enter your {provider_name} API key. This will be stored securely in your local configuration."
        }


        div { class: "wiz-field",
            label { class: "wiz-field__label", "{provider_name} API Key" }
            input {
                r#type: "password",
                class: "wiz-field__input",
                placeholder: "sk-... or your API key",
                value: "{api_key}",
                oninput: move |e| {
                    api_key.set(e.value());
                    test_status.set(None);
                },
            }
        }

        // Test connection button
        div { class: "wiz-test-row",
            button {
                onclick: {
                    let ws = ws.clone();
                    let provider_id = provider
                        .as_ref()
                        .map(|p| p.id.clone())
                        .unwrap_or_default();
                    let provider_default_base_url = provider
                        .as_ref()
                        .map(|p| p.default_base_url.clone())
                        .unwrap_or_default();
                    let selected_base_url = selected_base_url.trim().to_string();
                    move |_| {
                        if api_key().trim().is_empty() {
                            test_status.set(Some((false, "Please enter an API key first.".to_string())));
                            return;
                        }
                        testing.set(true);
                        test_status.set(None);
                        let ws = ws.clone();
                        let provider_id = provider_id.clone();
                        let provider_default_base_url = provider_default_base_url.clone();
                        let selected_base_url = selected_base_url.clone();
                        let api_key_value = api_key();
                        spawn(async move {
                            let base_url = if selected_base_url.is_empty() {
                                provider_default_base_url
                            } else {
                                selected_base_url
                            };
                            let result = ws.call::<serde_json::Value>(
                                "models.test",
                                Some(json!({
                                    "provider": provider_id,
                                    "base_url": base_url,
                                    "api_key": api_key_value,
                                })),
                            ).await;
                            testing.set(false);
                            match result {
                                Ok(payload) => {
                                    let ok = payload
                                        .get("ok")
                                        .and_then(|value| value.as_bool())
                                        .unwrap_or(true);
                                    let msg = payload
                                        .get("message")
                                        .and_then(|value| value.as_str())
                                        .unwrap_or(if ok {
                                            "Connection successful"
                                        } else {
                                            "Connection failed"
                                        })
                                        .to_string();
                                    test_status.set(Some((ok, msg)));
                                }
                                Err(e) => {
                                    test_status.set(Some((false, format!("Connection failed: {}", e))));
                                }
                            }
                        });
                    }
                },
                disabled: testing() || api_key().trim().is_empty(),
                class: "wiz-btn wiz-btn--accent",
                if testing() { "Testing..." } else { "Test Connection" }
            }

            if let Some((success, ref msg)) = test_status() {
                div {
                    class: if success { "wiz-test-result wiz-test-result--ok" } else { "wiz-test-result wiz-test-result--err" },
                    "{msg}"
                }
            }
        }

        // Help links
        div { class: "wiz-help",
            if let Some(help_url) = provider_help_url {
                p { class: "wiz-help__text", "Get your API key from {help_url}" }
            }
        }
    }
}

// ── Step 3: Models ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_step_model(
    ws: WsRpc,
    mut enabled_models: Signal<std::collections::HashSet<String>>,
    mut fetched_models: Signal<Option<Vec<ModelEntry>>>,
    mut models_loading: Signal<bool>,
    mut models_error: Signal<Option<String>>,
    selected_provider_id: Option<String>,
    models_list_params: Option<Value>,
) -> Element {
    let enabled_snapshot = enabled_models();
    let models = fetched_models();

    rsx! {
        div { class: "wiz-step",
            p { class: "wiz-step__desc",
                "Enable the models you want available in model selector."
            }

            if models_loading() {
                div { class: "wiz-loading",
                    div { class: "wiz-loading__spinner" }
                    span { class: "wiz-loading__text", "Fetching available models..." }
                }
            }

            if let Some(ref err) = models_error() {
                div { class: "wiz-callout wiz-callout--error",
                    p { class: "wiz-callout__text", "{err}" }
                    button {
                        onclick: {
                            let ws = ws.clone();
                            move |_| {
                                models_loading.set(true);
                                models_error.set(None);
                                fetched_models.set(None);
                                let ws = ws.clone();
                                let selected_provider_id = selected_provider_id
                                    .as_deref()
                                    .map(canonical_provider_id);
                                let request_params = models_list_params.clone();
                                spawn(async move {
                                    let result = ws
                                        .call::<ModelsResponse>("models.list", request_params)
                                        .await;
                                    models_loading.set(false);
                                    match result {
                                        Ok(resp) => {
                                            let entries = build_model_entries(resp, selected_provider_id);
                                            let enabled_ids = initial_enabled_models(&entries);
                                            enabled_models.set(enabled_ids);
                                            fetched_models.set(Some(entries));
                                        }
                                        Err(e) => {
                                            models_error.set(Some(format!("Failed to fetch models: {}", e)));
                                        }
                                    }
                                });
                            }
                        },
                        class: "wiz-btn wiz-btn--accent",
                        "Retry"
                    }
                }
            }

            if let Some(ref model_list) = models {
                if model_list.is_empty() {
                    div { class: "wiz-callout wiz-callout--warn",
                        p { class: "wiz-callout__text",
                            "No models found. This may indicate the provider is unreachable or the API key is incorrect."
                        }
                    }
                } else {
                    div { class: "wiz-model-count",
                        "{enabled_snapshot.len()} enabled / {model_list.len()} total"
                    }
                    div { class: "wiz-model-list",
                        for model in model_list.iter() {
                            {
                                let is_enabled = enabled_snapshot.contains(model.id.as_str());
                                let model_id = model.id.clone();
                                let prov_display = model.provider.clone().unwrap_or_default();
                                let model_entry = model.clone();
                                let row_class = if is_enabled {
                                    "wiz-model-card"
                                } else {
                                    "wiz-model-card wiz-model-card--off"
                                };
                                let toggle_class = if is_enabled {
                                    "wiz-model-toggle wiz-model-toggle--on"
                                } else {
                                    "wiz-model-toggle"
                                };
                                rsx! {
                                    div {
                                        key: "{model.id}",
                                        class: "{row_class}",
                                        div { class: "wiz-model-card__info",
                                            div { class: "wiz-model-card__name", "{model.name}" }
                                            div { class: "wiz-model-card__meta",
                                                span { class: "wiz-model-card__id", "{model.id}" }
                                                if !prov_display.is_empty() {
                                                    span { class: "wiz-model-card__prov", "{prov_display}" }
                                                }
                                            }
                                        }
                                        button {
                                            class: "{toggle_class}",
                                            title: if is_enabled { "Disable model" } else { "Enable model" },
                                            aria_label: if is_enabled { "Disable model" } else { "Enable model" },
                                            onclick: move |_| {
                                                let next_enabled = {
                                                    let mut set = enabled_models.write();
                                                    if set.contains(model_id.as_str()) {
                                                        set.remove(model_id.as_str());
                                                        false
                                                    } else {
                                                        set.insert(model_id.clone());
                                                        true
                                                    }
                                                };

                                                let key = model_visibility_key(&model_entry);
                                                let mut prefs = load_model_preferences();
                                                set_model_visibility(&mut prefs, &key, next_enabled);
                                                save_model_preferences(&prefs);
                                            },
                                            span { class: "wiz-model-toggle__thumb" }
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

// ── Step 4: Summary & Apply ─────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_step_summary(
    provider: &Option<ProviderInfo>,
    is_other: bool,
    custom_base_url: &str,
    api_key: &str,
    enabled_model_ids: &std::collections::HashSet<String>,
    fetched_models: &Option<Vec<ModelEntry>>,
    mut applying: Signal<bool>,
    mut apply_result: Signal<Option<(bool, String)>>,
    ws: WsRpc,
) -> Element {
    let provider_name = provider
        .as_ref()
        .map(|p| p.name.as_str())
        .unwrap_or("Unknown");
    let provider_id = provider
        .as_ref()
        .map(|p| p.id.as_str())
        .unwrap_or("unknown");
    let provider_env_key = provider
        .as_ref()
        .map(|p| p.env_key.clone())
        .unwrap_or_else(|| provider_api_key_env(provider_id));
    let enabled_models = fetched_models
        .as_ref()
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| enabled_model_ids.contains(entry.id.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let model_display = enabled_models
        .first()
        .map(|entry| entry.name.clone())
        .unwrap_or_else(|| "(none)".to_string());
    let has_api_key = !api_key.is_empty();

    let provider_id_owned = provider_id.to_string();
    let provider_name_owned = provider_name.to_string();
    let provider_env_key_owned = provider_env_key.clone();
    let api_key_owned = api_key.to_string();
    let base_url_owned = custom_base_url.to_string();
    let enabled_models_owned = enabled_models.clone();

    rsx! {
        div { class: "wiz-step",
            p { class: "wiz-step__desc",
                "Review your setup choices before applying."
            }

            div { class: "wiz-summary",
                // Provider row
                div { class: "wiz-summary__row",
                    div { class: "wiz-summary__label", "Provider" }
                    div { class: "wiz-summary__value",
                        span { class: "wiz-summary__badge", "{provider_name}" }
                        if is_other && !custom_base_url.trim().is_empty() {
                            span { class: "wiz-summary__detail", "{custom_base_url}" }
                        }
                    }
                }

                // API key row
                div { class: "wiz-summary__row",
                    div { class: "wiz-summary__label", "API Key" }
                    div { class: "wiz-summary__value",
                        if has_api_key {
                            span { class: "wiz-summary__badge wiz-summary__badge--ok", "Configured" }
                        } else {
                            span { class: "wiz-summary__badge wiz-summary__badge--muted", "Not set" }
                        }
                    }
                }

                // Model row
                div { class: "wiz-summary__row",
                    div { class: "wiz-summary__label", "Models" }
                    div { class: "wiz-summary__value",
                        if enabled_models.is_empty() {
                            span { class: "wiz-summary__badge wiz-summary__badge--muted", "None" }
                        } else {
                            span { class: "wiz-summary__badge", "{enabled_models.len()} enabled" }
                            span { class: "wiz-summary__mono", "Default: {model_display}" }
                        }
                    }
                }

            }

            // Apply button
            div { class: "wiz-apply-row",
                if let Some((success, ref msg)) = apply_result() {
                    div {
                        class: if success { "wiz-callout wiz-callout--success" } else { "wiz-callout wiz-callout--error" },
                        p { class: "wiz-callout__text", "{msg}" }
                    }
                }

                button {
                    onclick: {
                        let ws = ws.clone();
                        move |_| {
                            applying.set(true);
                            apply_result.set(None);
                            let ws = ws.clone();
                            let provider_id = provider_id_owned.clone();
                            let provider_name = provider_name_owned.clone();
                            let provider_env_key = provider_env_key_owned.clone();
                            let api_key = api_key_owned.clone();
                            let base_url = base_url_owned.clone();
                            let models = enabled_models_owned.clone();
                            spawn(async move {
                                // Build model entries for import
                                let model_entries: Vec<Value> = models
                                    .iter()
                                    .enumerate()
                                    .map(|(i, entry)| {
                                        let model_code = entry.id.split('/').last().unwrap_or(&entry.id);
                                        json!({
                                            "id": entry.id,
                                            "model_code": model_code,
                                            "name": entry.name,
                                            "provider": provider_id,
                                            "is_default": i == 0,
                                        })
                                    })
                                    .collect();

                                let mut import_params = json!({
                                    "provider_id": provider_id,
                                    "display_name": provider_name,
                                    "models": model_entries,
                                    "api_key": api_key,
                                    "env_key": provider_env_key,
                                });
                                if !base_url.is_empty() {
                                    import_params["base_url"] = json!(base_url);
                                }

                                let result = ws.call::<serde_json::Value>(
                                    "models.import",
                                    Some(import_params),
                                ).await;

                                applying.set(false);
                                match result {
                                    Ok(_) => {
                                        apply_result.set(Some((true, "Configuration applied successfully! You can now start using Savfox.".to_string())));
                                    }
                                    Err(e) => {
                                        apply_result.set(Some((false, format!("Failed to apply configuration: {}", e))));
                                    }
                                }
                            });
                        }
                    },
                    disabled: applying(),
                    class: "wiz-btn wiz-btn--primary wiz-btn--lg",
                    if applying() { "Applying Configuration..." } else { "Apply Configuration" }
                }
            }
        }
    }
}

// ── Styles ──────────────────────────────────────────────────────────

const WIZARD_STYLES: &str = r#"
    /* ---- Page layout ---- */
    .wiz-page {
        height: 100%;
        min-height: 0;
        overflow: hidden;
        padding: 20px 28px;
        max-width: 1320px;
        margin: 0 auto;
        display: flex;
        flex-direction: column;
        gap: 16px;
    }

    /* ---- Header ---- */
    .wiz-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
    }

    .wiz-header__title {
        font-size: 22px;
        font-weight: 700;
        margin: 0;
    }

    /* ---- Progress indicator ---- */
    .wiz-progress {
        display: flex;
        align-items: center;
        justify-content: center;
        gap: 0;
        padding: 8px 0;
    }

    .wiz-progress__step {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 6px;
        min-width: 70px;
    }

    .wiz-progress__circle {
        width: 36px;
        height: 36px;
        border-radius: 50%;
        border: 2px solid var(--border);
        display: flex;
        align-items: center;
        justify-content: center;
        font-size: 14px;
        font-weight: 600;
        color: var(--text-muted);
        background: var(--bg-secondary);
        transition: all 0.25s ease;
    }

    .wiz-progress__step--active .wiz-progress__circle {
        border-color: var(--accent);
        color: #fff;
        background: var(--accent);
    }

    .wiz-progress__step--done .wiz-progress__circle {
        border-color: var(--success);
        color: #fff;
        background: var(--success);
    }

    .wiz-progress__label {
        font-size: 11px;
        color: var(--text-muted);
        text-align: center;
        white-space: nowrap;
    }

    .wiz-progress__step--active .wiz-progress__label {
        color: var(--accent);
        font-weight: 600;
    }

    .wiz-progress__step--done .wiz-progress__label {
        color: var(--success);
    }

    .wiz-progress__line {
        flex: 1;
        height: 2px;
        min-width: 32px;
        max-width: 64px;
        background: var(--border);
        margin: 0 4px;
        margin-bottom: 22px;
        transition: background 0.25s ease;
    }

    .wiz-progress__line--done {
        background: var(--success);
    }

    /* ---- Step content ---- */
    .wiz-content {
        flex: 1;
        min-height: 0;
        overflow-y: auto;
        overflow-x: hidden;
        padding-bottom: 8px;
    }

    .wiz-step {
        display: flex;
        flex-direction: column;
        gap: 16px;
    }

    .wiz-step__desc {
        font-size: 14px;
        color: var(--text-secondary);
        line-height: 1.6;
        margin: 0;
    }

    .wiz-provider-search {
        position: sticky;
        top: 0;
        z-index: 6;
        background: var(--bg-primary);
        padding-bottom: 8px;
    }

    /* ---- Provider cards ---- */
    .wiz-cards {
        display: grid;
        grid-template-columns: 1fr;
        gap: 12px;
    }

    @media screen and (min-width: 640px) {
        .wiz-cards {
            grid-template-columns: repeat(2, minmax(0, 1fr));
        }
    }

    @media screen and (min-width: 900px) {
        .wiz-cards {
            grid-template-columns: repeat(3, minmax(0, 1fr));
        }
    }

    @media screen and (min-width: 1160px) {
        .wiz-cards {
            grid-template-columns: repeat(4, minmax(0, 1fr));
        }
    }

    @media screen and (min-width: 1460px) {
        .wiz-cards {
            grid-template-columns: repeat(5, minmax(0, 1fr));
        }
    }

    @media screen and (min-width: 1760px) {
        .wiz-cards {
            grid-template-columns: repeat(6, minmax(0, 1fr));
        }
    }

    @media screen and (min-width: 2060px) {
        .wiz-cards {
            grid-template-columns: repeat(7, minmax(0, 1fr));
        }
    }

    @media screen and (min-width: 2360px) {
        .wiz-cards {
            grid-template-columns: repeat(8, minmax(0, 1fr));
        }
    }

    @media screen and (min-width: 2660px) {
        .wiz-cards {
            grid-template-columns: repeat(9, minmax(0, 1fr));
        }
    }

    @media screen and (min-width: 2960px) {
        .wiz-cards {
            grid-template-columns: repeat(10, minmax(0, 1fr));
        }
    }

    .wiz-card {
        display: flex;
        align-items: center;
        gap: 14px;
        padding: 12px 14px;
        background: var(--bg-secondary);
        border: 2px solid var(--border);
        border-radius: var(--radius);
        cursor: pointer;
        text-align: left;
        transition: border-color 0.15s, background 0.15s, box-shadow 0.15s;
        position: relative;
    }

    .wiz-card:hover {
        border-color: var(--accent);
        background: var(--bg-hover, var(--bg-secondary));
    }

    .wiz-card--selected {
        border-color: var(--accent);
        background: var(--bg-hover, var(--bg-secondary));
        box-shadow: 0 0 0 1px var(--accent);
    }

    .wiz-card__icon {
        width: 40px;
        height: 40px;
        display: flex;
        align-items: center;
        justify-content: center;
        background: var(--bg-tertiary);
        border-radius: var(--radius);
        font-weight: 700;
        font-size: 14px;
        color: var(--text-secondary);
        flex-shrink: 0;
    }

    .wiz-card__body {
        flex: 1;
        min-width: 0;
    }

    .wiz-card__name {
        font-size: 15px;
        font-weight: 600;
        color: var(--text-primary);
    }

    .wiz-card__desc {
        font-size: 12px;
        color: var(--text-muted);
        margin-top: 2px;
    }

    .wiz-card__check {
        position: absolute;
        top: 8px;
        right: 10px;
        width: 22px;
        height: 22px;
        border-radius: 50%;
        background: var(--accent);
        color: #fff;
        display: flex;
        align-items: center;
        justify-content: center;
        font-size: 12px;
        font-weight: 700;
    }

    /* ---- Form fields ---- */
    .wiz-field {
        display: flex;
        flex-direction: column;
        gap: 6px;
    }

    .wiz-field__label {
        font-size: 13px;
        font-weight: 500;
        color: var(--text-secondary);
    }

    .wiz-field__input {
        width: 100%;
        padding: 10px 14px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        color: var(--text-primary);
        font-size: 14px;
        outline: none;
        box-sizing: border-box;
        transition: border-color 0.15s;
    }

    .wiz-field__input:focus {
        border-color: var(--accent);
    }

    .wiz-field__hint {
        font-size: 12px;
        color: var(--text-muted);
        margin: 0;
    }

    /* ---- Test connection ---- */
    .wiz-test-row {
        display: flex;
        align-items: center;
        gap: 16px;
        flex-wrap: wrap;
    }

    .wiz-test-result {
        font-size: 13px;
        padding: 6px 14px;
        border-radius: var(--radius);
    }

    .wiz-test-result--ok {
        background: rgba(34,197,94,0.1);
        color: var(--success);
        border: 1px solid var(--success);
    }

    .wiz-test-result--err {
        background: rgba(239,68,68,0.1);
        color: var(--danger);
        border: 1px solid var(--danger);
    }

    /* ---- Help text ---- */
    .wiz-help {
        padding-top: 4px;
    }

    .wiz-help__text {
        font-size: 13px;
        color: var(--text-muted);
        margin: 0;
    }

    /* ---- Model list ---- */
    .wiz-model-count {
        font-size: 13px;
        color: var(--text-muted);
    }

    .wiz-model-list {
        display: flex;
        flex-direction: column;
        gap: 8px;
        max-height: 360px;
        overflow-y: auto;
    }

    .wiz-model-card {
        display: flex;
        align-items: center;
        justify-content: space-between;
        padding: 14px 16px;
        background: var(--bg-secondary);
        border: 2px solid var(--border);
        border-radius: var(--radius);
        cursor: default;
        text-align: left;
        transition: border-color 0.15s, background 0.15s;
        position: relative;
    }

    .wiz-model-card:hover {
        border-color: var(--accent);
    }

    .wiz-model-card--selected {
        border-color: var(--accent);
        box-shadow: 0 0 0 1px var(--accent);
    }

    .wiz-model-card__info {
        display: flex;
        flex-direction: column;
        gap: 4px;
        min-width: 0;
    }

    .wiz-model-card__name {
        font-size: 15px;
        font-weight: 600;
        color: var(--text-primary);
    }

    .wiz-model-card--off .wiz-model-card__name {
        opacity: 0.56;
    }

    .wiz-model-card__meta {
        display: flex;
        gap: 12px;
        font-size: 12px;
        color: var(--text-muted);
    }

    .wiz-model-card__id {
        font-family: monospace;
    }

    .wiz-model-card__prov {
        padding: 1px 8px;
        background: var(--bg-tertiary);
        border-radius: 4px;
    }

    .wiz-model-toggle {
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

    .wiz-model-toggle__thumb {
        width: 14px;
        height: 14px;
        border-radius: 50%;
        background: #f2f2f2;
        transform: translateX(0);
        transition: transform 0.15s ease;
    }

    .wiz-model-toggle--on {
        background: color-mix(in srgb, var(--text-primary) 16%, var(--bg-hover) 84%);
        border-color: color-mix(in srgb, var(--text-primary) 38%, var(--border) 62%);
    }

    .wiz-model-toggle--on .wiz-model-toggle__thumb {
        transform: translateX(14px);
        background: #fff;
    }

    /* ---- Loading ---- */
    .wiz-loading {
        display: flex;
        align-items: center;
        gap: 12px;
        padding: 32px 0;
        justify-content: center;
    }

    .wiz-loading__spinner {
        width: 24px;
        height: 24px;
        border: 3px solid var(--border);
        border-top-color: var(--accent);
        border-radius: 50%;
        animation: wiz-spin 0.8s linear infinite;
    }

    @keyframes wiz-spin {
        to { transform: rotate(360deg); }
    }

    .wiz-loading__text {
        font-size: 14px;
        color: var(--text-secondary);
    }

    /* ---- Channel grid ---- */
    .wiz-channel-grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
        gap: 12px;
    }

    .wiz-channel {
        background: var(--bg-secondary);
        border: 2px solid var(--border);
        border-radius: var(--radius);
        overflow: hidden;
        transition: border-color 0.15s;
    }

    .wiz-channel--enabled {
        border-color: var(--accent);
    }

    .wiz-channel__header {
        display: flex;
        align-items: center;
    }

    .wiz-channel__toggle {
        display: flex;
        align-items: center;
        gap: 12px;
        padding: 14px 16px;
        width: 100%;
        background: transparent;
        border: none;
        cursor: pointer;
        text-align: left;
        color: var(--text-primary);
    }

    .wiz-channel__toggle:hover {
        background: var(--bg-hover, var(--bg-tertiary));
    }

    .wiz-channel__icon {
        width: 32px;
        height: 32px;
        display: flex;
        align-items: center;
        justify-content: center;
        background: var(--bg-tertiary);
        border-radius: var(--radius);
        font-weight: 700;
        font-size: 13px;
        color: var(--text-secondary);
        flex-shrink: 0;
    }

    .wiz-channel__name {
        flex: 1;
        font-size: 14px;
        font-weight: 600;
    }

    .wiz-channel__check {
        width: 22px;
        height: 22px;
        border-radius: 4px;
        border: 2px solid var(--border);
        display: flex;
        align-items: center;
        justify-content: center;
        font-size: 12px;
        font-weight: 700;
        flex-shrink: 0;
        transition: all 0.15s;
    }

    .wiz-channel__check--on {
        background: var(--accent);
        border-color: var(--accent);
        color: #fff;
    }

    .wiz-channel__config {
        padding: 0 16px 14px 16px;
        display: flex;
        flex-direction: column;
        gap: 6px;
    }

    /* ---- Summary ---- */
    .wiz-summary {
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        overflow: hidden;
    }

    .wiz-summary__row {
        display: flex;
        align-items: center;
        padding: 14px 20px;
        border-bottom: 1px solid var(--border);
    }

    .wiz-summary__row:last-child {
        border-bottom: none;
    }

    .wiz-summary__label {
        width: 140px;
        flex-shrink: 0;
        font-size: 13px;
        font-weight: 500;
        color: var(--text-secondary);
    }

    .wiz-summary__value {
        flex: 1;
        display: flex;
        align-items: center;
        gap: 8px;
        flex-wrap: wrap;
    }

    .wiz-summary__badge {
        font-size: 13px;
        padding: 3px 10px;
        background: var(--accent);
        color: #fff;
        border-radius: 4px;
        font-weight: 500;
    }

    .wiz-summary__badge--ok {
        background: var(--success);
    }

    .wiz-summary__badge--muted {
        background: var(--bg-tertiary);
        color: var(--text-muted);
    }

    .wiz-summary__mono {
        font-family: monospace;
        font-size: 14px;
        color: var(--text-primary);
    }

    .wiz-summary__detail {
        font-size: 12px;
        color: var(--text-muted);
        font-family: monospace;
    }

    /* ---- Apply row ---- */
    .wiz-apply-row {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 16px;
        padding-top: 8px;
    }

    /* ---- Callouts ---- */
    .wiz-callout {
        padding: 14px 18px;
        border-radius: var(--radius);
        display: flex;
        align-items: center;
        gap: 12px;
        flex-wrap: wrap;
    }

    .wiz-callout--info {
        background: rgba(99,102,241,0.08);
        border: 1px solid var(--accent);
    }

    .wiz-callout--success {
        background: rgba(34,197,94,0.1);
        border: 1px solid var(--success);
    }

    .wiz-callout--error {
        background: rgba(239,68,68,0.1);
        border: 1px solid var(--danger);
    }

    .wiz-callout--warn {
        background: rgba(234,179,8,0.1);
        border: 1px solid var(--warning, #eab308);
    }

    .wiz-callout__text {
        font-size: 14px;
        line-height: 1.5;
        margin: 0;
        flex: 1;
    }

    .wiz-callout--info .wiz-callout__text { color: var(--accent); }
    .wiz-callout--success .wiz-callout__text { color: var(--success); }
    .wiz-callout--error .wiz-callout__text { color: var(--danger); }
    .wiz-callout--warn .wiz-callout__text { color: var(--warning, #eab308); }

    /* ---- Navigation ---- */
    .wiz-nav {
        display: flex;
        justify-content: space-between;
        align-items: center;
        gap: 10px;
        flex-shrink: 0;
        padding-top: 12px;
        padding-bottom: 2px;
        border-top: 1px solid var(--border);
    }

    /* ---- Buttons ---- */
    .wiz-btn {
        padding: 10px 24px;
        border: none;
        border-radius: var(--radius);
        font-size: 14px;
        font-weight: 500;
        cursor: pointer;
        transition: opacity 0.15s, background 0.15s;
    }

    .wiz-btn:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    .wiz-btn--primary {
        background: var(--accent);
        color: #fff;
    }

    .wiz-btn--primary:hover:not(:disabled) {
        opacity: 0.9;
    }

    .wiz-btn--secondary {
        background: var(--bg-tertiary);
        color: var(--text-secondary);
        border: 1px solid var(--border);
    }

    .wiz-btn--secondary:hover:not(:disabled) {
        background: var(--bg-hover, var(--bg-tertiary));
        color: var(--text-primary);
    }

    .wiz-btn--accent {
        background: var(--accent);
        color: #fff;
    }

    .wiz-btn--accent:hover:not(:disabled) {
        opacity: 0.9;
    }

    .wiz-btn--ghost {
        background: transparent;
        color: var(--text-muted);
        border: 1px solid var(--border);
    }

    .wiz-btn--ghost:hover {
        color: var(--text-secondary);
        border-color: var(--text-muted);
    }

    .wiz-btn--lg {
        padding: 14px 48px;
        font-size: 16px;
    }

    /* ---- Responsive ---- */
    @media screen and (max-width: 640px) {
        .wiz-page {
            padding: 14px 12px;
            gap: 12px;
        }

        .wiz-cards {
            grid-template-columns: 1fr;
        }

        .wiz-channel-grid {
            grid-template-columns: 1fr;
        }

        .wiz-progress__step {
            min-width: 50px;
        }

        .wiz-progress__label {
            font-size: 9px;
        }

        .wiz-summary__row {
            flex-direction: column;
            align-items: flex-start;
            gap: 6px;
        }

        .wiz-summary__label {
            width: auto;
        }

        .wiz-nav {
            padding-bottom: 8px;
        }
    }
"#;
