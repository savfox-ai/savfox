use savfox_core::channel::ChannelAction;
use serde_json::Value;

#[allow(clippy::print_stdout)]
fn debug_matrix_inbound_message(room_id: &str, sender: &str, text: &str) {
    println!("[matrix][inbound] room={room_id} sender={sender} text={text}");
}

#[allow(clippy::print_stdout)]
fn debug_matrix_invite_detected(room_id: &str, invited_user_id: Option<&str>) {
    println!(
        "[matrix][invite] detected room={} invited_user={}",
        room_id,
        invited_user_id.unwrap_or("")
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixCommandEvent {
    pub room_id: String,
    pub sender: String,
    pub prompt: String,
    pub dedupe_key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatrixInboundParseResult {
    pub commands: Vec<MatrixCommandEvent>,
    pub rooms_to_auto_join: Vec<(String, Option<String>)>,
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

    debug_matrix_invite_detected(room_id, invited_user_id.as_deref());
    Some((room_id.to_owned(), invited_user_id))
}

pub fn parse_command_event(
    event: &Value,
    room_id_hint: Option<&str>,
) -> Option<MatrixCommandEvent> {
    if event.get("type").and_then(Value::as_str) != Some("m.room.message") {
        return None;
    }
    let content = event.get("content").unwrap_or(&Value::Null);
    if content.get("msgtype").and_then(Value::as_str) != Some("m.text") {
        return None;
    }
    let text = content.get("body").and_then(Value::as_str).unwrap_or("");
    let room_id = event
        .get("room_id")
        .and_then(Value::as_str)
        .or(room_id_hint)
        .unwrap_or("");
    let sender = event.get("sender").and_then(Value::as_str).unwrap_or("");
    debug_matrix_inbound_message(room_id, sender, text);
    let prompt = text.strip_prefix("!savfox ").map(str::trim)?;
    if prompt.is_empty() || room_id.is_empty() {
        return None;
    }
    let dedupe_key = event
        .get("event_id")
        .and_then(Value::as_str)
        .map(|id| format!("matrix:{id}"));
    Some(MatrixCommandEvent {
        room_id: room_id.to_owned(),
        sender: sender.to_owned(),
        prompt: prompt.to_owned(),
        dedupe_key,
    })
}

pub fn parse_inbound_payload(payload: &Value) -> MatrixInboundParseResult {
    let mut parsed = MatrixInboundParseResult::default();

    for (event, room_id_hint) in collect_matrix_events(payload) {
        if let Some((room_id, invited_user_id)) = parse_invite_event(event, room_id_hint)
            && !parsed.rooms_to_auto_join.iter().any(
                |(existing_room_id, _): &(String, Option<String>)| {
                    existing_room_id.eq_ignore_ascii_case(&room_id)
                },
            )
        {
            parsed.rooms_to_auto_join.push((room_id, invited_user_id));
        }

        if let Some(command) = parse_command_event(event, room_id_hint) {
            parsed.commands.push(command);
        }
    }

    parsed
}

pub fn parse_webhook_payload(payload: &Value) -> MatrixWebhookParseResult {
    let parsed = parse_inbound_payload(payload);
    let mut action = ChannelAction::Ignore;
    let mut dedupe_key = None;

    if let Some(command) = parsed.commands.first() {
        dedupe_key = command.dedupe_key.clone();
        action = ChannelAction::StartThread {
            channel: command.room_id.clone(),
            prompt: command.prompt.clone(),
        };
    }

    MatrixWebhookParseResult {
        action,
        dedupe_key,
        rooms_to_auto_join: parsed.rooms_to_auto_join,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{MatrixWebhookParseResult, parse_inbound_payload, parse_webhook_payload};
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

    #[test]
    fn parses_multiple_commands_from_single_sync_payload() {
        let payload = json!({
            "rooms": {
                "join": {
                    "!joined:matrix.org": {
                        "timeline": {
                            "events": [
                                {
                                    "type": "m.room.message",
                                    "event_id": "$joined-1",
                                    "sender": "@user-1:matrix.org",
                                    "content": {
                                        "msgtype": "m.text",
                                        "body": "!savfox first command"
                                    }
                                },
                                {
                                    "type": "m.room.message",
                                    "event_id": "$joined-2",
                                    "sender": "@user-2:matrix.org",
                                    "content": {
                                        "msgtype": "m.text",
                                        "body": "!savfox second command"
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        });

        let parsed = parse_inbound_payload(&payload);
        assert_eq!(parsed.commands.len(), 2);
        assert_eq!(parsed.commands[0].room_id, "!joined:matrix.org");
        assert_eq!(parsed.commands[0].sender, "@user-1:matrix.org");
        assert_eq!(parsed.commands[0].prompt, "first command");
        assert_eq!(
            parsed.commands[0].dedupe_key.as_deref(),
            Some("matrix:$joined-1")
        );
        assert_eq!(parsed.commands[1].room_id, "!joined:matrix.org");
        assert_eq!(parsed.commands[1].sender, "@user-2:matrix.org");
        assert_eq!(parsed.commands[1].prompt, "second command");
        assert_eq!(
            parsed.commands[1].dedupe_key.as_deref(),
            Some("matrix:$joined-2")
        );
    }
}
