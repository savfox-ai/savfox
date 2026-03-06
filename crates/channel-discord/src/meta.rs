use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordStartMeta {
    pub peer_id: Option<String>,
    pub guild_id: Option<String>,
    pub role_ids: Vec<String>,
    pub parent_thread_id: Option<String>,
    pub reply_target: Option<String>,
    pub parent_sender_id: Option<String>,
    pub chat_type: Option<String>,
}

pub fn parse_start_meta(payload: &Value) -> DiscordStartMeta {
    let peer_id = payload
        .pointer("/member/user/id")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/user/id").and_then(Value::as_str))
        .map(str::to_string);
    let guild_id = payload
        .get("guild_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let role_ids = payload
        .pointer("/member/roles")
        .and_then(Value::as_array)
        .map(|roles| {
            roles
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let parent_thread_id = payload
        .pointer("/message/message_reference/channel_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let reply_target = payload
        .pointer("/message/message_reference/message_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let parent_sender_id = payload
        .pointer("/message/referenced_message/author/id")
        .and_then(Value::as_str)
        .map(str::to_string);

    let is_group_chat = guild_id.is_some();
    DiscordStartMeta {
        peer_id,
        guild_id,
        role_ids,
        parent_thread_id,
        reply_target,
        parent_sender_id,
        chat_type: Some(if is_group_chat { "group" } else { "dm" }.to_string()),
    }
}
