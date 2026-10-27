#![allow(dead_code)]
use std::collections::{BTreeSet, HashMap};

use glory::prelude::*;
use serde_json::{Value, json};

use super::common::*;
use crate::ui::{self, AppContext};
#[derive(Clone, Debug, PartialEq)]
struct DiffEntry {
    path: String,
    kind: DiffKind,
    old_value: Option<serde_json::Value>,
    new_value: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq)]
enum DiffKind {
    Added,
    Removed,
    Changed,
}

fn compute_deep_diff(
    original: &HashMap<String, serde_json::Value>,
    current: &HashMap<String, serde_json::Value>,
) -> Vec<DiffEntry> {
    let mut entries = Vec::new();
    let mut all_keys: Vec<&String> = original.keys().chain(current.keys()).collect();
    all_keys.sort();
    all_keys.dedup();

    for key in all_keys {
        let orig = original.get(key);
        let curr = current.get(key);
        match (orig, curr) {
            (Some(o), Some(c)) => {
                if o != c {
                    diff_values(key, o, c, &mut entries);
                }
            }
            (Some(o), None) => {
                entries.push(DiffEntry {
                    path: key.clone(),
                    kind: DiffKind::Removed,
                    old_value: Some(o.clone()),
                    new_value: None,
                });
            }
            (None, Some(c)) => {
                entries.push(DiffEntry {
                    path: key.clone(),
                    kind: DiffKind::Added,
                    old_value: None,
                    new_value: Some(c.clone()),
                });
            }
            (None, None) => {}
        }
    }
    entries
}

fn diff_values(
    prefix: &str,
    old: &serde_json::Value,
    new: &serde_json::Value,
    entries: &mut Vec<DiffEntry>,
) {
    if old == new {
        return;
    }
    match (old, new) {
        (serde_json::Value::Object(old_map), serde_json::Value::Object(new_map)) => {
            let mut all_keys: Vec<&String> = old_map.keys().chain(new_map.keys()).collect();
            all_keys.sort();
            all_keys.dedup();

            for key in all_keys {
                let child_path = format!("{}.{}", prefix, key);
                match (old_map.get(key), new_map.get(key)) {
                    (Some(ov), Some(nv)) => {
                        diff_values(&child_path, ov, nv, entries);
                    }
                    (Some(ov), None) => {
                        entries.push(DiffEntry {
                            path: child_path,
                            kind: DiffKind::Removed,
                            old_value: Some(ov.clone()),
                            new_value: None,
                        });
                    }
                    (None, Some(nv)) => {
                        entries.push(DiffEntry {
                            path: child_path,
                            kind: DiffKind::Added,
                            old_value: None,
                            new_value: Some(nv.clone()),
                        });
                    }
                    (None, None) => {}
                }
            }
        }
        _ => {
            entries.push(DiffEntry {
                path: prefix.to_string(),
                kind: DiffKind::Changed,
                old_value: Some(old.clone()),
                new_value: Some(new.clone()),
            });
        }
    }
}

// ============== Validation ==============

#[derive(Clone, Debug, PartialEq)]
enum ValidityState {
    Valid,
    Invalid(usize),
    Unknown,
}

fn validate_config(
    form_value: &HashMap<String, serde_json::Value>,
    schema_props: &HashMap<String, serde_json::Value>,
) -> ValidityState {
    if schema_props.is_empty() {
        return ValidityState::Unknown;
    }
    let mut error_count = 0;
    for (key, schema) in schema_props {
        let value = form_value.get(key);
        error_count += validate_value_against_schema(value, schema);
    }
    if error_count == 0 {
        ValidityState::Valid
    } else {
        ValidityState::Invalid(error_count)
    }
}

