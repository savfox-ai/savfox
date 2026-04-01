use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlackStartMeta {
    pub peer_id: Option<String>,
    pub channel_id: Option<String>,
    pub team_id: Option<String>,
    pub account_id: Option<String>,
    pub thread_id: Option<String>,
    pub reply_target: Option<String>,
    pub parent_sender_id: Option<String>,
    pub slack_groups: Vec<String>,
    pub chat_type: Option<String>,
}

fn append_string_values(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.to_string()),
        Value::Array(items) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    out.push(s.to_string());
                }
            }
        }
        _ => {}
    }
}

pub fn parse_start_meta(payload: &Value) -> SlackStartMeta {
    let peer_id = payload
        .get("user_id")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/event/user").and_then(Value::as_str))
        .map(str::to_string);
    let team_id = payload
        .get("team_id")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/team/id").and_then(Value::as_str))
        .map(str::to_string);
    let account_id = payload
        .get("api_app_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let thread_id = payload
        .pointer("/event/thread_ts")
        .and_then(Value::as_str)
        .or_else(|| payload.get("thread_ts").and_then(Value::as_str))
        .map(str::to_string);
    let reply_target = payload
        .pointer("/event/ts")
        .and_then(Value::as_str)
        .map(str::to_string);
    let parent_sender_id = payload
        .pointer("/event/parent_user_id")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut slack_groups = Vec::new();
    for ptr in [
        "/event/user_groups",
        "/user_groups",
        "/event/user_profile/user_groups",
        "/user_profile/user_groups",
    ] {
        if let Some(value) = payload.pointer(ptr) {
            append_string_values(value, &mut slack_groups);
        }
    }

    let channel_id = payload
        .pointer("/event/channel")
        .and_then(Value::as_str)
        .or_else(|| payload.get("channel_id").and_then(Value::as_str))
        .map(str::to_string);
    let chat_type = channel_id
        .as_deref()
        .map(|channel| {
            if channel.starts_with('D') {
                "dm".to_string()
            } else {
                "group".to_string()
            }
        })
        .or_else(|| Some("group".to_string()));

    SlackStartMeta {
        peer_id,
        channel_id,
        team_id,
        account_id,
        thread_id: thread_id.clone(),
        reply_target,
        parent_sender_id,
        slack_groups,
        chat_type,
    }
}
