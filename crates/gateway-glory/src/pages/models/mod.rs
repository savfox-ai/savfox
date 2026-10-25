use glory::prelude::*;
use serde_json::{Value, json};

use super::common::*;
use crate::ui::{self, AppContext};
pub mod connect_provider;
mod login;
use crate::utils::model_visibility::{
    ModelKey, is_model_visible, load_model_preferences, save_model_preferences,
    set_model_visibility,
};
pub fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    if path == "/models/connect-provider" {
        return connect_provider::render(ctx);
    }
    let state = query(ctx, "models.list", None);
    let config = query(ctx, "config.get", None);
    let filter = ctx.scope.cage(String::new());
    let result = ctx.scope.cage(String::new());
    let preferences = ctx.scope.cage(load_model_preferences());
    let value = state.value;
    let context = ctx.clone();
    let row_state = state.clone();
    let items = ctx.scope.bond(move || {
        array(&value.get(), &["models"])
            .into_iter()
            .filter(|model| {
                model
                    .to_string()
                    .to_lowercase()
                    .contains(&filter.get().to_lowercase())
            })
            .enumerate()
            .map(|(i, v)| (format!("{i}-{}", record_key(&v)), v))
            .collect::<Vec<_>>()
    });
    ui::localized_page(
        ctx,
        "nav.models",
        "Available models and connected provider accounts",
        vec![
            nav(ctx, "Connect provider", "/models/connect-provider"),
            status(ctx, &state),
            refresh(ctx, &state),
            ui::field("Search models", filter, false),
            Box::new(text().content(result)),
            Box::new(text().content(config.value.map(|v| {
                format!(
                    "Global default: {}",
                    v.pointer("/config/model").unwrap_or(&Value::Null)
                )
            }))),
            Box::new(each(
                items,
                |(id, _)| id.clone(),
                move |item, _| {
                    let model = item.get_untracked().1;
                    let visibility = visibility_key(&model);
                    let visibility_read = visibility.clone();
                    let visible = preferences
                        .map(move |preferences| is_model_visible(preferences, &visibility_read));
                    let id = string(&model, &["id"]);
                    let slug = string(&model, &["model_slug"]);
                    let slug = if slug.is_empty() {
                        id.split('/').next_back().unwrap_or(&id).to_owned()
                    } else {
                        slug
                    };
                    let provider = string(&model, &["provider"]);
                    let provider = if provider.is_empty() {
                        id.split('/').next().unwrap_or("").to_owned()
                    } else {
                        provider
                    };
                    let context_default = context.clone();
                    let state = row_state.clone();
                    let provider_default = provider.clone();
                    let context_test = context.clone();
                    let state_test = row_state.clone();
                    let account = string(&model, &["account_slug", "provider"]);
                    let context_delete = context.clone();
                    let state_delete = row_state.clone();
                    card(
                        string(&model, &["name", "id"]),
                        vec![
                            Box::new(
                                sapid::checkbox()
                                    .label("Visible in model picker")
                                    .checked(visible)
                                    .on_checked_change(move |visible| {
                                        let mut current = preferences.get();
                                        set_model_visibility(&mut current, &visibility, visible);
                                        save_model_preferences(&current);
                                        ui::set(preferences, current);
                                    }),
                            ),
                            metadata(
                                &model,
                                &[
                                    ("id", "Model ID"),
                                    ("provider", "Provider"),
                                    ("account_name", "Account"),
                                    ("base_url", "Endpoint"),
                                    ("max_tokens", "Max tokens"),
                                    ("default_reasoning_level", "Reasoning"),
                                    ("is_default", "Default"),
                                ],
                            ),
                            ui::action("Set as global default", move || {
                                mutate(
                                    &context_default,
                                    &state,
                                    "config.patch",
                                    json!({"patch":{"model":{"slug":slug,"provider":provider_default}}}),
                                )
                            }),
                            ui::action("Test connection", move || {
                                let context_async = context_test.clone();
                                let provider = provider.clone();
                                let error = state_test.error;
                                context_test.spawn(async move {
                                    match context_async
                                        .rpc
                                        .call::<Value>(
                                            "models.test",
                                            Some(json!({"provider":provider})),
                                        )
                                        .await
                                    {
                                        Ok(value) => ui::set(result, string(&value, &["message"])),
                                        Err(message) => ui::set(error, message),
                                    }
                                });
                            }),
                            ui::action("Delete account", move || {
                                if confirmed("Delete this provider account and its models?") {
                                    mutate(
                                        &context_delete,
                                        &state_delete,
                                        "models.deleteAccount",
                                        json!({"account_id":account}),
                                    );
                                }
                            }),
                        ],
                    )
                },
            )),
        ],
    )
}

/// Keep visibility keys aligned with the provider catalogue, including account IDs.
pub(crate) fn visibility_key(model: &Value) -> ModelKey {
    let id = string(model, &["id"]);
    let provider = string(model, &["provider"]);
    let provider = if provider.trim().is_empty() {
        id.split('/').next().unwrap_or("other").to_owned()
    } else {
        provider
    };
    let slug = string(model, &["model_slug"]);
    let model_id = if !slug.trim().is_empty() {
        slug
    } else {
        id.strip_prefix(&format!("{provider}/"))
            .unwrap_or(&id)
            .to_owned()
    };
    ModelKey::new(provider, model_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visibility_uses_account_and_slug_like_the_model_picker() {
        assert_eq!(
            visibility_key(
                &json!({"id":"openai-work/gpt-5","provider":"openai-work","model_slug":"gpt-5"})
            ),
            ModelKey::new("openai-work", "gpt-5")
        );
        assert_eq!(
            visibility_key(&json!({"id":"openai/gpt-5"})),
            ModelKey::new("openai", "gpt-5")
        );
    }
    #[test]
    fn visibility_keeps_unrelated_provider_prefixes() {
        assert_eq!(
            visibility_key(&json!({"id":"vendor/model","provider":"custom"})),
            ModelKey::new("custom", "vendor/model")
        );
    }
}
