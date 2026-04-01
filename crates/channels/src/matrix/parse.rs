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

#[allow(clippy::print_stdout)]
fn debug_matrix_message_ignored(room_id: &str, sender: &str, reason: &str) {
    println!(
        "[matrix][inbound] ignored room={} sender={} reason={}",
        room_id, sender, reason
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

fn parse_message_event_internal(
    event: &Value,
    room_id_hint: Option<&str>,
    self_user_id: Option<&str>,
    allow_plain_text: bool,
) -> Option<MatrixCommandEvent> {
    if event.get("type").and_then(Value::as_str) != Some("m.room.message") {
        return None;
    }
    let room_id = event
        .get("room_id")
        .and_then(Value::as_str)
        .or(room_id_hint)
        .unwrap_or("");
    let sender = event.get("sender").and_then(Value::as_str).unwrap_or("");
    let content = event.get("content").unwrap_or(&Value::Null);
    let msgtype = content.get("msgtype").and_then(Value::as_str).unwrap_or("");
    if msgtype != "m.text" {
        debug_matrix_message_ignored(room_id, sender, "unsupported_msgtype");
        return None;
    }
    let text = content.get("body").and_then(Value::as_str).unwrap_or("");
    debug_matrix_inbound_message(room_id, sender, text);
    let prompt = match extract_prompt(content, self_user_id) {
        Some(prompt) => prompt,
        None if allow_plain_text => {
            let prompt = text.trim();
            if prompt.is_empty() {
                debug_matrix_message_ignored(room_id, sender, "empty_body");
                return None;
            }
            prompt.to_string()
        }
        None => {
            debug_matrix_message_ignored(room_id, sender, "no_prefix_or_bot_mention");
            return None;
        }
    };
    if room_id.is_empty() {
        debug_matrix_message_ignored(room_id, sender, "missing_room_id");
        return None;
    }
    let dedupe_key = event
        .get("event_id")
        .and_then(Value::as_str)
        .map(|id| format!("matrix:{id}"));
    Some(MatrixCommandEvent {
        room_id: room_id.to_owned(),
        sender: sender.to_owned(),
        prompt,
        dedupe_key,
    })
}

fn extract_prefixed_prompt(text: &str) -> Option<String> {
    let prompt = text
        .trim()
        .strip_prefix("!savfox ")
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())?;
    Some(prompt.to_string())
}

fn matrix_localpart(user_id: &str) -> Option<&str> {
    let trimmed = user_id.trim();
    let without_at = trimmed.strip_prefix('@').unwrap_or(trimmed);
    let localpart = without_at.split(':').next()?.trim();
    if localpart.is_empty() {
        None
    } else {
        Some(localpart)
    }
}

fn mention_candidates(user_id: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let trimmed = user_id.trim();
    if !trimmed.is_empty() {
        candidates.push(trimmed.to_string());
        candidates.push(trimmed.trim_start_matches('@').to_string());
    }
    if let Some(localpart) = matrix_localpart(trimmed) {
        candidates.push(localpart.to_string());
        candidates.push(format!("@{localpart}"));
    }
    candidates.sort_unstable();
    candidates.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    candidates
}

fn split_leading_candidate<'a>(text: &'a str, candidate: &str) -> Option<&'a str> {
    let text = text.trim_start();
    let prefix = text.get(..candidate.len())?;
    if !prefix.eq_ignore_ascii_case(candidate) {
        return None;
    }

    let remainder = text.get(candidate.len()..).unwrap_or("");
    if remainder.is_empty() {
        return Some(remainder);
    }

    let first = remainder.chars().next()?;
    if first.is_whitespace() || matches!(first, ':' | ',' | ';' | '>') {
        Some(remainder)
    } else {
        None
    }
}

fn cleanup_stripped_prompt(text: &str) -> Option<String> {
    let prompt = text
        .trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, ':' | ',' | ';' | '>'))
        .trim();
    if prompt.is_empty() {
        None
    } else {
        Some(prompt.to_string())
    }
}

