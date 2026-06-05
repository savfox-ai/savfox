//! Parse Cokret `ck.message.create` Event Envelopes into savfox internal
//! commands.
//!
//! Frame-level parsing (NDJSON `Delta` / `CatchupComplete` / etc.) is provided
//! by the Cokret SDK
//! (`cokret_core::AccountSubscribeFrame` / [`AccountSubscribeFrame::from_ndjson_line`]).
//! This module owns the "given an Event payload, decide whether to dispatch and
//! how" logic.

use serde_json::Value;

use super::config::CokretAccountConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CokretInboundEvent {
    pub account_id: String,
    pub event_id: String,
    pub realm_id: String,
    pub flow_id: Option<String>,
    pub sender_did: String,
    pub body: String,
    pub thread_root_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CokretInboundParseResult {
    pub events: Vec<CokretInboundEvent>,
}

/// Extract a `ck.message.create` Event payload into a savfox inbound command.
///
/// Returns `None` for non-message events, redacted/missing content, or
/// unsupported content kinds (anything other than `ck.content.text`).
#[must_use]
pub fn extract_message_event(event: &Value, account_id: &str) -> Option<CokretInboundEvent> {
    let kind = event.get("kind").and_then(Value::as_str)?;
    if kind != "ck.message.create" {
        return None;
    }
    let event_id = event.get("event_id").and_then(Value::as_str)?.to_owned();
    let realm_id = event.get("realm_id").and_then(Value::as_str)?.to_owned();
    let sender_did = event.get("actor_id").and_then(Value::as_str)?.to_owned();

    let content = event.get("content")?;
    // `content` is the operation payload; in v1 a `ck.message.create` payload
    // wraps `{ message_id, flow_id, track, content: { kind, body, ... } }`.
    let flow_id = content
        .get("flow_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let inner = content.get("content").unwrap_or(content);
    let content_kind = inner.get("kind").and_then(Value::as_str)?;
    if content_kind != "ck.content.text" {
        return None;
    }
    let body = inner.get("body").and_then(Value::as_str)?.to_owned();
    let thread_root_id = inner
        .get("thread_root_id")
        .and_then(Value::as_str)
        .or_else(|| content.get("thread_root_id").and_then(Value::as_str))
        .map(str::to_owned);

    Some(CokretInboundEvent {
        account_id: account_id.to_owned(),
        event_id,
        realm_id,
        flow_id,
        sender_did,
        body,
        thread_root_id,
    })
}

/// Decide whether the parsed event should be dispatched to the agent pipeline.
///
/// Rules:
/// * Drop messages sent by the listening account itself (loop guard).
/// * Drop messages whose realm is not in the account's allow set (when the account specifies one
///   via `default_realm_id`); accounts without a `default_realm_id` accept any realm.
#[must_use]
pub fn should_dispatch_event(event: &CokretInboundEvent, account: &CokretAccountConfig) -> bool {
    if event.sender_did.eq_ignore_ascii_case(&account.principal_id) {
        return false;
    }
    if let Some(allowed) = account.default_realm_id.as_deref()
        && !event.realm_id.eq_ignore_ascii_case(allowed)
    {
        return false;
    }
    !event.body.trim().is_empty()
}

/// Walk a `Delta` frame body's `realms` object and extract every dispatchable
/// `ck.message.create` event for the given account.
///
/// The shape expected is the v1 account subscribe `delta` shape:
/// `realms.<realm_id>.timeline.events[] : Event`. Tolerates missing nested
/// fields by returning an empty list rather than failing.
#[must_use]
pub fn parse_delta_frame_for_account(
    realms_value: &Value,
    account: &CokretAccountConfig,
) -> CokretInboundParseResult {
    let mut events = Vec::new();
    let Some(realms) = realms_value.as_object() else {
        return CokretInboundParseResult { events };
    };
    for (_realm_id, realm_body) in realms {
        let timeline = realm_body
            .get("timeline")
            .and_then(|t| t.get("events"))
            .and_then(Value::as_array);
        let Some(timeline) = timeline else { continue };
        for raw_event in timeline {
            let Some(parsed) = extract_message_event(raw_event, &account.id) else {
                continue;
            };
            if should_dispatch_event(&parsed, account) {
                events.push(parsed);
            }
        }
    }
    CokretInboundParseResult { events }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn make_account(realm: Option<&str>) -> CokretAccountConfig {
        CokretAccountConfig {
            id: "support".into(),
            principal_id: "did:webvh:example.org:agents:support".into(),
            device_id: "ck:device:01".into(),
            access_token: "t".into(),
            key_ref: None,
            verification_method: None,
            cokret_server_did: None,
            grant_event_path: None,
            default_realm_id: realm.map(str::to_owned),
            default_flow_id: None,
            agent_id: Some("support".into()),
            listen: true,
            send: true,
            scopes: vec![],
        }
    }

    fn message_event(actor: &str, body: &str) -> Value {
        message_event_in_realm("ck:realm:r1", actor, body)
    }

    fn message_event_in_realm(realm: &str, actor: &str, body: &str) -> Value {
        json!({
            "event_id": format!("ck:event:{}-{}", realm, actor),
            "kind": "ck.message.create",
            "realm_id": realm,
            "actor_id": actor,
            "content": {
                "message_id": "ck:message:m1",
                "flow_id": "ck:flow:f1",
                "track": "discussion",
                "content": {
                    "kind": "ck.content.text",
                    "body": body
                }
            }
        })
    }

    #[test]
    fn extracts_text_message() {
        let event = message_event("did:webvh:example.org:user-alice", "hello");
        let parsed = extract_message_event(&event, "support").expect("parse");
        assert_eq!(parsed.body, "hello");
        assert_eq!(parsed.realm_id, "ck:realm:r1");
        assert_eq!(parsed.flow_id.as_deref(), Some("ck:flow:f1"));
        assert_eq!(parsed.sender_did, "did:webvh:example.org:user-alice");
    }

    #[test]
    fn ignores_non_message_kind() {
        let event = json!({"kind":"ck.flow.update","event_id":"e","realm_id":"r","actor_id":"a","content":{}});
        assert!(extract_message_event(&event, "x").is_none());
    }

    #[test]
    fn ignores_non_text_content() {
        let event = json!({
            "kind":"ck.message.create","event_id":"e","realm_id":"r","actor_id":"a",
            "content":{"flow_id":"f","content":{"kind":"ck.content.encrypted","body":""}}
        });
        assert!(extract_message_event(&event, "x").is_none());
    }

    #[test]
    fn should_dispatch_filters_self_messages() {
        let account = make_account(Some("ck:realm:r1"));
        let event = message_event(&account.principal_id, "echo");
        let parsed =
            extract_message_event(&event, &account.id).expect("message event should parse");
        assert!(!should_dispatch_event(&parsed, &account));
    }

    #[test]
    fn should_dispatch_filters_non_allowed_realm() {
        let account = make_account(Some("ck:realm:other"));
        let event = message_event("did:webvh:other", "hi");
        let parsed =
            extract_message_event(&event, &account.id).expect("message event should parse");
        assert!(!should_dispatch_event(&parsed, &account));
    }

    #[test]
    fn should_dispatch_filters_empty_body() {
        let account = make_account(None);
        let event = message_event("did:webvh:other", "   ");
        let parsed =
            extract_message_event(&event, &account.id).expect("message event should parse");
        assert!(!should_dispatch_event(&parsed, &account));
    }

    #[test]
    fn parse_delta_frame_walks_realms() {
        let account = make_account(Some("ck:realm:r1"));
        let realms = json!({
            "ck:realm:r1": {
                "timeline": {
                    "events": [
                        message_event("did:webvh:bob", "hello bob"),
                        message_event(&account.principal_id, "self echo"),
                        message_event("did:webvh:carol", "")
                    ]
                }
            },
            "ck:realm:other": {
                "timeline": {
                    "events": [message_event_in_realm("ck:realm:other", "did:webvh:dan", "from other realm")]
                }
            }
        });
        let result = parse_delta_frame_for_account(&realms, &account);
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].sender_did, "did:webvh:bob");
        assert_eq!(result.events[0].body, "hello bob");
    }
}
