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
    let locale = ctx.locale;
    let state = query(ctx, "models.list", None);
    let config = query(ctx, "config.get", None);
    let filter = ctx.scope.cage(String::new());
    let result = ctx.scope.cage(String::new());
    let preferences = ctx.scope.cage(load_model_preferences());
    let value = state.value;
    let groups = ctx.scope.bond(move || {
        let search = filter.get().to_lowercase();
        let mut groups = std::collections::BTreeMap::<String, Vec<Value>>::new();
        for model in array(&value.get(), &["models"]) {
            if !model.to_string().to_lowercase().contains(&search) {
                continue;
            }
            let id = string(&model, &["id"]);
            let account = id.split('/').next().unwrap_or("other").to_owned();
            groups.entry(account).or_default().push(model);
        }
        groups
            .into_iter()
            .map(|(account, models)| {
                (
                    format!("{account}-{}", record_key(&json!(models))),
                    account,
                    models,
                )
            })
            .collect::<Vec<_>>()
    });
    let context = ctx.clone();
    let state_rows = state.clone();
    let sections = each(
        groups,
        |(key, ..)| key.clone(),
        move |group, _| {
            let (_, account, models) = group.get_untracked();
            let first = models.first().cloned().unwrap_or(Value::Null);
            let provider = string(&first, &["provider_name", "provider"]);
            let account_name = string(&first, &["account_name"]);
            let title = if account_name.is_empty() {
                provider.clone()
            } else {
                format!("{provider} / {account_name}")
            };
            let title = if title.trim().is_empty() {
                account.clone()
            } else {
                title
            };
            let context_test = context.clone();
            let state_test = state_rows.clone();
            let test_account = account.clone();
            let context_delete = context.clone();
            let state_delete = state_rows.clone();
            let delete_account = account.clone();
            let model_context = context.clone();
            let state_models = state_rows.clone();
            col().class("provider-section").fill((
  row().class("provider-header").fill((
   row().class("provider-header__main").fill((text().class("provider-name").kind(TextKind::Heading(2)).content(title),text().class("provider-count").content(models.len().to_string()))),
   row().class("provider-actions").fill((
    ui::action(ui::label_for(locale,"Test connection"),move ||{let context_async=context_test.clone();let provider=test_account.clone();let error=state_test.error;context_test.spawn(async move{match context_async.rpc.call::<Value>("models.test",Some(json!({"provider":provider}))).await{Ok(value)=>ui::set(result,string(&value,&["message"])),Err(message)=>ui::set(error,message)}});}),
    ui::action(ui::label_for(locale,"Delete account"),move ||{if confirmed("Delete this provider account and its models?"){mutate(&context_delete,&state_delete,"models.deleteAccount",json!({"account_id":delete_account}));}})
   ))
  )),
  col().class("models-card-grid").fill(models.into_iter().map(move |model|{
   let id=string(&model,&["id"]);let visibility=visibility_key(&model);let read=visibility.clone();let checked=preferences.map(move |current|is_model_visible(current,&read));let context=model_context.clone();let state=state_models.clone();let provider=string(&model,&["provider"]);let slug=string(&model,&["model_slug"]);let slug=if slug.is_empty(){id.split('/').next_back().unwrap_or(&id).to_owned()}else{slug};let name=string(&model,&["name","model_slug","id"]);let is_default=model["is_default"].as_bool().unwrap_or(false);let details_key=select_key(&id);
   Box::new(col().class(if is_default{"model-card model-card--default"}else{"model-card"}).fill((
    row().class("model-card__header").fill((text().class("model-card__name").content(name),sapid::checkbox().unstyled().class("model-visibility").label(ui::label_for(locale,"Visible in model picker")).checked(checked).on_checked_change(move |visible|{let mut current=preferences.get();set_model_visibility(&mut current,&visibility,visible);save_model_preferences(&current);ui::set(preferences,current);}))),
    text().class("model-card__specs").content(string(&model,&["default_reasoning_level"])),
    ui::details(&details_key,ui::label_for(locale,"More"),Box::new(col().class("model-card__details").fill((text().class("model-card__meta").content(id),metadata_for(locale,&model,&[("max_tokens","Max tokens"),("temperature","Temperature"),("default_reasoning_level","Reasoning")]),
    row().class("model-card__footer").fill(ui::action(ui::label_for(locale,if is_default{"Global default model"}else{"Set as global default"}),move ||mutate(&context,&state,"config.patch",json!({"patch":{"model":{"slug":slug,"provider":provider}}}))))))))
   ))) as Box<dyn Widget>
  }).collect::<Vec<_>>())
 ))
        },
    );
    ui::localized_page(
        ctx,
        "nav.models",
        "Available models and connected provider accounts",
        vec![
            Box::new(row().class("models-toolbar management-toolbar").fill((
                ui::field(ui::label_for(locale, "Search models"), filter, false),
                row().class("toolbar-actions").fill((
                    refresh(ctx, &state),
                    nav(ctx, "Connect provider", "/models/connect-provider"),
                )),
            ))),
            status(ctx, &state),
            Box::new(text().class("success-message").content(result)),
            Box::new(
                text()
                    .class("models-current-default")
                    .content(config.value.map(move |v| {
                        let model = v.pointer("/config/model");
                        let value = model
                            .and_then(|model| {
                                model
                                    .as_str()
                                    .map(str::to_owned)
                                    .or_else(|| model["slug"].as_str().map(str::to_owned))
                            })
                            .unwrap_or_default();
                        if value.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "{}: {value}",
                                ui::translate(locale.get(), "Global default model")
                            )
                        }
                    })),
            ),
            Box::new(sections),
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