fn content_mentions_user(content: &Value, self_user_id: &str) -> bool {
    content
        .get("m.mentions")
        .and_then(|mentions| mentions.get("user_ids"))
        .and_then(Value::as_array)
        .map(|user_ids| {
            user_ids
                .iter()
                .filter_map(Value::as_str)
                .any(|user_id| user_id.eq_ignore_ascii_case(self_user_id))
        })
        .unwrap_or(false)
}

fn strip_html_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn extract_prompt_from_formatted_body(formatted_body: &str, self_user_id: &str) -> Option<String> {
    let trimmed = formatted_body.trim_start();
    if !trimmed.starts_with("<a ") {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    let self_user_id = self_user_id.trim().to_ascii_lowercase();
    if !lower.contains(&self_user_id) {
        return None;
    }

    let anchor_end = trimmed.find("</a>")?;
    let remainder = trimmed.get(anchor_end + 4..).unwrap_or("");
    let remainder = strip_html_tags(remainder);
    cleanup_stripped_prompt(&remainder)
}

fn extract_prompt_from_bot_mention(content: &Value, self_user_id: &str) -> Option<String> {
    let body = content.get("body").and_then(Value::as_str).unwrap_or("");
    for candidate in mention_candidates(self_user_id) {
        if let Some(remainder) = split_leading_candidate(body, &candidate)
            && let Some(prompt) = cleanup_stripped_prompt(remainder)
        {
            return Some(prompt);
        }
    }

    if content_mentions_user(content, self_user_id)
        && let Some(formatted_body) = content.get("formatted_body").and_then(Value::as_str)
    {
        return extract_prompt_from_formatted_body(formatted_body, self_user_id);
    }

    None
}

fn extract_prompt(content: &Value, self_user_id: Option<&str>) -> Option<String> {
    let body = content.get("body").and_then(Value::as_str).unwrap_or("");
    if let Some(prompt) = extract_prefixed_prompt(body) {
        return Some(prompt);
    }

    let self_user_id = self_user_id?;
    extract_prompt_from_bot_mention(content, self_user_id)
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
    parse_command_event_for_user(event, room_id_hint, None)
}

pub fn parse_command_event_for_user(
    event: &Value,
    room_id_hint: Option<&str>,
    self_user_id: Option<&str>,
) -> Option<MatrixCommandEvent> {
    parse_message_event_internal(event, room_id_hint, self_user_id, false)
}

pub fn parse_appservice_message_event_for_user(
    event: &Value,
    self_user_id: Option<&str>,
) -> Option<MatrixCommandEvent> {
    parse_message_event_internal(event, None, self_user_id, true)
}

pub fn parse_inbound_payload(payload: &Value) -> MatrixInboundParseResult {
    parse_inbound_payload_for_user(payload, None)
}

pub fn parse_inbound_payload_for_user(
    payload: &Value,
    self_user_id: Option<&str>,
) -> MatrixInboundParseResult {
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

        if let Some(command) = parse_command_event_for_user(event, room_id_hint, self_user_id) {
            parsed.commands.push(command);
        }
    }

    parsed
}

pub fn parse_webhook_payload(payload: &Value) -> MatrixWebhookParseResult {
    parse_webhook_payload_for_user(payload, None)
}

