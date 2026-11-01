use std::collections::HashMap;

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

mod browser;
mod editor;

pub fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    editor::render(ctx, path)
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

fn pointer(path: &[String]) -> String {
    if path.is_empty() {
        return String::new();
    }
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
            let error = scope.cage(HashMap::new());
            editor::field(
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
