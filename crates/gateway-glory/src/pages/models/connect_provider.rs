//! Authenticate, choose models, then start a conversation.
use std::collections::BTreeSet;

use glory::prelude::*;
use serde_json::{Value, json};

use super::super::common::*;
use crate::ui::{self, AppContext};
use crate::utils::provider_registry::{
    canonical_provider_id, known_provider_ids, provider_api_key_env, provider_default_base_url,
    provider_display_name,
};

#[derive(Clone, Copy)]
struct Connection {
    provider: Cage<String>,
    method: Cage<String>,
    account: Cage<String>,
    base: Cage<String>,
    key: Cage<String>,
    signed_in: Cage<bool>,
    models: Cage<Vec<Value>>,
    enabled: Cage<BTreeSet<String>>,
    default_model: Cage<String>,
    manual: Cage<bool>,
    manual_model: Cage<String>,
    result: Cage<String>,
    action: Cage<String>,
    error: Cage<String>,
    generation: Cage<u64>,
}
impl Connection {
    fn credentials(self) -> Value {
        let provider = canonical_provider_id(&self.provider.get_untracked());
        let account_auth = provider == "openai" && self.method.get_untracked() != "api_key";
        let base_url = if account_auth || self.base.get_untracked().trim().is_empty() {
            provider_default_base_url(&provider)
                .unwrap_or("")
                .to_owned()
        } else {
            self.base.get_untracked().trim().to_owned()
        };
        let api_key = if account_auth {
            String::new()
        } else {
            self.key.get_untracked().trim().to_owned()
        };
        json!({"provider": provider, "base_url": base_url, "api_key": api_key})
    }
    fn selected(self) -> Vec<Value> {
        let enabled = self.enabled.get_untracked();
        let manual_mode = self.manual.get_untracked();
        let mut entries = if manual_mode {
            Vec::new()
        } else {
            self.models
                .get_untracked()
                .into_iter()
                .filter(|entry| enabled.contains(&string(entry, &["id"])))
                .collect::<Vec<_>>()
        };
        let manual = self.manual_model.get_untracked().trim().to_owned();
        if manual_mode && !manual.is_empty() {
            let provider = canonical_provider_id(&self.provider.get_untracked());
            let slug = manual
                .strip_prefix(&format!("{provider}/"))
                .unwrap_or(&manual);
            let id = format!("{provider}/{slug}");
            if !entries.iter().any(|entry| entry["id"] == id) {
                entries.push(json!({"id":id,"name":slug,"model_slug":slug}));
            }
        }
        entries
    }
}

pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let scope = &ctx.scope;
    let state = Connection {
        provider: scope.cage("openai".to_owned()),
        method: scope.cage("api_key".to_owned()),
        account: scope.cage(String::new()),
        base: scope.cage(String::new()),
        key: scope.cage(String::new()),
        signed_in: scope.cage(false),
        models: scope.cage(Vec::new()),
        enabled: scope.cage(BTreeSet::new()),
        default_model: scope.cage(String::new()),
        manual: scope.cage(false),
        manual_model: scope.cage(String::new()),
        result: scope.cage(String::new()),
        action: scope.cage(String::new()),
        error: scope.cage(String::new()),
        generation: scope.cage(0),
    };
    // Credential edits invalidate both discovered models and any in-flight reply.
    scope.effect(move || {
        let _ = (
            state.provider.get(),
            state.method.get(),
            state.base.get(),
            state.key.get(),
        );
        let _ = state
            .generation
            .try_revise(|value| *value = value.wrapping_add(1));
        let _ = state.models.try_revise(Vec::clear);
        let _ = state.enabled.try_revise(BTreeSet::clear);
        let _ = state.default_model.try_revise(String::clear);
        let _ = state.result.try_revise(String::clear);
        let _ = state.error.try_revise(String::clear);
        let _ = state.action.try_revise(String::clear);
        let _ = state.signed_in.try_revise(|value| *value = false);
    });
    scope.effect(move || {
        let _ = state.provider.get();
        let _ = state.manual_model.try_revise(String::clear);
        let _ = state.manual.try_revise(|manual| *manual = false);
    });
    scope.effect(move || {
        let _ = (
            state.models.get(),
            state.enabled.get(),
            state.manual.get(),
            state.manual_model.get(),
        );
        let entries = state.selected();
        if !entries
            .iter()
            .any(|entry| entry["id"] == state.default_model.get_untracked())
        {
            let next = entries
                .first()
                .map(|entry| string(entry, &["id"]))
                .unwrap_or_default();
            let _ = state.default_model.try_revise(|value| *value = next);
        }
    });
    let mut ids = BTreeSet::new();
    let options = known_provider_ids()
        .iter()
        .map(|id| canonical_provider_id(id))
        .filter(|id| !matches!(id.as_str(), "synthetic" | "submodel"))
        .filter(|id| ids.insert(id.clone()))
        .map(|id| {
            let label = provider_display_name(&id);
            (id, label)
        })
        .collect();
    let login_context = ctx.clone();
    let authentication = each(scope.bond(move || vec![(state.provider.get(), state.method.get())]))
        .key(|(provider, method)| format!("{provider}/{method}"))
        .children(move |entry| {
            let (provider, method) = entry.value.get_untracked();
            let context = login_context.clone();
            widget(move |scope| {
                let context = context.with_scope(scope);
                let mut controls = Vec::<Box<dyn Widget>>::new();
                if provider == "openai" {
                    controls.push(select_for(locale, "Authentication method", state.method, vec![
                        ("api_key".into(), ui::translate(locale.get_untracked(), "API key")),
                        ("chatgpt".into(), ui::translate(locale.get_untracked(), "ChatGPT account")),
                        ("deviceCode".into(), ui::translate(locale.get_untracked(), "Device code")),
                    ]));
                }
                if provider == "openai" && method != "api_key" {
                    controls.push(super::login::render(&context, if method == "deviceCode" {"deviceCode"} else {"chatgpt"}, state.signed_in));
                } else {
                    controls.push(secret(ui::label_for(locale, "API key"), state.key));
                    controls.push(paragraph(ui::label_for(locale, "Leave the key empty to use a credential configured on the gateway, or an endpoint that needs no key.")));
                    controls.push(ui::details("connection-endpoint", ui::label_for(locale, "Custom endpoint (optional)"), ui::field(ui::label_for(locale, "Base URL"), state.base, false)));
                }
                col().class("connection-authentication").fill(controls).build(scope)
            })
        });
    let discover_ctx = ctx.clone();
    let discover_button = sapid::button()
        .variant(sapid::ButtonVariant::Primary)
        .label(ui::label_for(locale, "Verify and find models"))
        .loading(state.action.map(|action| action == "discover"))
        .enabled(scope.bond(move || {
            state.action.get().is_empty()
                && (state.provider.get() != "openai"
                    || state.method.get() == "api_key"
                    || state.signed_in.get())
        }))
        .on_press(move || {
            ui::set(state.manual, false);
            discover(&discover_ctx, state);
        });
    let selection = each(state.models)
        .key(|model| string(model, &["id"]))
        .children(move |entry| {
            let model = entry.value.get_untracked();
            let id = string(&model, &["id"]);
            let selected_id = id.clone();
            sapid::checkbox()
                .label(string(&model, &["name", "id"]))
                .checked(state.enabled.map(move |items| items.contains(&selected_id)))
                .on_checked_change(move |checked| {
                    let mut items = state.enabled.get();
                    if checked {
                        items.insert(id.clone());
                    } else {
                        items.remove(&id);
                    }
                    if !items.contains(&state.default_model.get_untracked()) {
                        ui::set(
                            state.default_model,
                            items.first().cloned().unwrap_or_default(),
                        );
                    }
                    ui::set(state.enabled, items);
                })
        });
    let default_options = scope.bond(move || {
        let _ = (
            state.models.get(),
            state.enabled.get(),
            state.manual.get(),
            state.manual_model.get(),
        );
        state
            .selected()
            .into_iter()
            .map(|entry| (string(&entry, &["id"]), string(&entry, &["name", "id"])))
            .collect()
    });
    let save_ctx = ctx.clone();
    let save_button = sapid::button()
        .variant(sapid::ButtonVariant::Primary)
        .label(ui::label_for(locale, "Save and start chatting"))
        .loading(state.action.map(|action| action == "save"))
        .enabled(scope.bond(move || {
            let _ = (
                state.models.get(),
                state.enabled.get(),
                state.manual.get(),
                state.manual_model.get(),
            );
            state.action.get().is_empty()
                && !state.selected().is_empty()
                && (state.provider.get() != "openai"
                    || state.method.get() == "api_key"
                    || state.signed_in.get())
        }))
        .on_press(move || save(&save_ctx, state));
    let children = vec![
        nav(ctx, "All models", "/models"),
        ui::error_text(ctx, state.error),
        card(
            ui::label_for(locale, "1 · Provider and authentication"),
            vec![
                select_for(locale, "Provider", state.provider, options),
                Box::new(authentication),
            ],
        ),
        card(
            ui::label_for(locale, "2 · Verify and choose models"),
            vec![
                Box::new(discover_button),
                Box::new(
                    text()
                        .class("connection-result success-message")
                        .content(state.result),
                ),
                Box::new(
                    col()
                        .class(state.manual.map(|manual| {
                            if *manual {
                                "connection-model-list connection-model-list--hidden".into()
                            } else {
                                "connection-model-list".into()
                            }
                        }))
                        .fill(selection),
                ),
                Box::new(
                    sapid::switch()
                        .label(ui::label_for(locale, "Use a model ID instead"))
                        .checked(state.manual)
                        .on_checked_change(move |manual| ui::set(state.manual, manual)),
                ),
                Box::new(when(state.manual, move || {
                    col().class("connection-manual-model").fill((
                ui::field(ui::label_for(locale, "Model ID"), state.manual_model, false),
                paragraph(ui::label_for(locale, "For endpoints without model discovery. This model will be saved without verification.")),
            ))
                })),
            ],
        ),
        card(
            ui::label_for(locale, "3 · Start your conversation"),
            vec![
                ui::field(
                    ui::label_for(locale, "Account name (optional)"),
                    state.account,
                    false,
                ),
                select_source(
                    ctx,
                    "Default model for this connection",
                    state.default_model,
                    default_options,
                ),
                paragraph(ui::label_for(
                    locale,
                    "Saving also makes the selected model your global default. You can change it in any conversation.",
                )),
                Box::new(save_button),
            ],
        ),
    ];
    Box::new(
        col().class("provider-connect").fill(
            sapid::fieldset("provider-connection-form")
                .class("connection-form")
                .legend(ui::label_for(locale, "Connect provider"))
                .disabled(state.action.map(|action| action == "save"))
                .fill(ui::page(
                    ui::label_for(locale, "Connect provider"),
                    ui::label_for(
                        locale,
                        "Connect once, choose a model, and start a conversation.",
                    ),
                    children,
                )),
        ),
    )
}

