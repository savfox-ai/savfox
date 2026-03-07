use savfox_core::channel::ChannelAction;
use serde_json::Value;

pub fn quote_discord_arg(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_string();
    }
    if value
        .chars()
        .all(|ch| !ch.is_whitespace() && ch != '"' && ch != '\\')
    {
        return value.to_string();
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub fn append_discord_option_parts(option: &Value, out: &mut Vec<String>) {
    if let Some(nested) = option.get("options").and_then(Value::as_array) {
        for child in nested {
            append_discord_option_parts(child, out);
        }
    }

    let Some(name) = option.get("name").and_then(Value::as_str) else {
        return;
    };
    let Some(value) = option.get("value") else {
        return;
    };
    match value {
        Value::Bool(true) => out.push(format!("--{name}")),
        Value::Bool(false) => {}
        Value::Number(n) => out.push(format!("--{name} {n}")),
        Value::String(s) => {
            let value = s.trim();
            if !value.is_empty() {
                out.push(format!("--{name} {}", quote_discord_arg(value)));
            }
        }
        _ => {}
    }
}

pub fn parse_savfox_prompt(data: &Value) -> Option<String> {
    let prompt = data
        .get("options")
        .and_then(Value::as_array)
        .and_then(|opts| opts.first())
        .and_then(|opt| opt.get("value"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if prompt.is_empty() {
        None
    } else {
        Some(prompt)
    }
}

pub fn build_command_prompt(command_name: &str, data: &Value) -> Option<String> {
    let command_name = command_name.trim();
    if command_name.is_empty() {
        return None;
    }

    let mut prompt = format!("/{command_name}");
    let mut parts = Vec::new();
    if let Some(options) = data.get("options").and_then(Value::as_array) {
        for option in options {
            append_discord_option_parts(option, &mut parts);
        }
    }
    if !parts.is_empty() {
        prompt.push(' ');
        prompt.push_str(&parts.join(" "));
    }

    Some(prompt)
}

pub fn parse_interaction(payload: &Value) -> anyhow::Result<ChannelAction> {
    parse_interaction_with_resolver(payload, |_, _| None)
}

pub fn parse_interaction_with_resolver<F>(
    payload: &Value,
    resolve_command_prompt: F,
) -> anyhow::Result<ChannelAction>
where
    F: Fn(&str, &Value) -> Option<String>,
{
    let interaction_type = payload.get("type").and_then(Value::as_u64).unwrap_or(0);

    match interaction_type {
        1 => Ok(ChannelAction::Ignore),
        2 => {
            let data = payload.get("data").unwrap_or(&Value::Null);
            let command_name = data.get("name").and_then(Value::as_str).unwrap_or("");
            let channel = payload
                .get("channel_id")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();

            if command_name.eq_ignore_ascii_case("savfox") {
                if let Some(prompt) = parse_savfox_prompt(data) {
                    return Ok(ChannelAction::StartThread { channel, prompt });
                }
                return Ok(ChannelAction::Ignore);
            }

            if let Some(prompt) = resolve_command_prompt(command_name, data) {
                return Ok(ChannelAction::StartThread { channel, prompt });
            }

            Ok(ChannelAction::Ignore)
        }
        3 => {
            let data = payload.get("data").unwrap_or(&Value::Null);
            let custom_id = data.get("custom_id").and_then(Value::as_str).unwrap_or("");

            if let Some(thread_id) = custom_id.strip_prefix("approve:") {
                Ok(ChannelAction::Approve {
                    thread_id: thread_id.to_owned(),
                    decision: true,
                })
            } else if let Some(thread_id) = custom_id.strip_prefix("deny:") {
                Ok(ChannelAction::Approve {
                    thread_id: thread_id.to_owned(),
                    decision: false,
                })
            } else {
                Ok(ChannelAction::Ignore)
            }
        }
        _ => Ok(ChannelAction::Ignore),
    }
}

#[cfg(test)]
mod tests {
    use savfox_core::channel::ChannelAction;
    use serde_json::json;

    use super::{build_command_prompt, parse_interaction_with_resolver};

    #[test]
    fn parses_native_registry_slash_command() {
        let payload = json!({
            "type": 2,
            "channel_id": "123",
            "data": {
                "name": "status",
            }
        });

        let action = parse_interaction_with_resolver(&payload, |name, data| {
            build_command_prompt(name, data)
        })
        .expect("parse should succeed");
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "123");
                assert_eq!(prompt, "/status");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn keeps_savfox_prompt_flow() {
        let payload = json!({
            "type": 2,
            "channel_id": "123",
            "data": {
                "name": "savfox",
                "options": [
                    { "name": "prompt", "value": "hello world" }
                ]
            }
        });

        let action =
            parse_interaction_with_resolver(&payload, |_, _| None).expect("parse should succeed");
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "123");
                assert_eq!(prompt, "hello world");
            }
            _ => panic!("expected start thread action"),
        }
    }
}
