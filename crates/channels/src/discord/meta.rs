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

fn first_pointer_string(payload: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

pub fn parse_start_meta(payload: &Value) -> DiscordStartMeta {
    let peer_id = first_pointer_string(payload, &["/member/user/id", "/author/id", "/user/id"]);
    let guild_id = first_pointer_string(payload, &["/guild_id"]);
    let role_ids = payload
        .pointer("/member/roles")
        .and_then(Value::as_array)
        .map(|roles| {
            roles
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let parent_thread_id = first_pointer_string(
        payload,
        &[
            "/message/message_reference/channel_id",
            "/message_reference/channel_id",
        ],
    );
    let reply_target = first_pointer_string(
        payload,
        &[
            "/message/message_reference/message_id",
            "/message_reference/message_id",
        ],
    );
    let parent_sender_id = first_pointer_string(
        payload,
        &[
            "/message/referenced_message/author/id",
            "/referenced_message/author/id",
        ],
    );

    let is_group_chat = guild_id.is_some();
    DiscordStartMeta {
        peer_id,
        guild_id,
        role_ids,
        parent_thread_id,
        reply_target,
        parent_sender_id,
        chat_type: Some(if is_group_chat { "group" } else { "dm" }.to_owned()),
    }
}
