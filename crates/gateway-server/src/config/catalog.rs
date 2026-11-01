//! The configuration editor exposes the configuration consumed by the runtime.
//! Service-owned account, channel, speech and automation stores are edited by
//! their own APIs, never by a second hand-written config schema.
use serde_json::{Value, json};

use super::GatewayConfig;

pub(crate) fn schema() -> Value {
    let mut root: Value = savfox_core::config::schema::config_schema().into();
    let mut gateway: Value = schemars::generate::SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<GatewayConfig>()
        .into();
    if let Some(definitions) = gateway
        .as_object_mut()
        .and_then(|g| g.remove("definitions"))
    {
        root["definitions"]
            .as_object_mut()
            .expect("core definitions")
            .extend(
                definitions
                    .as_object()
                    .expect("gateway definitions")
                    .clone(),
            );
    }
    gateway["title"] = json!("Gateway");
    root["properties"]["gateway"] = gateway;
    root
}

fn resolved<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix('#'))
        .and_then(|pointer| root.pointer(pointer))
        .unwrap_or(schema)
}

fn inspect(
    value: &mut Value,
    schema: &Value,
    root: &Value,
    path: &str,
    removed: &mut Vec<String>,
    prune: bool,
) {
    let schema = resolved(schema, root);
    if let Some(variants) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
    {
        if let Some(variant) = variants.iter().map(|s| resolved(s, root)).find(|s| {
            match s.get("type").and_then(Value::as_str) {
                Some("object") => value.is_object(),
                Some("array") => value.is_array(),
                Some("string") => value.is_string(),
                Some("null") => value.is_null(),
                Some("boolean") => value.is_boolean(),
                Some("number" | "integer") => value.is_number(),
                _ => false,
            }
        }) {
            inspect(value, variant, root, path, removed, prune);
        }
        return;
    }
    if let Some(object) = value.as_object_mut() {
        if !prune {
            for key in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !object.contains_key(key) {
                    removed.push(format!("{path}.{key}: required field"));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        let additional = schema.get("additionalProperties");
        let keys = object.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let field = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            let child_schema = properties
                .and_then(|props| props.get(&key))
                .or_else(|| additional.filter(|s| s.is_object()));
            if let Some(child_schema) = child_schema {
                inspect(
                    object.get_mut(&key).expect("existing key"),
                    child_schema,
                    root,
                    &field,
                    removed,
                    prune,
                );
            } else if additional == Some(&Value::Bool(false)) {
                removed.push(field);
                if prune {
                    object.remove(&key);
                }
            }
        }
    } else if let Some(items) = value.as_array_mut() {
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in items.iter_mut().enumerate() {
                inspect(
                    item,
                    item_schema,
                    root,
                    &format!("{path}[{index}]"),
                    removed,
                    prune,
                );
            }
        }
    } else if !prune {
        if let Some(options) = schema.get("enum").and_then(Value::as_array)
            && !options.contains(value)
        {
            removed.push(format!("{path}: unsupported value"));
        }
        if let Some(number) = value.as_f64() {
            if schema
                .get("minimum")
                .and_then(Value::as_f64)
                .is_some_and(|min| number < min)
            {
                removed.push(format!("{path}: below minimum {}", schema["minimum"]));
            }
            if schema
                .get("maximum")
                .and_then(Value::as_f64)
                .is_some_and(|max| number > max)
            {
                removed.push(format!("{path}: above maximum {}", schema["maximum"]));
            }
        }
    }
}

/// Remove unsupported/obsolete fields; no aliases or migration are applied.
pub(crate) fn clean(value: &mut Value) -> Vec<String> {
    let schema = schema();
    let mut removed = Vec::new();
    if value.get("model").is_some_and(|model| {
        !model.is_null()
            && serde_json::from_value::<savfox_core::config::SelectedModel>(model.clone()).is_err()
    }) {
        value
            .as_object_mut()
            .expect("model belongs to object")
            .remove("model");
        removed.push("model".into());
    }
    inspect(value, &schema, &schema, "", &mut removed, true);
    removed
}

pub(crate) fn validate(value: &Value) -> Result<(), String> {
    if !value.is_object() {
        return Err("Configuration must be an object".into());
    }
    let schema = schema();
    let mut unknown = Vec::new();
    inspect(
        &mut value.clone(),
        &schema,
        &schema,
        "",
        &mut unknown,
        false,
    );
    if !unknown.is_empty() {
        return Err(format!(
            "Invalid or unsupported configuration fields: {}",
            unknown.join(", ")
        ));
    }
    let mut core = value.clone();
    if let Some(gateway) = core
        .as_object_mut()
        .and_then(|object| object.remove("gateway"))
    {
        let gateway: GatewayConfig =
            serde_json::from_value(gateway).map_err(|e| format!("gateway: {e}"))?;
        if gateway.port == 0
            || gateway.rate_limit.max_requests == 0
            || gateway.rate_limit.window_secs == 0
            || gateway.rate_limit.max_connections_per_ip == 0
        {
            return Err("Gateway port and rate limits must be greater than zero".into());
        }
        if gateway.tls_cert.is_some() != gateway.tls_key.is_some() {
            return Err("TLS certificate and key must be configured together".into());
        }
    }
    serde_json::from_value::<savfox_core::config::ConfigToml>(core)
        .map_err(|e| format!("Configuration: {e}"))?;
    let checks = super::validator::validate_config(
        &json!({"gateway":value.get("gateway").cloned().unwrap_or(json!({}))}),
    );
    let errors = checks
        .errors
        .into_iter()
        .filter(|e| e.severity == super::validator::Severity::Error)
        .map(|e| format!("{}: {}", e.field, e.message))
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    #[test]
    fn removes_unconsumed_sections_and_legacy_gateway_bridges() {
        let mut config = json!({"auth":{"cooldowns":{}},"canvasHost":{},"models":{},"gateway":{"port":18881,"bridges":{}},"model_context_window":64000});
        let removed = clean(&mut config);
        assert_eq!(
            config,
            json!({"gateway":{"port":18881},"model_context_window":64000})
        );
        assert_eq!(removed.len(), 4);
        validate(&config).unwrap();
    }
    #[test]
    fn rejects_invalid_and_unknown_fields_but_accepts_runtime_maps() {
        for config in [
            json!({"auth":{}}),
            json!({"gateway":{"port":0}}),
            json!({"gateway":{"port":"bad"}}),
            json!({"gateway":{"rate_limit":{"window_secs":0}}}),
            json!({"gateway":{"tls_key":"key"}}),
            json!({"agents":{"max_sessions":0}}),
        ] {
            assert!(validate(&config).is_err(), "{config}");
        }
        validate(&json!({"mcp_servers":{"example":{"command":"server"}},"gateway":{"port":18881}}))
            .unwrap();
    }
}