fn discover(ctx: &AppContext, state: Connection) {
    if !state.action.get_untracked().is_empty() {
        return;
    }
    let generation = state.generation.get_untracked();
    let credentials = state.credentials();
    ui::set(state.action, "discover".into());
    ui::set(state.error, String::new());
    ui::set(state.result, String::new());
    let context = ctx.clone();
    ctx.spawn(async move {
        let response = context
            .rpc
            .call::<Value>("models.list", Some(credentials))
            .await;
        if state.generation.get_untracked() != generation {
            return;
        }
        match response {
            Ok(value) => {
                let entries = array(&value, &["models"]);
                ui::set(
                    state.default_model,
                    entries
                        .first()
                        .map(|entry| string(entry, &["id"]))
                        .unwrap_or_default(),
                );
                ui::set(
                    state.enabled,
                    entries.iter().map(|entry| string(entry, &["id"])).collect(),
                );
                ui::set(
                    state.result,
                    ui::translate(
                        context.locale.get_untracked(),
                        if entries.is_empty() {
                            "No models found. You can enter a model ID below."
                        } else {
                            "Connection verified. Choose the models you want to use."
                        },
                    ),
                );
                ui::set(state.models, entries);
            }
            Err(message) => ui::set(state.error, message),
        }
        ui::set(state.action, String::new());
    });
}

fn save(ctx: &AppContext, state: Connection) {
    if !state.action.get_untracked().is_empty() {
        return;
    }
    let provider = canonical_provider_id(&state.provider.get_untracked());
    let mut entries = state.selected();
    if entries.is_empty() {
        return;
    }
    let selected_default = state.default_model.get_untracked();
    let default_entry = entries
        .iter()
        .find(|entry| entry["id"] == selected_default)
        .unwrap_or(&entries[0]);
    let default_id = string(default_entry, &["id"]);
    let default_slug = model_slug(default_entry, &provider);
    for entry in &mut entries {
        let slug = model_slug(entry, &provider);
        let is_default = entry["id"] == default_id;
        entry["provider"] = json!(provider);
        entry["model_slug"] = json!(slug);
        entry["is_default"] = json!(is_default);
        if let Some(object) = entry.as_object_mut() {
            object.remove("api_key");
        }
    }
    let credentials = state.credentials();
    let params = json!({"provider_id":provider,
        "name": if state.account.get_untracked().trim().is_empty() {provider_display_name(&provider)} else {state.account.get_untracked().trim().to_owned()},
        "models":entries,"api_key":credentials["api_key"],"env_key":provider_api_key_env(&provider),"base_url":credentials["base_url"]});
    ui::set(state.action, "save".into());
    ui::set(state.error, String::new());
    let context = ctx.clone();
    ctx.spawn(async move {
        match context
            .rpc
            .call::<Value>("models.import", Some(params))
            .await
        {
            Ok(value) => {
                let account = value["account_id"].as_str().unwrap_or(&provider);
                let model = format!("{account}/{default_slug}");
                context.navigate(&format!(
                    "/sessions?model={}",
                    crate::utils::deep_link::encode_query(&model)
                ));
            }
            Err(message) => {
                ui::set(state.error, message);
                ui::set(state.action, String::new());
            }
        }
    });
}
fn model_slug(entry: &Value, provider: &str) -> String {
    let slug = string(entry, &["model_slug"]);
    if !slug.is_empty() {
        return slug;
    }
    let id = string(entry, &["id"]);
    id.strip_prefix(&format!("{provider}/"))
        .unwrap_or(&id)
        .to_owned()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_slug_preserves_model_namespaces_and_explicit_slugs() {
        assert_eq!(
            model_slug(&json!({"id":"openrouter/vendor/model"}), "openrouter"),
            "vendor/model"
        );
        assert_eq!(
            model_slug(
                &json!({"id":"account/model", "model_slug":"vendor/model"}),
                "openai"
            ),
            "vendor/model"
        );
    }
}
