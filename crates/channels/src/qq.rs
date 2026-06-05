use std::path::PathBuf;

use anyhow::Context;
use savfox_core::channel::ChannelAction;
use serde_json::{Value, json};

use crate::base::non_empty;

#[derive(Debug, Clone)]
pub struct QQChannelConfig {
    pub id: String,
    pub webhook_url: Option<String>,
    pub verify_token: Option<String>,
}

impl QQChannelConfig {
    #[must_use]
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.kind.eq_ignore_ascii_case("qq") {
            return None;
        }
        let raw = config.config.as_object()?;
        Some(Self {
            id: config.id.clone(),
            webhook_url: non_empty(
                raw,
                &["webhook_url", "webhookUrl", "send_url", "sendUrl", "url"],
            ),
            verify_token: non_empty(
                raw,
                &[
                    "verify_token",
                    "verifyToken",
                    "verification_token",
                    "verificationToken",
                    "token",
                ],
            ),
        })
    }

    fn has_outbound_auth(&self) -> bool {
        self.webhook_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    }
}

pub async fn resolve_qq_webhook_url(savfox_home: &PathBuf) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(QQChannelConfig::from_channel_config)
        .filter(QQChannelConfig::has_outbound_auth)
        .find_map(|cfg| cfg.webhook_url);
    Ok(result)
}

pub async fn resolve_qq_verify_token(savfox_home: &PathBuf) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(QQChannelConfig::from_channel_config)
        .find_map(|cfg| cfg.verify_token);
    Ok(result)
}

#[derive(Debug, Clone, Default)]
pub struct QQStartMeta {
    pub user_id: Option<String>,
    pub group_id: Option<String>,
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub message_id: Option<String>,
    pub sender_name: Option<String>,
    pub chat_type: Option<String>,
}

fn value_to_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        Some(Value::Number(number)) => Some(number.to_string()),
        _ => None,
    }
}

fn extract_text(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        Some(Value::Array(items)) => {
            let mut out = String::new();
            for item in items {
                if let Some(text) = item.as_str() {
                    out.push_str(text);
                    continue;
                }
                if item.get("type").and_then(Value::as_str) == Some("text")
                    && let Some(text) = item
                        .get("data")
                        .and_then(|data| data.get("text"))
                        .and_then(Value::as_str)
                {
                    out.push_str(text);
                    continue;
                }
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
            let trimmed = out.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        _ => None,
    }
}

fn normalize_prompt(text: &str, is_group: bool) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !is_group {
        return Some(trimmed.to_owned());
    }

    for prefix in ["/savfox ", "!savfox ", "@savfox "] {
        if let Some(prompt) = trimmed.strip_prefix(prefix) {
            let prompt = prompt.trim();
            if !prompt.is_empty() {
                return Some(prompt.to_owned());
            }
        }
    }

    None
}

pub fn parse_start_meta(payload: &Value) -> QQStartMeta {
    let sender = payload.get("sender").unwrap_or(&Value::Null);
    let channel_id = value_to_string(payload.get("channel_id"));
    let group_id = value_to_string(payload.get("group_id"));
    let guild_id = value_to_string(payload.get("guild_id"));
    let user_id = value_to_string(payload.get("user_id"))
        .or_else(|| value_to_string(payload.get("sender_id")))
        .or_else(|| value_to_string(sender.get("user_id")))
        .or_else(|| value_to_string(sender.get("id")));

    let chat_type = payload
        .get("message_type")
        .or_else(|| payload.get("chat_type"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            if group_id.is_some() || channel_id.is_some() || guild_id.is_some() {
                Some("group".to_owned())
            } else {
                Some("private".to_owned())
            }
        });

    QQStartMeta {
        user_id,
        group_id,
        guild_id,
        channel_id,
        message_id: value_to_string(payload.get("message_id"))
            .or_else(|| value_to_string(payload.get("id"))),
        sender_name: value_to_string(sender.get("nickname"))
            .or_else(|| value_to_string(sender.get("card")))
            .or_else(|| value_to_string(sender.get("name"))),
        chat_type,
    }
}

#[must_use]
pub fn parse_webhook_payload(payload: &Value) -> ChannelAction {
    let meta = parse_start_meta(payload);
    let is_group = matches!(meta.chat_type.as_deref(), Some("group" | "channel"))
        || meta.group_id.is_some()
        || meta.channel_id.is_some()
        || meta.guild_id.is_some();

    let Some(text) = extract_text(
        payload
            .get("raw_message")
            .or_else(|| payload.get("message"))
            .or_else(|| payload.get("text"))
            .or_else(|| payload.get("content")),
    ) else {
        return ChannelAction::Ignore;
    };
    let Some(prompt) = normalize_prompt(&text, is_group) else {
        return ChannelAction::Ignore;
    };

    let channel = meta
        .channel_id
        .or(meta.group_id)
        .or(meta.guild_id)
        .or(meta.user_id)
        .unwrap_or_default();
    if channel.is_empty() {
        return ChannelAction::Ignore;
    }

    ChannelAction::StartThread { channel, prompt }
}

pub async fn send_webhook_message(
    client: &reqwest::Client,
    webhook_url: &str,
    channel: &str,
    text: &str,
) -> anyhow::Result<()> {
    let body = json!({
        "channel": channel,
        "text": text,
    });

    let response = client
        .post(webhook_url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    crate::http::warn_on_error(response, "QQ bridge API error").await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn private_message_routes_full_text() {
        let payload = json!({
            "message_type": "private",
            "user_id": 12345,
            "raw_message": "hello from qq"
        });

        let action = parse_webhook_payload(&payload);
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "12345");
                assert_eq!(prompt, "hello from qq");
            }
            other => panic!("expected StartThread, got {other:?}"),
        }
    }

    #[test]
    fn group_message_requires_prefix() {
        let payload = json!({
            "message_type": "group",
            "group_id": 67890,
            "raw_message": "just chatting"
        });

        assert_eq!(parse_webhook_payload(&payload), ChannelAction::Ignore);
    }

    #[test]
    fn group_message_with_prefix_routes_prompt() {
        let payload = json!({
            "message_type": "group",
            "group_id": 67890,
            "raw_message": "/savfox summarize this"
        });

        let action = parse_webhook_payload(&payload);
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "67890");
                assert_eq!(prompt, "summarize this");
            }
            other => panic!("expected StartThread, got {other:?}"),
        }
    }
}
