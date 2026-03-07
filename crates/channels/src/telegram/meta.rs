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

pub fn parse_start_meta(payload: &Value) -> TelegramStartMeta {
    let peer_id = payload
        .pointer("/message/from/id")
        .and_then(Value::as_i64)
        .map(|v| v.to_string());
    let chat_id = payload
        .pointer("/message/chat/id")
        .and_then(Value::as_i64)
        .map(|v| v.to_string());
    let chat_type = payload
        .pointer("/message/chat/type")
        .and_then(Value::as_str)
        .map(str::to_string);
    let thread_id = payload
        .pointer("/message/message_thread_id")
        .and_then(Value::as_i64)
        .map(|v| v.to_string());
    let reply_target = payload
        .pointer("/message/reply_to_message/message_id")
        .and_then(Value::as_i64)
        .map(|v| v.to_string());
    let topic = payload
        .pointer("/message/chat/title")
        .and_then(Value::as_str)
        .map(str::to_string);

    TelegramStartMeta {
        peer_id,
        chat_id,
        chat_type,
        thread_id,
        reply_target,
        topic,
    }
}
