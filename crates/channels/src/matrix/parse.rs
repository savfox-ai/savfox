use savfox_core::channel::ChannelAction;
use serde_json::Value;

#[allow(clippy::print_stdout)]
fn debug_matrix_inbound_message(room_id: &str, sender: &str, text: &str) {
    println!("[matrix][inbound] room={room_id} sender={sender} text={text}");
}

#[derive(Debug, Clone)]
pub struct MatrixWebhookParseResult {
    pub action: ChannelAction,
    pub dedupe_key: Option<String>,
    pub rooms_to_auto_join: Vec<(String, Option<String>)>,
}

pub fn parse_invite_event(event: &Value) -> Option<(String, Option<String>)> {
    if event.get("type").and_then(Value::as_str) != Some("m.room.member") {
        return None;
    }

    let membership = event
        .get("content")
        .and_then(|c| c.get("membership"))
        .and_then(Value::as_str)?;
    if !membership.eq_ignore_ascii_case("invite") {
        return None;
    }

    let room_id = event.get("room_id").and_then(Value::as_str)?.trim();
    if room_id.is_empty() {
        return None;
    }

    let invited_user_id = event
        .get("state_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    Some((room_id.to_owned(), invited_user_id))
}

pub fn parse_webhook_payload(payload: &Value) -> MatrixWebhookParseResult {
    let mut action = ChannelAction::Ignore;
    let mut dedupe_key = None;
    let mut rooms_to_auto_join = Vec::new();

    if let Some(events) = payload.get("events").and_then(Value::as_array) {
        for event in events {
            if let Some((room_id, invited_user_id)) = parse_invite_event(event)
                && !rooms_to_auto_join.iter().any(
                    |(existing_room_id, _): &(String, Option<String>)| {
                        existing_room_id.eq_ignore_ascii_case(&room_id)
                    },
                )
            {
                rooms_to_auto_join.push((room_id, invited_user_id));
            }

            if event.get("type").and_then(Value::as_str) != Some("m.room.message") {
                continue;
            }
            let content = event.get("content").unwrap_or(&Value::Null);
            if content.get("msgtype").and_then(Value::as_str) != Some("m.text") {
                continue;
            }
            let text = content.get("body").and_then(Value::as_str).unwrap_or("");
            let room_id = event.get("room_id").and_then(Value::as_str).unwrap_or("");
            let sender = event.get("sender").and_then(Value::as_str).unwrap_or("");
            debug_matrix_inbound_message(room_id, sender, text);
            if let Some(prompt) = text.strip_prefix("!savfox ").map(str::trim)
                && !prompt.is_empty()
            {
                let room_id = room_id.to_owned();
                if room_id.is_empty() {
                    break;
                }
                dedupe_key = event
                    .get("event_id")
                    .and_then(Value::as_str)
                    .map(|id| format!("matrix:{id}"));
                action = ChannelAction::StartThread {
                    channel: room_id,
                    prompt: prompt.to_owned(),
                };
                break;
            }
        }
    }

    MatrixWebhookParseResult {
        action,
        dedupe_key,
        rooms_to_auto_join,
    }
}
