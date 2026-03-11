use std::collections::HashSet;

use serde_json::{Value, json};

fn trim_nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn canonical_provider_or_empty(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        crate::canonical_provider_id(trimmed)
    }
}

fn nonempty_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).and_then(trim_nonempty)
}

fn model_item_field(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| nonempty_string(item.get(*key)))
}

fn model_item_flag(item: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| item.get(*key).and_then(Value::as_bool))
}

fn normalize_reasoning_presets(raw: &Value) -> Option<Value> {
    let options = raw.as_array()?;
    let mut normalized = Vec::with_capacity(options.len());
    for option in options {
        if let Some(effort) = option.as_str().and_then(trim_nonempty) {
            normalized.push(json!({
                "effort": effort,
                "description": "",
            }));
            continue;
        }

        let Some(obj) = option.as_object() else {
            continue;
        };
        let effort = obj
            .get("effort")
            .and_then(Value::as_str)
            .and_then(trim_nonempty)
            .or_else(|| {
                obj.get("reasoning_effort")
                    .and_then(Value::as_str)
                    .and_then(trim_nonempty)
            })
            .or_else(|| {
                obj.get("reasoningEffort")
                    .and_then(Value::as_str)
                    .and_then(trim_nonempty)
            });
        let Some(effort) = effort else {
            continue;
        };

        let description = obj
            .get("description")
            .and_then(Value::as_str)
            .and_then(trim_nonempty)
            .unwrap_or_default();
        normalized.push(json!({
            "effort": effort,
            "description": description,
        }));
    }

    Some(Value::Array(normalized))
}

fn extract_models_array(payload: &Value) -> Option<&Vec<Value>> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.get("models").and_then(Value::as_array))
        .or_else(|| payload.as_array())
}