fn validate_value_against_schema(
    value: Option<&serde_json::Value>,
    schema: &serde_json::Value,
) -> usize {
    let resolved_schema = resolve_schema_variant(schema, value);
    let field_type = schema_primary_type(&resolved_schema).unwrap_or_else(|| "object".to_string());
    let required_fields: Vec<String> = resolved_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mut errors = 0;

    if value.is_some_and(|v| v.is_null()) {
        if !schema_is_nullable(&resolved_schema) {
            errors += 1;
        }
        return errors;
    }

    match field_type.as_str() {
        "object" => {
            if let Some(props) = resolved_schema
                .get("properties")
                .and_then(|p| p.as_object())
            {
                let val_obj = value.and_then(|v| v.as_object());
                for req in &required_fields {
                    let field_val = val_obj.and_then(|o| o.get(req));
                    if is_empty_value(field_val) {
                        errors += 1;
                    }
                }
                // Recurse into nested properties
                if let Some(obj) = val_obj {
                    for (prop_key, prop_schema) in props {
                        let child_val = obj.get(prop_key);
                        errors += validate_value_against_schema(child_val, prop_schema);
                    }
                }
            }
        }
        "array" => {
            if let Some(v) = value
                && !v.is_array()
            {
                errors += 1;
            }
        }
        "number" | "integer" => {
            if let Some(v) = value {
                match v {
                    serde_json::Value::Number(_) => {}
                    serde_json::Value::String(s) => {
                        if !s.is_empty() && s.parse::<f64>().is_err() {
                            errors += 1;
                        }
                    }
                    _ => {
                        errors += 1;
                    }
                }
            }
        }
        "boolean" => {
            if let Some(v) = value {
                match v {
                    serde_json::Value::Bool(_) => {}
                    _ => {
                        errors += 1;
                    }
                }
            }
        }
        "string" => {
            if let Some(v) = value
                && !v.is_string()
            {
                errors += 1;
            }
        }
        _ => {}
    }

    errors
}

fn is_empty_value(value: Option<&serde_json::Value>) -> bool {
    match value {
        None => true,
        Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => s.is_empty(),
        Some(serde_json::Value::Array(a)) => a.is_empty(),
        Some(serde_json::Value::Object(o)) => o.is_empty(),
        _ => false,
    }
}

fn schema_primary_type(schema: &serde_json::Value) -> Option<String> {
    match schema.get("type") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Array(types)) => {
            let non_null = types
                .iter()
                .filter_map(|t| t.as_str())
                .find(|t| *t != "null")
                .map(ToString::to_string);
            non_null.or_else(|| {
                types
                    .iter()
                    .filter_map(|t| t.as_str())
                    .next()
                    .map(ToString::to_string)
            })
        }
        _ => {
            if schema.get("properties").is_some() {
                Some("object".to_string())
            } else if schema.get("items").is_some() {
                Some("array".to_string())
            } else {
                None
            }
        }
    }
}

fn schema_is_nullable(schema: &serde_json::Value) -> bool {
    if let Some(serde_json::Value::String(s)) = schema.get("type") {
        return s == "null";
    }
    if let Some(serde_json::Value::Array(types)) = schema.get("type") {
        return types.iter().any(|t| t.as_str() == Some("null"));
    }
    if let Some(one_of) = schema.get("oneOf").and_then(|v| v.as_array()) {
        return one_of.iter().any(schema_is_nullable);
    }
    if let Some(any_of) = schema.get("anyOf").and_then(|v| v.as_array()) {
        return any_of.iter().any(schema_is_nullable);
    }
    false
}

