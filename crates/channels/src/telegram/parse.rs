use savfox_core::channel::ChannelAction;
use serde_json::Value;

fn split_telegram_command(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let (head, rest) = if let Some(idx) = trimmed.find(char::is_whitespace) {
        let (head, tail) = trimmed.split_at(idx);
        (head, tail.trim())
    } else {
        (trimmed, "")
    };
    let command = head
        .trim_start_matches('/')
        .split('@')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if command.is_empty() {
        return None;
    }
    Some((command, rest.to_string()))
}

fn normalize_registry_command_with_resolver<F>(text: &str, resolve_command_name: &F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    let (command, args) = split_telegram_command(text)?;
    let canonical = resolve_command_name(&command)?;
    let mut prompt = format!("/{canonical}");
    let args = args.trim();
    if !args.is_empty() {
        prompt.push(' ');
        prompt.push_str(args);
    }
    Some(prompt)
}

pub fn parse_update(payload: &Value) -> anyhow::Result<ChannelAction> {
    parse_update_with_resolver(payload, |_| None)
}

pub fn parse_update_with_resolver<F>(
    payload: &Value,
    resolve_command_name: F,
) -> anyhow::Result<ChannelAction>
where
    F: Fn(&str) -> Option<String>,
{
    let message = payload.get("message").unwrap_or(&Value::Null);
    let text = message.get("text").and_then(Value::as_str).unwrap_or("");

    let chat_id = message
        .get("chat")
        .and_then(|c| c.get("id"))
        .and_then(Value::as_i64)
        .map(|id| id.to_string())
        .unwrap_or_default();

    if let Some((command, args)) = split_telegram_command(text) {
        if command == "savfox" {
            let prompt = args.trim().to_string();
            if prompt.is_empty() {
                return Ok(ChannelAction::Ignore);
            }
            return Ok(ChannelAction::StartThread {
                channel: chat_id,
                prompt,
            });
        }

        if let Some(prompt) =
            normalize_registry_command_with_resolver(text, &resolve_command_name)
        {
            return Ok(ChannelAction::StartThread {
                channel: chat_id,
                prompt,
            });
        }
    }

    if let Some(callback) = payload.get("callback_query") {
        let data = callback.get("data").and_then(Value::as_str).unwrap_or("");

        if let Some(thread_id) = data.strip_prefix("approve:") {
            return Ok(ChannelAction::Approve {
                thread_id: thread_id.to_owned(),
                decision: true,
            });
        }
        if let Some(thread_id) = data.strip_prefix("deny:") {
            return Ok(ChannelAction::Approve {
                thread_id: thread_id.to_owned(),
                decision: false,
            });
        }
    }

    Ok(ChannelAction::Ignore)
}

pub fn parse_display_name(payload: &Value) -> Option<String> {
    let from = payload.pointer("/message/from").unwrap_or(&Value::Null);
    let first = from
        .get("first_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let last = from
        .get("last_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let username = from
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if !first.is_empty() || !last.is_empty() {
        let full = format!("{first} {last}").trim().to_string();
        if !full.is_empty() {
            return Some(full);
        }
    }
    username.map(str::to_string)
}

#[cfg(test)]
mod tests {
    use savfox_core::channel::ChannelAction;
    use serde_json::json;

    use super::parse_update_with_resolver;

    fn resolve_command_name(command: &str) -> Option<String> {
        match command {
            "commands" => Some("commands".to_string()),
            "status" => Some("status".to_string()),
            _ => None,
        }
    }

    #[test]
    fn supports_commands_alias_surface() {
        let payload = json!({
            "message": {
                "text": "/commands",
                "chat": { "id": 42 }
            }
        });

        let action = parse_update_with_resolver(&payload, resolve_command_name).expect("parse");
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "42");
                assert_eq!(prompt, "/commands");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn supports_bot_qualified_savfox_command() {
        let payload = json!({
            "message": {
                "text": "/savfox@mybot summarize this",
                "chat": { "id": 42 }
            }
        });

        let action = parse_update_with_resolver(&payload, resolve_command_name).expect("parse");
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "42");
                assert_eq!(prompt, "summarize this");
            }
            _ => panic!("expected start thread action"),
        }
    }
}
