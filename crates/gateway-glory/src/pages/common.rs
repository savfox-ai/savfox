//! Shared owner-scoped resources and semantic controls for gateway pages.
use glory::prelude::*;
use serde_json::Value;

use crate::ui::{self, AppContext};

#[derive(Clone)]
pub struct Query {
    pub value: Cage<Value>,
    pub error: Cage<String>,
    pub busy: Cage<bool>,
    pub revision: Cage<u64>,
}
impl Query {
    pub fn refresh(&self) {
        ui::set(self.revision, self.revision.get().wrapping_add(1));
    }
}
pub fn query(ctx: &AppContext, method: &str, params: Option<Value>) -> Query {
    let result = Query {
        value: ctx.scope.cage(Value::Null),
        error: ctx.scope.cage(String::new()),
        busy: ctx.scope.cage(false),
        revision: ctx.scope.cage(0),
    };
    let context = ctx.clone();
    let method = method.to_owned();
    let result_effect = result.clone();
    ctx.scope.effect(move || {
        let connected = context.connected.get();
        let _revision = result_effect.revision.get();
        if !connected {
            return;
        }
        let context_async = context.clone();
        let method = method.clone();
        let params = params.clone();
        let result = result_effect.clone();
        context.spawn(async move {
            ui::set(result.busy, true);
            ui::set(result.error, String::new());
            match context_async.rpc.call::<Value>(&method, params).await {
                Ok(value) => ui::set(result.value, value),
                Err(error) => ui::set(result.error, error),
            }
            ui::set(result.busy, false);
        });
    });
    result
}
pub fn mutate(ctx: &AppContext, state: &Query, method: &str, params: Value) {
    if state.busy.get() {
        return;
    }
    ui::set(state.busy, true);
    ui::set(state.error, String::new());
    let context = ctx.clone();
    let state = state.clone();
    let method = method.to_owned();
    ctx.spawn(async move {
        match context.rpc.call::<Value>(&method, Some(params)).await {
            Ok(_) => {
                ui::set(state.busy, false);
                state.refresh();
            }
            Err(error) => {
                ui::set(state.error, error);
                ui::set(state.busy, false);
            }
        }
    });
}
pub fn status(ctx: &AppContext, state: &Query) -> Box<dyn Widget> {
    Box::new(col().class("page-status").fill((
        ui::error_text(ctx, state.error),
        text().content(state.busy.map(|busy| {
            if *busy {
                "Loading…".to_owned()
            } else {
                String::new()
            }
        })),
    )))
}
pub fn refresh(ctx: &AppContext, state: &Query) -> Box<dyn Widget> {
    let state = state.clone();
    let connected = ctx.connected;
    Box::new(
        sapid::button()
            .label("Refresh")
            .enabled(connected)
            .on_press(move || state.refresh()),
    )
}
pub fn card(title: impl Into<String>, children: Vec<Box<dyn Widget>>) -> Box<dyn Widget> {
    Box::new(col().class("card").fill((
        text().class("card-title").content(title.into()),
        col().class("card-body").fill(children),
    )))
}
pub fn paragraph(value: impl Into<String>) -> Box<dyn Widget> {
    Box::new(text().content(value.into()))
}
pub fn toggle(label: &str, value: Cage<bool>) -> Box<dyn Widget> {
    Box::new(
        sapid::checkbox()
            .label(label.to_owned())
            .checked(value)
            .on_checked_change(move |next| ui::set(value, next)),
    )
}
/// Sapid selects semantic option keys; application values remain independent.
pub fn select(
    label: &str,
    value: Cage<String>,
    mut options: Vec<(String, String)>,
) -> Box<dyn Widget> {
    let initial = value.get();
    if !options.iter().any(|(key, _)| *key == initial) {
        options.push((
            initial.clone(),
            if initial.is_empty() {
                "Unspecified".to_owned()
            } else {
                format!("{initial} (saved)")
            },
        ));
    }
    let values = std::rc::Rc::new(
        options
            .iter()
            .map(|(value, _)| value.clone())
            .collect::<Vec<_>>(),
    );
    let selected_values = values.clone();
    let native_options = options
        .into_iter()
        .enumerate()
        .map(|(i, (value, label))| sapid::select_option(format!("option-{i}"), label).value(value));
    Box::new(
        sapid::native_select(select_key(label))
            .label(label.to_owned())
            .value(value.map(move |current| {
                selected_values
                    .iter()
                    .position(|value| value == current)
                    .map(|index| format!("option-{index}"))
            }))
            .options(native_options)
            .on_value_change(move |key| {
                if let Some(next) = key
                    .and_then(|key| {
                        key.strip_prefix("option-")
                            .and_then(|index| index.parse::<usize>().ok())
                    })
                    .and_then(|index| values.get(index))
                {
                    ui::set(value, next.clone());
                }
            }),
    )
}
pub(super) fn select_key(label: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    label.hash(&mut hash);
    format!("select-{:016x}", hash.finish())
}
pub fn choice(label: &str, value: Cage<String>, options: &[(&str, &str)]) -> Box<dyn Widget> {
    select(
        label,
        value,
        options
            .iter()
            .map(|(key, label)| (key.to_string(), label.to_string()))
            .collect(),
    )
}
pub fn secret(label: &str, value: Cage<String>) -> Box<dyn Widget> {
    Box::new(
        sapid::password_input()
            .name(label.to_owned())
            .value(value)
            .on_value_change(move |next| ui::set(value, next)),
    )
}
pub fn item_id(value: &Value) -> String {
    ["id", "agent_id", "node_id", "host", "name", "key", "path"]
        .iter()
        .find_map(|key| value[*key].as_str())
        .unwrap_or("")
        .to_owned()
}
pub fn string(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value[*key].as_str())
        .unwrap_or("")
        .to_owned()
}
pub fn array(value: &Value, keys: &[&str]) -> Vec<Value> {
    if let Some(items) = value.as_array() {
        return items.clone();
    }
    keys.iter()
        .find_map(|key| value[*key].as_array().cloned())
        .unwrap_or_default()
}
pub fn records(
    ctx: &AppContext,
    query: &Query,
    keys: &'static [&'static str],
    render: impl Fn(Value, AppContext) -> Box<dyn Widget> + 'static,
) -> Box<dyn Widget> {
    let items = ctx.scope.bond({
        let value = query.value;
        move || {
            array(&value.get(), keys)
                .into_iter()
                .enumerate()
                .map(|(i, value)| (format!("{i}-{}", record_key(&value)), value))
                .collect::<Vec<_>>()
        }
    });
    let context = ctx.clone();
    let render = std::rc::Rc::new(render);
    Box::new(col().class("record-list").fill((
        text().class("empty-state").content(ctx.scope.bond(move || {
            if items.get().is_empty() {
                "No entries".to_owned()
            } else {
                String::new()
            }
        })),
        each(
            items,
            |(key, _)| key.clone(),
            move |item, _| {
                let context = context.clone();
                let render = render.clone();
                widget(move |scope| {
                    let mut content = render(item.get_untracked().1, context.with_scope(scope));
                    content.build(scope)?;
                    scope.retain(content)
                })
            },
        ),
    )))
}
pub fn metadata(value: &Value, fields: &[(&str, &str)]) -> Box<dyn Widget> {
    let rows = fields
        .iter()
        .filter_map(|(key, label)| {
            let value = &value[*key];
            if value.is_null() {
                return None;
            }
            let value = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            Some(Box::new(text().content(format!("{label}: {value}"))) as Box<dyn Widget>)
        })
        .collect::<Vec<_>>();
    Box::new(col().class("metadata").fill(rows))
}
pub fn nav(ctx: &AppContext, label: &str, path: impl Into<String>) -> Box<dyn Widget> {
    let context = ctx.clone();
    let path = path.into();
    ui::action(label.to_owned(), move || context.navigate(&path))
}
pub fn confirmed(message: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.confirm_with_message(message).ok())
            .unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = message;
        false
    }
}
pub fn record_key(value: &Value) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.to_string().hash(&mut hasher);
    format!("{}-{:x}", item_id(value), hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_select_maps_empty_and_long_values_to_bounded_keys() {
        for initial in [
            String::new(),
            "account/".to_owned() + &"long-model".repeat(20),
            "unknown-saved-value".to_owned(),
        ] {
            let html = glory::ssr::render_with(|scope| {
                select(
                    "I/O protocol (configured)",
                    scope.cage(initial),
                    vec![
                        (String::new(), "Inherit".to_owned()),
                        (
                            "account/".to_owned() + &"long-model".repeat(20),
                            "Long model".to_owned(),
                        ),
                    ],
                )
            })
            .expect("application values must never become semantic keys");
            assert!(html.contains("Inherit"));
            assert!(html.contains("Long model"));
        }
    }
}