fn value_matches_type(value: &serde_json::Value, ty: &str) -> bool {
    match ty {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.as_f64().is_some(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn value_matches_schema(value: &serde_json::Value, schema: &serde_json::Value) -> bool {
    if value.is_null() {
        return schema_is_nullable(schema)
            || schema_primary_type(schema).as_deref() == Some("null");
    }

    if let Some(enum_vals) = schema.get("enum").and_then(|v| v.as_array()) {
        return enum_vals.iter().any(|allowed| allowed == value);
    }

    if let Some(ty) = schema_primary_type(schema) {
        return value_matches_type(value, &ty);
    }

    true
}

fn resolve_schema_variant(
    schema: &serde_json::Value,
    value: Option<&serde_json::Value>,
) -> serde_json::Value {
    let variants = schema
        .get("oneOf")
        .and_then(|v| v.as_array())
        .or_else(|| schema.get("anyOf").and_then(|v| v.as_array()));

    let Some(variants) = variants else {
        return schema.clone();
    };

    if let Some(current) = value
        && let Some(matched) = variants
            .iter()
            .find(|candidate| value_matches_schema(current, candidate))
    {
        return matched.clone();
    }

    if let Some(non_null) = variants
        .iter()
        .find(|candidate| schema_primary_type(candidate).as_deref() != Some("null"))
    {
        return non_null.clone();
    }

    variants.first().cloned().unwrap_or_else(|| schema.clone())
}

fn field_validation_error(
    schema: &serde_json::Value,
    value: Option<&serde_json::Value>,
) -> Option<String> {
    let value = value?;

    if value.is_null() {
        if !schema_is_nullable(schema) {
            return Some("This field cannot be null".to_string());
        }
        return None;
    }

    if let Some(enum_vals) = schema.get("enum").and_then(|v| v.as_array()) {
        if !enum_vals.iter().any(|allowed| allowed == value) {
            return Some("Value is not in allowed enum options".to_string());
        }
        return None;
    }

    if let Some(ty) = schema_primary_type(schema)
        && !value_matches_type(value, &ty)
    {
        return Some(format!("Expected {ty} value"));
    }

    None
}

fn infer_schema_from_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Null => json!({ "type": ["string", "null"] }),
        serde_json::Value::Bool(_) => json!({ "type": "boolean" }),
        serde_json::Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                json!({ "type": "integer" })
            } else {
                json!({ "type": "number" })
            }
        }
        serde_json::Value::String(_) => json!({ "type": "string" }),
        serde_json::Value::Array(items) => {
            let item_schema = items
                .iter()
                .find(|item| !item.is_null())
                .map(infer_schema_from_value)
                .unwrap_or_else(|| json!({ "type": "string" }));
            json!({
                "type": "array",
                "items": item_schema
            })
        }
        serde_json::Value::Object(map) => {
            let mut props = serde_json::Map::new();
            for (key, val) in map {
                props.insert(key.clone(), infer_schema_from_value(val));
            }
            let mut obj = serde_json::Map::new();
            obj.insert("type".to_string(), json!("object"));
            if !props.is_empty() {
                obj.insert("properties".to_string(), serde_json::Value::Object(props));
            }
            serde_json::Value::Object(obj)
        }
    }
}