pub fn parse_webhook_payload_for_user(
    payload: &Value,
    self_user_id: Option<&str>,
) -> MatrixWebhookParseResult {
    let parsed = parse_inbound_payload_for_user(payload, self_user_id);
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
    use savfox_core::channel::ChannelAction;
    use serde_json::json;

    use super::{
        MatrixWebhookParseResult, parse_appservice_message_event_for_user, parse_inbound_payload,
        parse_inbound_payload_for_user, parse_webhook_payload, parse_webhook_payload_for_user,
    };

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

    #[test]
    fn parses_leading_localpart_mentions_for_authenticated_user() {
        let payload = json!({
            "rooms": {
                "join": {
                    "!joined:matrix.org": {
                        "timeline": {
                            "events": [
                                {
                                    "type": "m.room.message",
                                    "event_id": "$mention-1",
                                    "sender": "@user:matrix.org",
                                    "content": {
                                        "msgtype": "m.text",
                                        "body": "savfox_bot: hi",
                                        "m.mentions": {
                                            "user_ids": ["@savfox_bot:127.0.0.1:6006"]
                                        }
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        });

        let parsed = parse_inbound_payload_for_user(&payload, Some("@savfox_bot:127.0.0.1:6006"));
        assert_eq!(parsed.commands.len(), 1);
        assert_eq!(parsed.commands[0].room_id, "!joined:matrix.org");
        assert_eq!(parsed.commands[0].sender, "@user:matrix.org");
        assert_eq!(parsed.commands[0].prompt, "hi");
        assert_eq!(
            parsed.commands[0].dedupe_key.as_deref(),
            Some("matrix:$mention-1")
        );
    }

    #[test]
    fn parses_formatted_body_mentions_for_authenticated_user() {
        let payload = json!({
            "room_id": "!joined:matrix.org",
            "type": "m.room.message",
            "event_id": "$mention-2",
            "sender": "@user:matrix.org",
            "content": {
                "msgtype": "m.text",
                "body": "Savvy Fox: hi",
                "format": "org.matrix.custom.html",
                "formatted_body": "<a href=\"https://matrix.to/#/@savfox_bot:127.0.0.1:6006\">Savvy Fox</a>: hi",
                "m.mentions": {
                    "user_ids": ["@savfox_bot:127.0.0.1:6006"]
                }
            }
        });

        let parsed = parse_webhook_payload_for_user(&payload, Some("@savfox_bot:127.0.0.1:6006"));
        assert_eq!(parsed.dedupe_key.as_deref(), Some("matrix:$mention-2"));
        assert_start_thread(&parsed, "!joined:matrix.org", "hi");
    }

    #[test]
    fn mention_events_without_authenticated_user_still_require_prefix() {
        let payload = json!({
            "room_id": "!joined:matrix.org",
            "type": "m.room.message",
            "event_id": "$mention-3",
            "sender": "@user:matrix.org",
            "content": {
                "msgtype": "m.text",
                "body": "savfox_bot: hi",
                "m.mentions": {
                    "user_ids": ["@savfox_bot:127.0.0.1:6006"]
                }
            }
        });

        let parsed = parse_inbound_payload(&payload);
        assert!(parsed.commands.is_empty());

        let parsed = parse_webhook_payload(&payload);
        assert!(matches!(parsed.action, ChannelAction::Ignore));
    }

    #[test]
    fn appservice_messages_accept_plain_text_without_prefix() {
        let event = json!({
            "type": "m.room.message",
            "room_id": "!joined:matrix.org",
            "event_id": "$appservice-1",
            "sender": "@user:matrix.org",
            "content": {
                "msgtype": "m.text",
                "body": "hello"
            }
        });

        let parsed = parse_appservice_message_event_for_user(
            &event,
            Some("@_savfox_default:127.0.0.1:6006"),
        )
        .expect("plain text appservice message should parse");
        assert_eq!(parsed.room_id, "!joined:matrix.org");
        assert_eq!(parsed.sender, "@user:matrix.org");
        assert_eq!(parsed.prompt, "hello");
        assert_eq!(parsed.dedupe_key.as_deref(), Some("matrix:$appservice-1"));
    }

    #[test]
    fn appservice_messages_still_strip_leading_mention_when_present() {
        let event = json!({
            "type": "m.room.message",
            "room_id": "!joined:matrix.org",
            "event_id": "$appservice-2",
            "sender": "@user:matrix.org",
            "content": {
                "msgtype": "m.text",
                "body": "_savfox_default: hi there",
                "m.mentions": {
                    "user_ids": ["@_savfox_default:127.0.0.1:6006"]
                }
            }
        });

        let parsed = parse_appservice_message_event_for_user(
            &event,
            Some("@_savfox_default:127.0.0.1:6006"),
        )
        .expect("mention appservice message should parse");
        assert_eq!(parsed.prompt, "hi there");
    }
}
