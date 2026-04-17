use savfox_core::channel::ChannelAction;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordMessageParseResult {
    pub action: ChannelAction,
    pub dedupe_key: Option<String>,
}

fn discord_message_id(payload: &Value) -> Option<String> {
    payload
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn discord_message_content(payload: &Value) -> &str {
    payload
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn discord_channel_id(payload: &Value) -> Option<String> {
    payload
        .get("channel_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn split_discord_command(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let (head, rest) = if let Some(index) = trimmed.find(char::is_whitespace) {
        let (head, tail) = trimmed.split_at(index);
        (head, tail.trim())
    } else {
        (trimmed, "")
    };
    let command = head.trim_start_matches('/').trim().to_ascii_lowercase();
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
    let (command, args) = split_discord_command(text)?;
    let canonical = resolve_command_name(&command)?;
    let mut prompt = format!("/{canonical}");
    let args = args.trim();
    if !args.is_empty() {
        prompt.push(' ');
        prompt.push_str(args);
    }
    Some(prompt)
}

fn parse_command_prompt_with_resolver<F>(text: &str, resolve_command_name: &F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    let (command, args) = split_discord_command(text)?;
    if command == "savfox" {
        let prompt = args.trim();
        if prompt.is_empty() {
            return None;
        }
        return Some(prompt.to_owned());
    }

    normalize_registry_command_with_resolver(text, resolve_command_name)
}

fn parse_direct_prompt_with_resolver<F>(text: &str, resolve_command_name: &F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(prompt) = parse_command_prompt_with_resolver(text, resolve_command_name) {
        return Some(prompt);
    }

    let prompt = text.trim();
    if prompt.is_empty() {
        None
    } else {
        Some(prompt.to_owned())
    }
}

fn strip_leading_bot_mention<'a>(text: &'a str, bot_user_id: &str) -> Option<&'a str> {
    let trimmed = text.trim_start();
    let mention = format!("<@{bot_user_id}>");
    let bang_mention = format!("<@!{bot_user_id}>");
    let rest = if let Some(rest) = trimmed.strip_prefix(&mention) {
        rest
    } else if let Some(rest) = trimmed.strip_prefix(&bang_mention) {
        rest
    } else {
        return None;
    };

    Some(rest.trim_start_matches(|ch: char| ch.is_whitespace() || ch == ':' || ch == ','))
}

fn replied_to_bot(payload: &Value, bot_user_id: &str) -> bool {
    payload
        .pointer("/referenced_message/author/id")
        .and_then(Value::as_str)
        .is_some_and(|value| value == bot_user_id)
}