fn merge_schema_with_value(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> serde_json::Value {
    let mut merged = schema.clone();
    let Some(value_obj) = value.as_object() else {
        return merged;
    };

    if !merged.is_object() {
        merged = json!({});
    }

    let has_additional_props = merged.get("additionalProperties").is_some();
    let Some(merged_obj) = merged.as_object_mut() else {
        return merged;
    };

    if let Some(props) = merged_obj
        .get_mut("properties")
        .and_then(|p| p.as_object_mut())
    {
        for (key, val) in value_obj {
            props
                .entry(key.clone())
                .or_insert_with(|| infer_schema_from_value(val));
        }
        return merged;
    }

    if !has_additional_props && !value_obj.is_empty() {
        let mut inferred_props = serde_json::Map::new();
        for (key, val) in value_obj {
            inferred_props.insert(key.clone(), infer_schema_from_value(val));
        }
        merged_obj.insert("type".to_string(), json!("object"));
        merged_obj.insert(
            "properties".to_string(),
            serde_json::Value::Object(inferred_props),
        );
    }

    merged
}

pub fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let config = query(ctx, "config.get", None);
    let schema = query(ctx, "config.schema", None);
    let draft = ctx.scope.cage(json!({}));
    let raw = ctx.scope.cage(String::new());
    let value = config.value;
    ctx.scope.effect(move || {
        let value = value.get();
        if let Some(config) = value.get("config") {
            ui::set(draft, config.clone());
            ui::set(
                raw,
                serde_json::to_string_pretty(config).unwrap_or_default(),
            );
        }
    });
    let section = ctx
        .scope
        .cage(path.strip_prefix("/config/").map(str::to_owned));
    let config_value = config.value;
    let schema_value = schema.value;
    let context = ctx.clone();
    let field_error = config.error;
    let sections = ctx.scope.bond(move || {
        let config = config_value.get();
        let schema = schema_value.get();
        let value = config.get("config").cloned().unwrap_or(json!({}));
        let schema = schema.get("schema").cloned().unwrap_or(json!({}));
        let mut keys = BTreeSet::new();
        if let Some(values) = value.as_object() {
            keys.extend(values.keys().cloned());
        }
        if let Some(properties) = schema["properties"].as_object() {
            keys.extend(properties.keys().cloned());
        }
        ordered_section_keys(keys)
            .into_iter()
            .filter(|key| section.get().as_ref().is_none_or(|section| section == key))
            .map(|key| {
                let fingerprint = format!("{key}-{}-{}", value[&key], schema["properties"][&key]);
                (fingerprint, key, value.clone(), schema.clone())
            })
            .collect::<Vec<_>>()
    });
    let mut children = vec![
        status(ctx, &config),
        status(ctx, &schema),
        Box::new(each(
            sections,
            |(key, ..)| key.clone(),
            move |item, _| {
                let (_, key, _, root_schema) = item.get_untracked();
                let schema = root_schema["properties"][&key].clone();
                let current = draft
                    .get_untracked()
                    .get(&key)
                    .cloned()
                    .unwrap_or(Value::Null);
                let object = schema_primary_type(&schema).as_deref() == Some("object")
                    || current.is_object();
                let body = field(
                    &context,
                    draft,
                    vec![key.clone()],
                    &schema,
                    &root_schema,
                    field_error,
                    0,
                );
                if object {
                    body
                } else {
                    card(ui::label(&context, key), vec![body])
                }
            },
        )),
    ];
    children.push(ui::details(
        "config-review",
        ui::label_for(locale, "Review configuration"),
        card(
            ui::label_for(locale, "Review configuration"),
            vec![
                Box::new(text().class("config-diff").content(ctx.scope.bond(move || {
                    let original = config_value.get()["config"]
                        .as_object()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect::<HashMap<_, _>>();
                    let current = draft
                        .get()
                        .as_object()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect::<HashMap<_, _>>();
                    compute_deep_diff(&original, &current)
                        .iter()
                        .map(|entry| format!("{:?}: {}", entry.kind, entry.path))
                        .collect::<Vec<_>>()
                        .join("\n")
                }))),
                Box::new(text().class("config-preview").content(
                    draft.map(|value| serde_json::to_string_pretty(value).unwrap_or_default()),
                )),
            ],
        ),
    ));
    let mut actions = vec![refresh(ctx, &config)];
    for (label, apply) in [("Save configuration", false), ("Save and apply", true)] {
        let context = ctx.clone();
        let state = config.clone();
        actions.push(Box::new(
            sapid::button()
                .unstyled()
                .class(if apply {
                    "btn btn-primary"
                } else {
                    "btn btn-secondary"
                })
                .label(ui::label_for(locale, label))
                .loading(config.busy)
                .on_press(move || {
                    let document = draft.get();
                    let schema = schema_value.get();
                    let props = schema
                        .pointer("/schema/properties")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    let form = document
                        .as_object()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect();
                    if let ValidityState::Invalid(count) =
                        validate_config(&form, &props.into_iter().collect())
                    {
                        ui::set(
                            state.error,
                            format!("Fix {count} configuration validation errors before saving"),
                        );
                        return;
                    }
                    let context_async = context.clone();
                    let state = state.clone();
                    ui::set(state.busy, true);
                    ui::set(state.error, String::new());
                    context.spawn(async move {
                        let result = context_async
                            .rpc
                            .call::<Value>("config.patch", Some(json!({"patch":document})))
                            .await;
                        let result = if apply && result.is_ok() {
                            context_async
                                .rpc
                                .call::<Value>("config.apply", Some(json!({"config":document})))
                                .await
                        } else {
                            result
                        };
                        match result {
                            Ok(_) => state.refresh(),
                            Err(error) => ui::set(state.error, error),
                        }
                        ui::set(state.busy, false);
                    });
                }),
        ));
    }
    actions.push(ui::action(
        ui::label_for(locale, "Export configuration"),
        move || {
            crate::utils::download::trigger_download(
                "savfox-config.json",
                &serde_json::to_string_pretty(&draft.get()).unwrap_or_default(),
                "application/json",
            )
        },
    ));
    actions.push(ui::action(
        ui::label_for(locale, "Revert all changes"),
        move || {
            if let Some(value) = config_value.get().get("config") {
                ui::set(draft, value.clone());
            }
        },
    ));
    let error = config.error;
    children.push(ui::details(
        "config-import",
        ui::label_for(locale, "JSON import"),
        card(
            ui::label_for(locale, "JSON import"),
            vec![
                ui::field(ui::label_for(locale, "Configuration JSON"), raw, true),
                ui::action(ui::label_for(locale, "Import into form"), move || {
                    match serde_json::from_str::<Value>(&raw.get()) {
                        Ok(value) if value.is_object() => {
                            ui::set(draft, value);
                            ui::set(error, String::new());
                        }
                        Ok(_) => ui::set(error, "Configuration must be a JSON object".to_owned()),
                        Err(message) => ui::set(error, message.to_string()),
                    }
                }),
            ],
        ),
    ));
    let search = ctx.scope.cage(String::new());
    let schema_nav = schema.value;
    let config_nav = config.value;
    let section_names = ctx.scope.bond(move || {
        let schema = schema_nav.get();
        let config = config_nav.get();
        let mut keys = BTreeSet::new();
        if let Some(properties) = schema
            .pointer("/schema/properties")
            .and_then(Value::as_object)
        {
            keys.extend(properties.keys().cloned());
        }
        if let Some(properties) = config["config"].as_object() {
            keys.extend(properties.keys().cloned());
        }
        let search = search.get().to_lowercase();
        ordered_section_keys(keys)
            .into_iter()
            .filter(|key| key.to_lowercase().contains(&search))
            .collect::<Vec<_>>()
    });
    let all = ui::action(ui::label_for(locale, "All settings"), move || {
        ui::set(section, None);
        crate::utils::deep_link::replace_url("/config");
    });
    let sidebar = col().class("config-sidebar").fill((
        col().class("config-sidebar__header").fill(
            text()
                .class("page-title")
                .kind(TextKind::Heading(1))
                .content(ui::label_for(locale, "Settings")),
        ),
        col().class("config-sidebar__search").fill(ui::field(
            ui::label_for(locale, "Search settings"),
            search,
            false,
        )),
        col().class("config-nav").fill((
            all,
            each(
                section_names,
                |name| name.clone(),
                move |name, _| {
                    let key = name.get_untracked();
                    let read = key.clone();
                    let label = key.replace('_', " ");
                    sapid::button()
                        .unstyled()
                        .class(section.map(move |current| {
                            if current.as_deref() == Some(read.as_str()) {
                                "btn-plain config-nav-item config-nav-item--active".into()
                            } else {
                                "btn-plain config-nav-item".into()
                            }
                        }))
                        .label(ui::label_for(locale, label))
                        .on_press(move || {
                            ui::set(section, Some(key.clone()));
                            crate::utils::deep_link::replace_url(&format!("/config/{key}"));
                        })
                },
            ),
        )),
    ));
    Box::new(
        row()
            .class("gateway-page management-page config-page config-layout")
            .fill((
                sidebar,
                col().class("config-main").fill((
                    row().class("config-actions").fill(actions),
                    col().class("config-scroll").fill(children),
                )),
            )),
    )
}
fn ordered_section_keys(keys: BTreeSet<String>) -> Vec<String> {
    const ORDER: &[&str] = &[
        "env",
        "environment",
        "updates",
        "agents",
        "auth",
        "channels",
        "messages",
        "commands",
        "hooks",
        "skills",
        "tools",
        "gateway",
        "logging",
        "browser",
        "models",
        "audio",
        "memory",
    ];
    let mut keys = keys.into_iter().collect::<Vec<_>>();
    keys.sort_by_key(|key| {
        (
            ORDER
                .iter()
                .position(|section| *section == key.as_str())
                .unwrap_or(ORDER.len()),
            key.clone(),
        )
    });
    keys
}
fn pointer(path: &[String]) -> String {
    path.iter()
        .map(|part| format!("/{}", part.replace('~', "~0").replace('/', "~1")))
        .collect()
}
fn write_path(document: &mut Value, path: &[String], value: Value) {
    if path.is_empty() {
        *document = value;
        return;
    }
    if !document.is_object() {
        *document = json!({});
    }
    if path.len() == 1 {
        document[&path[0]] = value;
    } else {
        write_path(&mut document[&path[0]], &path[1..], value);
    }
}
fn update(draft: Cage<Value>, path: &[String], value: Value) {
    let mut document = draft.get();
    write_path(&mut document, path, value);
    ui::set(draft, document);
}
fn field(
    ctx: &AppContext,
    draft: Cage<Value>,
    path: Vec<String>,
    schema: &Value,
    root_schema: &Value,
    error: Cage<String>,
    depth: usize,
) -> Box<dyn Widget> {
    let ptr = pointer(&path);
    let current = draft.get().pointer(&ptr).cloned().unwrap_or(Value::Null);
    let schema = if let Some(reference) = schema["$ref"].as_str() {
        reference
            .strip_prefix('#')
            .and_then(|p| root_schema.pointer(p))
            .cloned()
            .unwrap_or_else(|| schema.clone())
    } else {
        schema.clone()
    };
    let schema = resolve_schema_variant(&schema, Some(&current));
    let schema = merge_schema_with_value(&schema, &current);
    let inferred = infer_schema_from_value(&current);
    let kind = schema_primary_type(&schema)
        .or_else(|| schema_primary_type(&inferred))
        .unwrap_or_else(|| "string".to_owned());
    let name = schema["title"]
        .as_str()
        .unwrap_or_else(|| path.last().map(String::as_str).unwrap_or("Configuration"))
        .to_owned();
    let description = schema["description"].as_str().unwrap_or("").to_owned();
    if kind == "object" && depth < 8 {
        let mut children = vec![paragraph(description)];
        let properties = schema["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        for (key, schema) in properties {
            let mut child = path.clone();
            child.push(key);
            children.push(field(
                ctx,
                draft,
                child,
                &schema,
                root_schema,
                error,
                depth + 1,
            ));
        }
        if let Some(object) = current.as_object() {
            for (key, value) in object {
                if schema["properties"].get(key).is_none() {
                    let mut child = path.clone();
                    child.push(key.clone());
                    children.push(field(
                        ctx,
                        draft,
                        child,
                        &infer_schema_from_value(value),
                        root_schema,
                        error,
                        depth + 1,
                    ));
                }
            }
        }
        if children.len() == 1 || schema.get("additionalProperties").is_some() {
            children.push(json_field(ctx, draft, path, schema, error, name.clone()));
        }
        return card(name, children);
    }
    if kind == "boolean" {
        let bind_ptr = ptr.clone();
        return Box::new(
            sapid::checkbox()
                .class("form-checkbox")
                .label(name)
                .checked(draft.map(move |v| {
                    v.pointer(&bind_ptr)
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                }))
                .on_checked_change(move |value| update(draft, &path, json!(value))),
        );
    }
    if matches!(kind.as_str(), "array" | "object") {
        return json_field(ctx, draft, path, schema, error, name);
    }
    let bound_ptr = ptr.clone();
    let value = draft.map(move |v| match v.pointer(&bound_ptr) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(value) => value.to_string(),
    });
    if let Some(options) = schema["enum"].as_array() {
        let values = std::rc::Rc::new(options.clone());
        let selection_values = values.clone();
        let options = options
            .iter()
            .enumerate()
            .map(|(i, value)| {
                sapid::select_option(
                    format!("option-{i}"),
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                )
                .value(value.to_string())
            })
            .collect::<Vec<_>>();
        let bind_ptr = ptr.clone();
        return Box::new(
            crate::ui::select_control(select_key(&format!("config-enum-{}", path.join("/"))))
                .class("form-select")
                .label(name)
                .value(draft.map(move |document| {
                    document
                        .pointer(&bind_ptr)
                        .and_then(|current| {
                            selection_values.iter().position(|value| value == current)
                        })
                        .map(|index| format!("option-{index}"))
                }))
                .options(options)
                .on_value_change(move |key| {
                    if let Some(value) = key
                        .and_then(|key| {
                            key.strip_prefix("option-")
                                .and_then(|index| index.parse::<usize>().ok())
                        })
                        .and_then(|index| values.get(index))
                    {
                        update(draft, &path, value.clone());
                    }
                }),
        );
    }
    let input = sapid::text_field()
        .unstyled()
        .class("form-input")
        .name(name.clone())
        .value(value)
        .on_value_change(move |text| {
            let value = match kind.as_str() {
                "integer" => match text.parse::<i64>() {
                    Ok(value) => json!(value),
                    Err(_) => {
                        ui::set(error, "Expected an integer".to_owned());
                        return;
                    }
                },
                "number" => match text.parse::<f64>().ok().filter(|v| v.is_finite()) {
                    Some(value) => json!(value),
                    None => {
                        ui::set(error, "Expected a finite number".to_owned());
                        return;
                    }
                },
                _ => json!(text),
            };
            if let Some(message) = field_validation_error(&schema, Some(&value)) {
                ui::set(error, message);
                return;
            }
            ui::set(error, String::new());
            update(draft, &path, value);
        });
    Box::new(col().class("form-field").fill((
        text().class("form-label").content(name),
        input,
        text().class("form-help").content(description),
    )))
}
fn json_field(
    ctx: &AppContext,
    draft: Cage<Value>,
    path: Vec<String>,
    schema: Value,
    error: Cage<String>,
    name: String,
) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let ptr = pointer(&path);
    let initial = draft.get().pointer(&ptr).cloned().unwrap_or_else(|| {
        if schema_primary_type(&schema).as_deref() == Some("array") {
            json!([])
        } else {
            json!({})
        }
    });
    let input = ctx
        .scope
        .cage(serde_json::to_string_pretty(&initial).unwrap_or_default());
    Box::new(col().class("form-field").fill((
        ui::field(name, input, true),
        ui::action(
            ui::label_for(locale, "Update field"),
            move || match serde_json::from_str::<Value>(&input.get()) {
                Ok(value) => {
                    if let Some(message) = field_validation_error(&schema, Some(&value)) {
                        ui::set(error, message);
                    } else {
                        update(draft, &path, value);
                        ui::set(error, String::new());
                    }
                }
                Err(message) => ui::set(error, message.to_string()),
            },
        ),
    )))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_writes_preserve_sibling_settings() {
        let mut value = json!({"gateway":{"port":18881,"host":"localhost"},"model":"kept"});
        write_path(&mut value, &["gateway".into(), "port".into()], json!(18882));
        assert_eq!(
            value,
            json!({"gateway":{"port":18882,"host":"localhost"},"model":"kept"})
        );
    }
    #[test]
    fn escaped_json_pointer_segments_roundtrip() {
        let path = vec!["provider/foo".to_owned(), "a~b".to_owned()];
        let mut value = json!({});
        write_path(&mut value, &path, json!(true));
        assert_eq!(value.pointer(&pointer(&path)), Some(&json!(true)));
    }
    #[test]
    fn union_schemas_keep_nullable_numbers() {
        let schema = json!({"anyOf":[{"type":"null"},{"type":"number"}]});
        assert!(schema_is_nullable(&schema));
        assert_eq!(
            schema_primary_type(&resolve_schema_variant(&schema, Some(&json!(1)))),
            Some("number".to_owned())
        );
    }
    #[test]
    fn enum_controls_accept_unicode_and_separator_property_names() {
        let html = glory::ssr::render_with(|scope| {
            let context = AppContext::new(scope);
            let draft = scope.cage(json!({"provider/私有":"fast"}));
            let error = scope.cage(String::new());
            field(
                &context,
                draft,
                vec!["provider/私有".to_owned()],
                &json!({"type":"string","enum":["fast","slow"]}),
                &json!({}),
                error,
                0,
            )
        })
        .expect("configuration keys must not become semantic IDs");
        assert!(html.contains("fast"));
        assert!(html.contains("slow"));
    }
}
