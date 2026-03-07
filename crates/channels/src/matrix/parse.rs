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

fn push_events<'a>(
    out: &mut Vec<(&'a Value, Option<&'a str>)>,
    events: Option<&'a Vec<Value>>,
    room_id_hint: Option<&'a str>,
) {
    let Some(events) = events else {
        return;
    };

    for event in events {
        out.push((event, room_id_hint));
    }
}

fn collect_sync_room_events<'a>(
    payload: &'a Value,
    section: &str,
    event_paths: &[(&str, &str)],
    out: &mut Vec<(&'a Value, Option<&'a str>)>,
) {
    let Some(rooms) = payload
        .get("rooms")
        .and_then(|value| value.get(section))
        .and_then(Value::as_object)
    else {
        return;
    };

    for (room_id, room_data) in rooms {
        for (container_key, events_key) in event_paths {
            push_events(
                out,
                room_data
                    .get(*container_key)
                    .and_then(|value| value.get(*events_key))
                    .and_then(Value::as_array),
                Some(room_id.as_str()),
            );
        }
    }
}

fn collect_matrix_events(payload: &Value) -> Vec<(&Value, Option<&str>)> {
    let mut out = Vec::new();

    if payload.get("type").is_some() {
        out.push((payload, payload.get("room_id").and_then(Value::as_str)));
    }

    push_events(
        &mut out,
        payload.get("events").and_then(Value::as_array),
        None,
    );
    collect_sync_room_events(payload, "invite", &[("invite_state", "events")], &mut out);
    collect_sync_room_events(
        payload,
        "join",
        &[("timeline", "events"), ("state", "events")],
        &mut out,
    );

    out
}

pub fn parse_invite_event(
    event: &Value,
    room_id_hint: Option<&str>,
) -> Option<(String, Option<String>)> {
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

    let room_id = event
        .get("room_id")
        .and_then(Value::as_str)
        .or(room_id_hint)?
        .trim();
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

    for (event, room_id_hint) in collect_matrix_events(payload) {
        if let Some((room_id, invited_user_id)) = parse_invite_event(event, room_id_hint)
            && !rooms_to_auto_join
                .iter()
                .any(|(existing_room_id, _): &(String, Option<String>)| {
                    existing_room_id.eq_ignore_ascii_case(&room_id)
                })
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
        let room_id = event
            .get("room_id")
            .and_then(Value::as_str)
            .or(room_id_hint)
            .unwrap_or("");
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

    MatrixWebhookParseResult {
        action,
        dedupe_key,
        rooms_to_auto_join,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{MatrixWebhookParseResult, parse_webhook_payload};
    use savfox_core::channel::ChannelAction;

    fn assert_start_thread(
        parsed: &MatrixWebhookParseResult,
        expected_room_id: &str,
        expected_prompt: &str,
    ) {
        match &parsed.action {
            ChannelAction::StartThread { channel, prompt } => {
                assert_eq!(channel, expected_room_id);
                assert_eq!(prompt, expected_prompt);
            }
            other => panic!("expected start thread action, got {other:?}"),
        }
    }

    #[test]
    fn parses_flat_event_payload_for_invites_and_messages() {
        let payload = json!({
            "events": [
                {
                    "type": "m.room.member",
                    "room_id": "!flat:matrix.org",
                    "state_key": "@savfox:matrix.org",
                    "content": { "membership": "invite" }
                },
                {
                    "type": "m.room.message",
                    "room_id": "!flat:matrix.org",
                    "event_id": "$flat",
                    "sender": "@user:matrix.org",
                    "content": {
                        "msgtype": "m.text",
                        "body": "!savfox summarize this room"
                    }
                }
            ]
        });

        let parsed = parse_webhook_payload(&payload);
        assert_eq!(
            parsed.rooms_to_auto_join,
            vec![(
                "!flat:matrix.org".to_string(),
                Some("@savfox:matrix.org".to_string())
            )]
        );
        assert_eq!(parsed.dedupe_key.as_deref(), Some("matrix:$flat"));
        assert_start_thread(&parsed, "!flat:matrix.org", "summarize this room");
    }

    #[test]
    fn parses_sync_payload_for_invites_and_joined_room_messages() {
        let payload = json!({
            "rooms": {
                "invite": {
                    "!invite:matrix.org": {
                        "invite_state": {
                            "events": [
                                {
                                    "type": "m.room.member",
                                    "state_key": "@savfox:matrix.org",
                                    "content": { "membership": "invite" }
                                }
                            ]
                        }
                    }
                },
                "join": {
                    "!joined:matrix.org": {
                        "timeline": {
                            "events": [
                                {
                                    "type": "m.room.message",
                                    "event_id": "$joined",
                                    "sender": "@user:matrix.org",
                                    "content": {
                                        "msgtype": "m.text",
                                        "body": "!savfox sync payload works"
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        });

        let parsed = parse_webhook_payload(&payload);
        assert_eq!(
            parsed.rooms_to_auto_join,
            vec![(
                "!invite:matrix.org".to_string(),
                Some("@savfox:matrix.org".to_string())
            )]
        );
        assert_eq!(parsed.dedupe_key.as_deref(), Some("matrix:$joined"));
        assert_start_thread(&parsed, "!joined:matrix.org", "sync payload works");
    }
}
