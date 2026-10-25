use glory::prelude::*;
use serde_json::{Value, json};

use super::super::common::*;
use crate::ui::{self, AppContext};
use crate::utils::provider_registry::{
    canonical_provider_id, known_provider_ids, provider_api_key_env, provider_default_base_url,
    provider_display_name,
};

pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let provider = ctx.scope.cage("openai".to_owned());
    let account = ctx.scope.cage(String::new());
    let base = ctx.scope.cage(String::new());
    let key = ctx.scope.cage(String::new());
    let models = ctx.scope.cage(Vec::<Value>::new());
    let enabled = ctx.scope.cage(std::collections::BTreeSet::<String>::new());
    let manual_model = ctx.scope.cage(String::new());
    let result = ctx.scope.cage(String::new());
    let busy = ctx.scope.cage(false);
    let error = ctx.scope.cage(String::new());
    let mut ids = std::collections::BTreeSet::new();
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
    let mut children = vec![
        nav(ctx, "All models", "/models"),
        ui::error_text(ctx, error),
        select("Provider", provider, options),
        ui::field("Account name", account, false),
        ui::field("Custom endpoint (optional)", base, false),
        secret("API key", key),
        Box::new(text().content(result)),
    ];
    let login_context = ctx.clone();
    children.push(Box::new(when(
        provider.map(|value| value == "openai"),
        move || {
            let context = login_context.clone();
            widget(move |scope| super::login::render(&context.with_scope(scope), key).build(scope))
        },
    )));
    for (label, method) in [
        ("Test connection", "models.test"),
        ("Discover models", "models.list"),
    ] {
        let context = ctx.clone();
        children.push(Box::new(
            sapid::button()
                .label(label)
                .loading(busy)
                .on_press(move || {
                    if busy.get() {
                        return;
                    }
                    ui::set(busy, true);
                    ui::set(error, String::new());
                    let provider = canonical_provider_id(&provider.get());
                    let base_url = if base.get().trim().is_empty() {
                        provider_default_base_url(&provider)
                            .unwrap_or("")
                            .to_owned()
                    } else {
                        base.get().trim().to_owned()
                    };
                    let params =
                        json!({"provider":provider,"base_url":base_url,"api_key":key.get()});
                    let context_async = context.clone();
                    context.spawn(async move {
                        match context_async.rpc.call::<Value>(method, Some(params)).await {
                            Ok(value) => {
                                if method == "models.list" {
                                    let entries = array(&value, &["models"]);
                                    ui::set(
                                        enabled,
                                        entries
                                            .iter()
                                            .filter_map(|entry| {
                                                entry["id"].as_str().map(str::to_owned)
                                            })
                                            .collect(),
                                    );
                                    ui::set(models, entries);
                                } else {
                                    ui::set(result, string(&value, &["message"]));
                                    if value["ok"].as_bool() == Some(false) {
                                        ui::set(error, "Connection test failed".to_owned());
                                    }
                                }
                            }
                            Err(message) => ui::set(error, message),
                        }
                        ui::set(busy, false);
                    });
                }),
        ));
    }
    children.push(Box::new(each(
        models,
        |model| string(model, &["id"]),
        move |model, _| {
            let model = model.get_untracked();
            let id = string(&model, &["id"]);
            let id_value = id.clone();
            let checked = enabled.map(move |items| items.contains(&id_value));
            Box::new(
                sapid::checkbox()
                    .label(string(&model, &["name", "id"]))
                    .checked(checked)
                    .on_checked_change(move |checked| {
                        let mut items = enabled.get();
                        if checked {
                            items.insert(id.clone());
                        } else {
                            items.remove(&id);
                        }
                        ui::set(enabled, items);
                    }),
            ) as Box<dyn Widget>
        },
    )));
    children.push(ui::field(
        "Manual model ID (for endpoints without discovery)",
        manual_model,
        false,
    ));
    let context = ctx.clone();
    children.push(Box::new(sapid::button().label("Save provider and models").loading(busy).on_press(move||{
        if busy.get(){return;}let provider=canonical_provider_id(&provider.get());let mut entries=models.get().into_iter().filter(|entry|enabled.get().contains(&string(entry,&["id"]))).collect::<Vec<_>>();
        if !manual_model.get_untracked().trim().is_empty(){let model=manual_model.get_untracked();entries.push(json!({"id":format!("{provider}/{model}"),"name":model,"model_slug":model}));}
        if entries.is_empty(){ui::set(error,"Choose at least one model or enter a model ID".to_owned());return;}
        for(i,entry)in entries.iter_mut().enumerate(){let id=string(entry,&["id"]);entry["provider"]=json!(provider);entry["model_slug"]=json!(entry["model_slug"].as_str().unwrap_or_else(||id.split('/').next_back().unwrap_or(&id)));entry["is_default"]=json!(i==0);entry.as_object_mut().map(|object|object.remove("api_key"));}
        let params=json!({"provider_id":provider,"name":if account.get().trim().is_empty(){provider_display_name(&provider)}else{account.get()},"models":entries,"api_key":key.get(),"env_key":provider_api_key_env(&provider),"base_url":if base.get().trim().is_empty(){provider_default_base_url(&provider).unwrap_or("").to_owned()}else{base.get()}});
        ui::set(busy,true);ui::set(error,String::new());let context_async=context.clone();context.spawn(async move{match context_async.rpc.call::<Value>("models.import",Some(params)).await{Ok(_)=>{ui::set(key,String::new());context_async.navigate("/models");},Err(message)=>ui::set(error,message)}ui::set(busy,false);});
    })));
    ui::page(
        "Connect provider",
        "Choose an endpoint, test credentials, select models and save the account",
        children,
    )
}
