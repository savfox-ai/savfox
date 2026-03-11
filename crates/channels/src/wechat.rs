use std::path::PathBuf;

use anyhow::Context;
use serde_json::{Map, Value, json};
use tracing::warn;

use savfox_core::channel::ChannelAction;

fn non_empty(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
    })
}

#[derive(Debug, Clone)]
pub struct WeChatChannelConfig {
    pub id: String,
    pub webhook_url: Option<String>,
    pub verify_token: Option<String>,
}

impl WeChatChannelConfig {
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.kind.eq_ignore_ascii_case("wechat") {
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

pub async fn resolve_wechat_webhook_url(savfox_home: &PathBuf) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(WeChatChannelConfig::from_channel_config)
        .filter(WeChatChannelConfig::has_outbound_auth)
        .find_map(|cfg| cfg.webhook_url);
    Ok(result)
}

pub async fn resolve_wechat_verify_token(savfox_home: &PathBuf) -> anyhow::Result<Option<String>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs")?;
    let result = all_configs
        .iter()
        .filter(|c| c.enabled)
        .filter_map(WeChatChannelConfig::from_channel_config)
        .find_map(|cfg| cfg.verify_token);
    Ok(result)
}

#[derive(Debug, Clone, Default)]
pub struct WeChatStartMeta {
    pub user_id: Option<String>,
    pub room_id: Option<String>,
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
                Some(trimmed.to_string())
            }
        }
        Some(Value::Number(number)) => Some(number.to_string()),
        _ => None,
    }
}

fn extract_text(payload: &Value) -> Option<String> {
    for key in ["Content", "content", "text", "message"] {
        if let Some(text) = payload.get(key).and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn normalize_prompt(text: &str, is_group: bool) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !is_group {
        return Some(trimmed.to_string());
    }

    for prefix in ["/savfox ", "!savfox ", "@savfox "] {
        if let Some(prompt) = trimmed.strip_prefix(prefix) {
            let prompt = prompt.trim();
            if !prompt.is_empty() {
                return Some(prompt.to_string());
            }
        }
    }

    None
}

pub fn parse_start_meta(payload: &Value) -> WeChatStartMeta {
    let room_id = value_to_string(
        payload
            .get("room_id")
            .or_else(|| payload.get("roomId"))
            .or_else(|| payload.get("chatroom_id"))
            .or_else(|| payload.get("chatroomId")),
    );
    let user_id = value_to_string(
        payload
            .get("from_user")
            .or_else(|| payload.get("fromUser"))
            .or_else(|| payload.get("talker_id"))
            .or_else(|| payload.get("talkerId"))
            .or_else(|| payload.get("FromUserName")),
    )
    .or_else(|| {
        payload
            .get("sender")
            .and_then(|sender| sender.get("id"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
    });

    WeChatStartMeta {
        user_id,
        room_id: room_id.clone(),
        message_id: value_to_string(
            payload
                .get("message_id")
                .or_else(|| payload.get("messageId"))
                .or_else(|| payload.get("MsgId"))
                .or_else(|| payload.get("id")),
        ),
        sender_name: value_to_string(
            payload
                .get("sender_name")
                .or_else(|| payload.get("senderName"))
                .or_else(|| payload.get("nickname")),
        )
        .or_else(|| {
            payload
                .get("sender")
                .and_then(|sender| sender.get("name"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        }),
        chat_type: Some(if room_id.is_some() {
            "group".to_string()
        } else {
            "private".to_string()
        }),
    }
}

pub fn parse_webhook_payload(payload: &Value) -> ChannelAction {
    let meta = parse_start_meta(payload);
    let is_group = meta.room_id.is_some();
    let Some(text) = extract_text(payload) else {
        return ChannelAction::Ignore;
    };
    let Some(prompt) = normalize_prompt(&text, is_group) else {
        return ChannelAction::Ignore;
    };

    let channel = meta.room_id.or(meta.user_id).unwrap_or_default();
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

    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "WeChat bridge API error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn direct_message_routes_full_text() {
        let payload = json!({
            "from_user": "wxid_alice",
            "content": "hello from wechat"
        });

        let action = parse_webhook_payload(&payload);
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "wxid_alice");
                assert_eq!(prompt, "hello from wechat");
            }
            other => panic!("expected StartThread, got {other:?}"),
        }
    }

    #[test]
    fn group_message_requires_prefix() {
        let payload = json!({
            "room_id": "room-1",
            "content": "plain group chat"
        });

        assert_eq!(parse_webhook_payload(&payload), ChannelAction::Ignore);
    }

    #[test]
    fn group_message_with_prefix_routes_prompt() {
        let payload = json!({
            "room_id": "room-1",
            "content": "!savfox please help"
        });

        let action = parse_webhook_payload(&payload);
        match action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, "room-1");
                assert_eq!(prompt, "please help");
            }
            other => panic!("expected StartThread, got {other:?}"),
        }
    }
}
