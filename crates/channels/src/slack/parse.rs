use savfox_core::channel::ChannelAction;
use serde_json::{Map, Value};

fn split_head_and_tail(s: &str) -> (&str, &str) {
    if let Some(idx) = s.find(char::is_whitespace) {
        let (head, tail) = s.split_at(idx);
        (head, tail.trim())
    } else {
        (s, "")
    }
}

fn normalize_command_prompt_with_resolver<F>(text: &str, resolve_command_name: &F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let (head, tail) = split_head_and_tail(trimmed);
    let canonical = resolve_command_name(head)?;
    let mut prompt = format!("/{canonical}");
    if !tail.is_empty() {
        prompt.push(' ');
        prompt.push_str(tail);
    }
    Some(prompt)
}

fn slash_command_prompt_with_resolver<F>(
    command: &str,
    text: &str,
    resolve_command_name: &F,
) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    let command = command.trim();
    let text = text.trim();

    if command.eq_ignore_ascii_case("/savfox") {
        if text.is_empty() {
            return None;
        }
        return Some(
            normalize_command_prompt_with_resolver(text, resolve_command_name)
                .unwrap_or_else(|| text.to_string()),
        );
    }

    let canonical = resolve_command_name(command)?;
    let mut prompt = format!("/{canonical}");
    if !text.is_empty() {
        prompt.push(' ');
        prompt.push_str(text);
    }
    Some(prompt)
}

fn extract_mentioned_prompt(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if let Some(prompt) = trimmed.strip_prefix("/savfox ") {
        let prompt = prompt.trim();
        if !prompt.is_empty() {
            return Some(prompt);
        }
    }

    if trimmed.starts_with("<@")
        && let Some(end) = trimmed.find('>')
    {
        let prompt = trimmed[end + 1..].trim();
        if !prompt.is_empty() {
            return Some(prompt);
        }
    }

    None
}

pub fn parse_event(payload: &Value) -> anyhow::Result<ChannelAction> {
    parse_event_with_resolver(payload, |_| None)
}

pub fn parse_event_with_resolver<F>(
    payload: &Value,
    resolve_command_name: F,
) -> anyhow::Result<ChannelAction>
where
    F: Fn(&str) -> Option<String>,
{
    let event_type = payload.get("type").and_then(Value::as_str).unwrap_or("");

    match event_type {
        "url_verification" => Ok(ChannelAction::Ignore),
        "event_callback" => {
            let event = payload.get("event").unwrap_or(&Value::Null);
            let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");

            match event_type {
                "app_mention" | "message" => {
                    let text = event.get("text").and_then(Value::as_str).unwrap_or("");
                    let channel = event
                        .get("channel")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();

                    let Some(prompt) = extract_mentioned_prompt(text) else {
                        return Ok(ChannelAction::Ignore);
                    };

                    if let Some(command_prompt) =
                        normalize_command_prompt_with_resolver(prompt, &resolve_command_name)
                    {
                        Ok(ChannelAction::StartThread {
                            channel,
                            prompt: command_prompt,
                        })
                    } else {
                        Ok(ChannelAction::StartThread {
                            channel,
                            prompt: prompt.to_string(),
                        })
                    }
                }
                _ => Ok(ChannelAction::Ignore),
            }
        }
        _ => Ok(ChannelAction::Ignore),
    }
}

pub fn parse_slash_command(payload: &Value) -> anyhow::Result<ChannelAction> {
    parse_slash_command_with_resolver(payload, |_| None)
}

pub fn parse_slash_command_with_resolver<F>(
    payload: &Value,
    resolve_command_name: F,
) -> anyhow::Result<ChannelAction>
where
    F: Fn(&str) -> Option<String>,
{
    let command = payload.get("command").and_then(Value::as_str).unwrap_or("");
    let text = payload.get("text").and_then(Value::as_str).unwrap_or("");

    let channel = payload
        .get("channel_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    if let Some(prompt) = slash_command_prompt_with_resolver(command, text, &resolve_command_name) {
        Ok(ChannelAction::StartThread { channel, prompt })
    } else {
        Ok(ChannelAction::Ignore)
    }
}

pub fn parse_payload_bytes(body: &[u8]) -> anyhow::Result<Value> {
    if let Ok(json_value) = serde_json::from_slice::<Value>(body) {
        return Ok(json_value);
    }

    let mut map = Map::new();
    for (key, value) in form_urlencoded::parse(body).into_owned() {
        map.insert(key, Value::String(value));
    }
    if map.is_empty() {
        anyhow::bail!("unsupported payload format");
    }
    Ok(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use savfox_core::channel::ChannelAction;
    use serde_json::json;

    use super::{parse_event_with_resolver, parse_slash_command_with_resolver};

    fn resolve_command_name(command: &str) -> Option<String> {
        match command.trim_start_matches('/') {
            "status" => Some("status".to_string()),
            "help" => Some("help".to_string()),
            _ => None,
        }
    }

    #[test]
    fn slash_command_supports_native_registry_command() {
        let payload = json!({
            "command": "/status",
            "channel_id": "C123",
            "text": ""
        });

        let action =
            parse_slash_command_with_resolver(&payload, resolve_command_name).expect("parse");
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "C123");
                assert_eq!(prompt, "/status");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn savfox_slash_command_accepts_command_text() {
        let payload = json!({
            "command": "/savfox",
            "channel_id": "C123",
            "text": "/help"
        });

        let action =
            parse_slash_command_with_resolver(&payload, resolve_command_name).expect("parse");
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "C123");
                assert_eq!(prompt, "/help");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn message_event_requires_leading_bot_mention() {
        let payload = json!({
            "type": "event_callback",
            "event": {
                "type": "message",
                "channel": "C123",
                "text": "not a bot mention > just some text"
            }
        });

        let action = parse_event_with_resolver(&payload, resolve_command_name).expect("parse");
        assert_eq!(action, ChannelAction::Ignore);
    }

    #[test]
    fn app_mention_routes_plain_prompt() {
        let payload = json!({
            "type": "event_callback",
            "event": {
                "type": "app_mention",
                "channel": "C123",
                "text": "<@U123> summarize this"
            }
        });

        let action = parse_event_with_resolver(&payload, resolve_command_name).expect("parse");
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "C123");
                assert_eq!(prompt, "summarize this");
            }
            _ => panic!("expected start thread action"),
        }
    }
}
