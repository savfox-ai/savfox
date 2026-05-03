use savfox_core::channel::ChannelAction;
use serde_json::Value;

fn inbound_message(payload: &Value) -> Option<&Value> {
    payload
        .get("message")
        .or_else(|| payload.get("channel_post"))
}

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
    Some((command, rest.to_owned()))
}

fn normalize_registry_command_with_resolver<F>(
    text: &str,
    resolve_command_name: &F,
) -> Option<String>
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
    let message = inbound_message(payload);
    let text = message
        .and_then(|msg| {
            msg.get("text")
                .and_then(Value::as_str)
                .or_else(|| msg.get("caption").and_then(Value::as_str))
        })
        .unwrap_or("");
    let chat_type = message
        .and_then(|msg| msg.get("chat"))
        .and_then(|chat| chat.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let chat_id = message
        .and_then(|msg| msg.get("chat"))
        .and_then(|chat| chat.get("id"))
        .and_then(Value::as_i64)
        .map(|id| id.to_string())
        .unwrap_or_default();

    if let Some((command, args)) = split_telegram_command(text) {
        if command == "savfox" {
            let prompt = args.trim().to_owned();
            if prompt.is_empty() {
                return Ok(ChannelAction::Ignore);
            }
            return Ok(ChannelAction::StartThread {
                channel: chat_id,
                prompt,
            });
        }

        if let Some(prompt) = normalize_registry_command_with_resolver(text, &resolve_command_name)
        {
            return Ok(ChannelAction::StartThread {
                channel: chat_id,
                prompt,
            });
        }

        return Ok(ChannelAction::Ignore);
    }

    if chat_type == "private" {
        let prompt = text.trim();
        if !prompt.is_empty() {
            return Ok(ChannelAction::StartThread {
                channel: chat_id,
                prompt: prompt.to_owned(),
            });
        }
    }

    let prompt = text.trim();
    if !prompt.is_empty() {
        return Ok(ChannelAction::StartThread {
            channel: chat_id,
            prompt: prompt.to_owned(),
        });
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
    let from = inbound_message(payload)
        .and_then(|msg| msg.get("from"))
        .unwrap_or(&Value::Null);
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
        let full = format!("{first} {last}").trim().to_owned();
        if !full.is_empty() {
            return Some(full);
        }
    }
    username
        .map(str::to_owned)
        .or_else(|| {
            inbound_message(payload)
                .and_then(|msg| msg.get("sender_chat"))
                .and_then(|sender| sender.get("title"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            inbound_message(payload)
                .and_then(|msg| msg.get("chat"))
                .and_then(|chat| chat.get("title"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
}

#[cfg(test)]
mod tests {
    use savfox_core::channel::ChannelAction;
    use serde_json::json;

    use super::parse_update_with_resolver;

    fn resolve_command_name(command: &str) -> Option<String> {
        match command {
            "commands" => Some("commands".to_owned()),
            "status" => Some("status".to_owned()),
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

    #[test]
    fn private_chat_plain_text_starts_thread() {
        let payload = json!({
            "message": {
                "text": "summarize this",
                "chat": { "id": 42, "type": "private" }
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

    #[test]
    fn group_plain_text_reaches_runtime() {
        let payload = json!({
            "message": {
                "text": "summarize this",
                "chat": { "id": -10042, "type": "group" }
            }
        });

        let action = parse_update_with_resolver(&payload, resolve_command_name).expect("parse");
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "-10042");
                assert_eq!(prompt, "summarize this");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn unknown_slash_command_is_ignored_in_private_chat() {
        let payload = json!({
            "message": {
                "text": "/help",
                "chat": { "id": 42, "type": "private" }
            }
        });

        let action = parse_update_with_resolver(&payload, resolve_command_name).expect("parse");
        assert!(matches!(action, ChannelAction::Ignore));
    }

    #[test]
    fn channel_post_savfox_command_starts_thread() {
        let payload = json!({
            "channel_post": {
                "text": "/savfox summarize release",
                "chat": { "id": -100123, "type": "channel", "title": "Release Feed" },
                "sender_chat": { "id": -100123, "title": "Release Feed" }
            }
        });

        let action = parse_update_with_resolver(&payload, resolve_command_name).expect("parse");
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "-100123");
                assert_eq!(prompt, "summarize release");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn parse_display_name_falls_back_to_channel_title() {
        let payload = json!({
            "channel_post": {
                "text": "/savfox summarize release",
                "chat": { "id": -100123, "type": "channel", "title": "Release Feed" },
                "sender_chat": { "id": -100123, "title": "Release Feed" }
            }
        });

        assert_eq!(
            super::parse_display_name(&payload).as_deref(),
            Some("Release Feed")
        );
    }
}