#[must_use]
pub fn quote_discord_arg(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_owned();
    }
    if value
        .chars()
        .all(|ch| !ch.is_whitespace() && ch != '"' && ch != '\\')
    {
        return value.to_owned();
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
        .to_owned();
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

pub fn parse_message(
    payload: &Value,
    bot_user_id: &str,
) -> anyhow::Result<DiscordMessageParseResult> {
    parse_message_with_resolver(payload, bot_user_id, |_| None)
}

pub fn parse_message_with_resolver<F>(
    payload: &Value,
    bot_user_id: &str,
    resolve_command_name: F,
) -> anyhow::Result<DiscordMessageParseResult>
where
    F: Fn(&str) -> Option<String>,
{
    let dedupe_key = discord_message_id(payload).map(|id| format!("discord:{id}"));
    let Some(channel) = discord_channel_id(payload) else {
        return Ok(DiscordMessageParseResult {
            action: ChannelAction::Ignore,
            dedupe_key,
        });
    };

    let content = discord_message_content(payload);
    let is_dm = payload
        .get("guild_id")
        .and_then(Value::as_str)
        .is_none_or(|value| value.trim().is_empty());

    let action =
        if let Some(prompt) = parse_command_prompt_with_resolver(content, &resolve_command_name) {
            ChannelAction::StartThread { channel, prompt }
        } else if let Some(stripped) = strip_leading_bot_mention(content, bot_user_id) {
            match parse_direct_prompt_with_resolver(stripped, &resolve_command_name) {
                Some(prompt) => ChannelAction::StartThread { channel, prompt },
                None => ChannelAction::Ignore,
            }
        } else if replied_to_bot(payload, bot_user_id) {
            match parse_direct_prompt_with_resolver(content, &resolve_command_name) {
                Some(prompt) => ChannelAction::StartThread { channel, prompt },
                None => ChannelAction::Ignore,
            }
        } else if is_dm {
            match parse_direct_prompt_with_resolver(content, &resolve_command_name) {
                Some(prompt) => ChannelAction::StartThread { channel, prompt },
                None => ChannelAction::Ignore,
            }
        } else {
            let prompt = content.trim();
            if prompt.is_empty() {
                ChannelAction::Ignore
            } else {
                ChannelAction::StartThread {
                    channel,
                    prompt: prompt.to_owned(),
                }
            }
        };

    Ok(DiscordMessageParseResult { action, dedupe_key })
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

pub fn parse_display_name(payload: &Value) -> Option<String> {
    payload
        .pointer("/member/nick")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            payload
                .pointer("/author/global_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            payload
                .pointer("/author/username")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            payload
                .pointer("/user/global_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            payload
                .pointer("/user/username")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

#[cfg(test)]
mod tests {
    use savfox_core::channel::ChannelAction;
    use serde_json::json;

    use super::{
        build_command_prompt, parse_display_name, parse_interaction_with_resolver,
        parse_message_with_resolver,
    };

    fn resolve_command_name(command: &str) -> Option<String> {
        match command {
            "status" => Some("status".to_owned()),
            "commands" => Some("commands".to_owned()),
            _ => None,
        }
    }

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

    #[test]
    fn dm_plain_text_starts_thread() {
        let payload = json!({
            "id": "99",
            "channel_id": "123",
            "content": "hello from discord",
            "author": { "id": "u1", "username": "alice" }
        });

        let parsed =
            parse_message_with_resolver(&payload, "bot-1", resolve_command_name).expect("parse");
        assert_eq!(parsed.dedupe_key.as_deref(), Some("discord:99"));
        match parsed.action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "123");
                assert_eq!(prompt, "hello from discord");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn guild_message_without_direct_context_reaches_runtime() {
        let payload = json!({
            "id": "99",
            "channel_id": "123",
            "guild_id": "456",
            "content": "hello from discord",
            "author": { "id": "u1", "username": "alice" }
        });

        let parsed =
            parse_message_with_resolver(&payload, "bot-1", resolve_command_name).expect("parse");
        match parsed.action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "123");
                assert_eq!(prompt, "hello from discord");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn bot_mention_in_guild_starts_thread() {
        let payload = json!({
            "id": "99",
            "channel_id": "123",
            "guild_id": "456",
            "content": "<@bot-1> summarize this",
            "author": { "id": "u1", "username": "alice" }
        });

        let parsed =
            parse_message_with_resolver(&payload, "bot-1", resolve_command_name).expect("parse");
        match parsed.action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "123");
                assert_eq!(prompt, "summarize this");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn reply_to_bot_starts_thread_without_mention() {
        let payload = json!({
            "id": "99",
            "channel_id": "123",
            "guild_id": "456",
            "content": "+",
            "author": { "id": "u1", "username": "alice" },
            "referenced_message": {
                "author": { "id": "bot-1", "username": "savfox" }
            }
        });

        let parsed =
            parse_message_with_resolver(&payload, "bot-1", resolve_command_name).expect("parse");
        match parsed.action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "123");
                assert_eq!(prompt, "+");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn registry_command_normalizes_to_slash_prompt() {
        let payload = json!({
            "id": "99",
            "channel_id": "123",
            "content": "/status",
            "author": { "id": "u1", "username": "alice" }
        });

        let parsed =
            parse_message_with_resolver(&payload, "bot-1", resolve_command_name).expect("parse");
        match parsed.action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "123");
                assert_eq!(prompt, "/status");
            }
            _ => panic!("expected start thread action"),
        }
    }

    #[test]
    fn parse_display_name_prefers_nick_then_global_name() {
        let payload = json!({
            "author": { "id": "u1", "username": "alice", "global_name": "Alice Global" },
            "member": { "nick": "Alice Nick" }
        });

        assert_eq!(parse_display_name(&payload).as_deref(), Some("Alice Nick"));
    }
}
