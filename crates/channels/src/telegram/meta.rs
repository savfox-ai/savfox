use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TelegramStartMeta {
    pub peer_id: Option<String>,
    pub chat_id: Option<String>,
    pub chat_type: Option<String>,
    pub thread_id: Option<String>,
    pub reply_target: Option<String>,
    pub topic: Option<String>,
}

fn inbound_message(payload: &Value) -> Option<&Value> {
    payload
        .get("message")
        .or_else(|| payload.get("channel_post"))
}

pub fn parse_start_meta(payload: &Value) -> TelegramStartMeta {
    let message = inbound_message(payload);
    let peer_id = message
        .and_then(|msg| {
            msg.get("from")
                .and_then(|from| from.get("id"))
                .and_then(Value::as_i64)
                .or_else(|| {
                    msg.get("sender_chat")
                        .and_then(|sender| sender.get("id"))
                        .and_then(Value::as_i64)
                })
        })
        .map(|v| v.to_string());
    let chat_id = message
        .and_then(|msg| msg.get("chat"))
        .and_then(|chat| chat.get("id"))
        .and_then(Value::as_i64)
        .map(|v| v.to_string());
    let chat_type = message
        .and_then(|msg| msg.get("chat"))
        .and_then(|chat| chat.get("type"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let thread_id = message
        .and_then(|msg| msg.get("message_thread_id"))
        .and_then(Value::as_i64)
        .map(|v| v.to_string());
    let reply_target = message
        .and_then(|msg| msg.get("reply_to_message"))
        .and_then(|reply| reply.get("message_id"))
        .and_then(Value::as_i64)
        .map(|v| v.to_string());
    let topic = message
        .and_then(|msg| msg.get("chat"))
        .and_then(|chat| chat.get("title"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    TelegramStartMeta {
        peer_id,
        chat_id,
        chat_type,
        thread_id,
        reply_target,
        topic,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{TelegramStartMeta, parse_start_meta};

    #[test]
    fn parses_channel_post_start_meta() {
        let payload = json!({
            "channel_post": {
                "chat": { "id": -100123, "type": "channel", "title": "Release Feed" },
                "sender_chat": { "id": -100123, "title": "Release Feed" },
                "message_thread_id": 77,
                "reply_to_message": { "message_id": 12 }
            }
        });

        assert_eq!(
            parse_start_meta(&payload),
            TelegramStartMeta {
                peer_id: Some("-100123".to_owned()),
                chat_id: Some("-100123".to_owned()),
                chat_type: Some("channel".to_owned()),
                thread_id: Some("77".to_owned()),
                reply_target: Some("12".to_owned()),
                topic: Some("Release Feed".to_owned()),
            }
        );
    }
}
