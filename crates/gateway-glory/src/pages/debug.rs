use std::result::Result;

use glory::prelude::*;
use serde_json::Value;

use super::common::*;
use crate::ui::{self, AppContext};
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct History {
    method: String,
    params: String,
    result: String,
    success: bool,
}
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let state = query(ctx, "status", None);
    let health = ctx.scope.cage(Value::Null);
    let method = ctx.scope.cage(String::new());
    let params = ctx.scope.cage("{}".to_owned());
    let output = ctx.scope.cage(Value::Null);
    let pinned = ctx.scope.cage(Value::Null);
    let history = ctx.scope.cage(load_history());
    let busy = ctx.scope.cage(false);
    let error = state.error;
    let context = ctx.clone();
    ctx.spawn(async move {
        if let Ok(value) = crate::api::client::fetch_json::<Value>("GET", "/health", None).await {
            ui::set(health, value);
        }
    });
    let mut children = vec![
        status(ctx, &state),
        refresh(ctx, &state),
        card("Gateway status", vec![ui::json_text(ctx, state.value)]),
        card("HTTP health", vec![ui::json_text(ctx, health)]),
    ];
    let examples = [
        "status",
        "models.list",
        "sessions.list",
        "system-presence",
        "config.get",
    ]
    .iter()
    .map(|example| {
        let example = example.to_string();
        ui::action(example.clone(), move || {
            ui::set(method, example.clone());
            ui::set(params, "{}".to_owned());
        })
    })
    .collect::<Vec<_>>();
    children.push(Box::new(row().class("actions").fill(examples)));
    children.push(ui::field("RPC method", method, false));
    children.push(ui::field("Parameters (JSON)", params, true));
    children.push(Box::new(
        sapid::button()
            .label("Call method")
            .loading(busy)
            .on_press(move || {
                if method.get().trim().is_empty() {
                    ui::set(error, "Method is required".to_owned());
                    return;
                }
                let parsed = match parse_params(&params.get()) {
                    Ok(value) => value,
                    Err(message) => {
                        ui::set(error, message);
                        return;
                    }
                };
                let context_async = context.clone();
                let method = method.get();
                let parameters = params.get();
                ui::set(busy, true);
                ui::set(error, String::new());
                context.spawn(async move {
                    let result = context_async.rpc.call::<Value>(&method, parsed).await;
                    let success = result.is_ok();
                    let result = match result {
                        Ok(value) => {
                            let formatted =
                                serde_json::to_string_pretty(&value).unwrap_or_default();
                            ui::set(output, value);
                            formatted
                        }
                        Err(message) => {
                            ui::set(error, message.clone());
                            message
                        }
                    };
                    let mut entries = history.get();
                    entries.insert(
                        0,
                        History {
                            method,
                            params: parameters,
                            result,
                            success,
                        },
                    );
                    entries.truncate(20);
                    save_history(&entries);
                    ui::set(history, entries);
                    ui::set(busy, false);
                });
            }),
    ));
    children.push(card(
        "Response",
        vec![
            ui::json_text(ctx, output),
            ui::action("Pin response", move || ui::set(pinned, output.get())),
        ],
    ));
    children.push(card("Pinned response", vec![ui::json_text(ctx, pinned)]));
    children.push(card(
        "Call history",
        vec![
            Box::new(each(
                history.map(|entries| {
                    entries
                        .iter()
                        .enumerate()
                        .map(|(i, entry)| {
                            (
                                format!("{i}-{}-{}", entry.method, entry.result),
                                entry.clone(),
                            )
                        })
                        .collect::<Vec<_>>()
                }),
                |(key, _)| key.clone(),
                move |item, _| {
                    let (_, entry) = item.get_untracked();
                    let title = format!(
                        "{} · {}",
                        entry.method,
                        if entry.success { "success" } else { "failed" }
                    );
                    let method_value = entry.method;
                    let params_value = entry.params;
                    card(
                        title,
                        vec![
                            paragraph(entry.result),
                            ui::action("Reuse", move || {
                                ui::set(method, method_value.clone());
                                ui::set(params, params_value.clone());
                            }),
                        ],
                    )
                },
            )),
            ui::action("Clear history", move || {
                save_history(&[]);
                ui::set(history, Vec::new());
            }),
        ],
    ));
    ui::localized_page(
        ctx,
        "nav.debug",
        "Inspect gateway health and call JSON-RPC methods",
        children,
    )
}
fn parse_params(input: &str) -> Result<Option<Value>, String> {
    if input.trim().is_empty() {
        Ok(None)
    } else {
        serde_json::from_str(input)
            .map(Some)
            .map_err(|error| format!("Invalid JSON parameters: {error}"))
    }
}
fn load_history() -> Vec<History> {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.session_storage().ok().flatten())
            .and_then(|s| s.get_item("savfox_rpc_history").ok().flatten())
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Vec::new()
    }
}
fn save_history(entries: &[History]) {
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) = web_sys::window().and_then(|w| w.session_storage().ok().flatten())
        && let Ok(value) = serde_json::to_string(entries)
    {
        let _ = storage.set_item("savfox_rpc_history", &value);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = entries;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_params_and_json_null_are_distinct() {
        assert_eq!(parse_params(" "), Ok(None));
        assert_eq!(parse_params("null"), Ok(Some(Value::Null)));
        assert!(parse_params("{bad}").is_err());
    }
}
