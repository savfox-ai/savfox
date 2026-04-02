use std::collections::HashMap;

pub type TemplateContext = HashMap<String, serde_json::Value>;

fn format_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                serde_json::Value::Bool(b) => Some(b.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::Object(_) => String::new(),
    }
}

#[must_use]
pub fn apply_template(template: &str, ctx: &TemplateContext) -> String {
    let mut result = template.to_owned();
    let mut start = 0;

    while let Some(open) = result[start..].find("{{") {
        let open_pos = start + open;
        if let Some(close) = result[open_pos..].find("}}") {
            let close_pos = open_pos + close + 2;
            let key = result[open_pos + 2..close_pos - 2].trim();

            if let Some(value) = ctx.get(key) {
                let replacement = format_value(value);
                result.replace_range(open_pos..close_pos, &replacement);
                start = open_pos + replacement.len();
            } else {
                start = close_pos;
            }
        } else {
            break;
        }
    }

    result
}

pub fn apply_template_with_defaults(
    template: &str,
    ctx: &TemplateContext,
    defaults: &TemplateContext,
) -> String {
    let mut combined = defaults.clone();
    for (k, v) in ctx {
        combined.insert(k.clone(), v.clone());
    }
    apply_template(template, &combined)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_simple_substitution() {
        let mut ctx = TemplateContext::new();
        ctx.insert("name".to_owned(), json!("Alice"));
        ctx.insert("count".to_owned(), json!(42));

        let result = apply_template("Hello, {{name}}! You have {{count}} messages.", &ctx);
        assert_eq!(result, "Hello, Alice! You have 42 messages.");
    }

    #[test]
    fn test_missing_key() {
        let ctx = TemplateContext::new();
        let result = apply_template("Hello, {{name}}!", &ctx);
        assert_eq!(result, "Hello, {{name}}!");
    }

    #[test]
    fn test_array_formatting() {
        let mut ctx = TemplateContext::new();
        ctx.insert("items".to_owned(), json!(["apple", "banana", "cherry"]));

        let result = apply_template("Items: {{items}}", &ctx);
        assert_eq!(result, "Items: apple, banana, cherry");
    }

    #[test]
    fn test_whitespace_in_placeholder() {
        let mut ctx = TemplateContext::new();
        ctx.insert("name".to_owned(), json!("Bob"));

        let result = apply_template("Hello, {{  name  }}!", &ctx);
        assert_eq!(result, "Hello, Bob!");
    }
}