pub fn parse_remote_models(payload: &Value, provider_hint: &str) -> Vec<Value> {
    let models = match extract_models_array(payload) {
        Some(items) => items,
        None => return Vec::new(),
    };

    let hint = canonical_provider_or_empty(provider_hint);
    let mut parsed = Vec::new();
    let mut seen = HashSet::new();

    for item in models {
        let (raw_id, name, provider_from_item, is_default) = if let Some(id) =
            model_item_field(item, &["id", "model", "model_id", "model_slug", "slug"])
        {
            (
                id,
                model_item_field(item, &["name", "display_name", "displayName", "label"]),
                model_item_field(item, &["provider", "provider_id", "providerId"]),
                model_item_flag(item, &["is_default", "isDefault"]).unwrap_or(false),
            )
        } else if let Some(id) = item.as_str().and_then(trim_nonempty) {
            (id, None, None, false)
        } else {
            continue;
        };

        let mut provider = provider_from_item
            .map(|value| canonical_provider_or_empty(&value))
            .unwrap_or_default();
        let mut model_slug = raw_id.clone();
        if let Some((provider_prefix, suffix)) = crate::parse_provider_prefixed_model(&raw_id) {
            if provider.is_empty() {
                provider = canonical_provider_or_empty(provider_prefix);
            }
            model_slug = suffix.to_string();
        } else if provider.is_empty() {
            provider = hint.clone();
        }

        if model_slug.is_empty() {
            continue;
        }

        let model_id = if raw_id.contains('/') || provider.is_empty() {
            raw_id
        } else {
            format!("{provider}/{model_slug}")
        };

        if !seen.insert(model_id.clone()) {
            continue;
        }

        let mut entry = item.as_object().cloned().unwrap_or_default();
        entry.insert("id".to_string(), json!(model_id));
        entry.insert(
            "name".to_string(),
            json!(name.unwrap_or_else(|| model_slug.clone())),
        );
        entry.insert("model_slug".to_string(), json!(model_slug));
        entry.insert("is_default".to_string(), json!(is_default));
        entry.insert("builtin".to_string(), json!(true));
        if !provider.is_empty() {
            entry.insert("provider".to_string(), json!(provider));
        }

        if !entry.contains_key("display_name")
            && let Some(value) = entry.get("displayName").cloned()
        {
            entry.insert("display_name".to_string(), value);
        }
        if !entry.contains_key("default_reasoning_effort")
            && let Some(value) = entry.get("defaultReasoningEffort").cloned()
        {
            entry.insert("default_reasoning_effort".to_string(), value);
        }
        if !entry.contains_key("default_reasoning_level")
            && let Some(value) = entry.get("defaultReasoningLevel").cloned()
        {
            entry.insert("default_reasoning_level".to_string(), value);
        }
        if !entry.contains_key("supports_personality")
            && let Some(value) = entry.get("supportsPersonality").cloned()
        {
            entry.insert("supports_personality".to_string(), value);
        }
        if !entry.contains_key("supported_in_api")
            && let Some(value) = entry.get("supportedInApi").cloned()
        {
            entry.insert("supported_in_api".to_string(), value);
        }
        if !entry.contains_key("input_modalities")
            && let Some(value) = entry.get("inputModalities").cloned()
        {
            entry.insert("input_modalities".to_string(), value);
        }

        let normalized_reasoning = entry
            .get("supported_reasoning_levels")
            .cloned()
            .or_else(|| entry.get("supported_reasoning_efforts").cloned())
            .or_else(|| entry.get("supportedReasoningLevels").cloned())
            .or_else(|| entry.get("supportedReasoningEfforts").cloned())
            .and_then(|raw| normalize_reasoning_presets(&raw));
        if let Some(presets) = normalized_reasoning {
            entry.insert("supported_reasoning_levels".to_string(), presets.clone());
            entry.insert("supported_reasoning_efforts".to_string(), presets);
        }

        let visibility_hidden = entry
            .get("visibility")
            .and_then(Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case("hide"));
        if visibility_hidden
            && !entry.contains_key("is_disabled")
            && !entry.contains_key("disabled")
        {
            entry.insert("is_disabled".to_string(), json!(true));
        }

        parsed.push(Value::Object(entry));
    }

    parsed
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::parse_remote_models;

    #[test]
    fn parse_remote_models_reads_openai_shape() {
        let payload = json!({
            "data": [
                { "id": "gpt-5.1", "name": "GPT-5.1", "is_default": true },
                { "id": "gpt-4.1", "name": "GPT-4.1" }
            ]
        });

        let parsed = parse_remote_models(&payload, "openai");
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0].get("id").and_then(Value::as_str),
            Some("openai/gpt-5.1")
        );
        assert_eq!(
            parsed[1].get("id").and_then(Value::as_str),
            Some("openai/gpt-4.1")
        );
    }

    #[test]
    fn parse_remote_models_reads_slug_shape() {
        let payload = json!({
            "models": [
                { "slug": "gpt-5", "name": "GPT-5", "is_default": true }
            ]
        });

        let parsed = parse_remote_models(&payload, "openai");
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].get("id").and_then(Value::as_str),
            Some("openai/gpt-5")
        );
        assert_eq!(parsed[0].get("name").and_then(Value::as_str), Some("GPT-5"));
    }

    #[test]
    fn parse_remote_models_normalizes_reasoning_metadata() {
        let payload = json!({
            "data": [
                {
                    "id": "gpt-5.3-codex",
                    "displayName": "gpt-5.3-codex",
                    "isDefault": true,
                    "defaultReasoningEffort": "medium",
                    "supportedReasoningEfforts": [
                        {
                            "reasoningEffort": "low",
                            "description": "Fast responses with lighter reasoning"
                        },
                        {
                            "reasoningEffort": "xhigh",
                            "description": "Extra high reasoning depth for complex problems"
                        }
                    ],
                    "inputModalities": ["text", "image"],
                    "supportsPersonality": true,
                    "supportedInApi": true
                }
            ]
        });

        let parsed = parse_remote_models(&payload, "openai");
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].get("id").and_then(Value::as_str),
            Some("openai/gpt-5.3-codex")
        );
        assert_eq!(
            parsed[0]
                .get("default_reasoning_effort")
                .and_then(Value::as_str),
            Some("medium")
        );
        assert_eq!(
            parsed[0]
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("effort"))
                .and_then(Value::as_str),
            Some("low")
        );
        assert_eq!(
            parsed[0]
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .and_then(|items| items.get(1))
                .and_then(|item| item.get("effort"))
                .and_then(Value::as_str),
            Some("xhigh")
        );
        assert_eq!(
            parsed[0]
                .get("input_modalities")
                .and_then(Value::as_array)
                .map(|values| values.len()),
            Some(2)
        );
        assert_eq!(
            parsed[0]
                .get("supports_personality")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            parsed[0].get("supported_in_api").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            parsed[0].get("is_default").and_then(Value::as_bool),
            Some(true)
        );
    }
}
