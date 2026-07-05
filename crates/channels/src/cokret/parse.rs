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
use super::crypto_state::{
    extract_encrypted_payload_from_message_content, message_content_has_encrypted_carrier,
};

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
    pub skipped: Vec<CokretInboundSkippedEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CokretInboundSkippedEvent {
    pub account_id: String,
    pub event_id: Option<String>,
    pub realm_id: Option<String>,
    pub sender_did: Option<String>,
    pub encrypted_payload: Option<cokret_core::EncryptedPayload>,
    pub reason: CokretInboundSkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CokretInboundSkipReason {
    KindNotMessageCreate,
    MissingRequiredField(&'static str),
    EncryptedContent,
    UnsupportedContentKind(String),
    LoopbackFromAccount,
    RealmNotAllowed,
    EmptyBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CokretInboundEventOutcome {
    Dispatchable(CokretInboundEvent),
    Skip(CokretInboundSkippedEvent),
}

/// Extract a `ck.message.create` Event payload into a savfox inbound command.
///
/// Returns `None` for non-message events, redacted/missing content, or
/// unsupported content kinds (anything other than `ck.content.text`).
#[must_use]
pub fn extract_message_event(event: &Value, account_id: &str) -> Option<CokretInboundEvent> {
    match classify_message_event(event, account_id) {
        CokretInboundEventOutcome::Dispatchable(event) => Some(event),
        CokretInboundEventOutcome::Skip(_) => None,
    }
}

/// Classify one Cokret event for the account-mode inbound path.
///
/// Encrypted payload carriers are detected explicitly and reported as
/// [`CokretInboundSkipReason::EncryptedContent`]. Savfox does not yet keep the
/// Cokret MLS/session state required to decrypt these payloads, so callers must
/// fail closed instead of treating them as generic unsupported content.
#[must_use]
pub fn classify_message_event(event: &Value, account_id: &str) -> CokretInboundEventOutcome {
    let Some(kind) = event.get("kind").and_then(Value::as_str) else {
        return skip_event(
            event,
            account_id,
            CokretInboundSkipReason::MissingRequiredField("kind"),
        );
    };
    if kind != "ck.message.create" {
        return skip_event(
            event,
            account_id,
            CokretInboundSkipReason::KindNotMessageCreate,
        );
    }
    let Some(event_id) = event.get("event_id").and_then(Value::as_str) else {
        return skip_event(
            event,
            account_id,
            CokretInboundSkipReason::MissingRequiredField("event_id"),
        );
    };
    let Some(realm_id) = event.get("realm_id").and_then(Value::as_str) else {
        return skip_event(
            event,
            account_id,
            CokretInboundSkipReason::MissingRequiredField("realm_id"),
        );
    };
    let Some(sender_did) = event.get("actor_id").and_then(Value::as_str) else {
        return skip_event(
            event,
            account_id,
            CokretInboundSkipReason::MissingRequiredField("actor_id"),
        );
    };

    let Some(content) = event.get("content") else {
        return skip_event(
            event,
            account_id,
            CokretInboundSkipReason::MissingRequiredField("content"),
        );
    };
    // `content` is the operation payload; in v1 a `ck.message.create` payload
    // wraps `{ message_id, flow_id, track, content: { kind, body, ... } }`.
    let flow_id = content
        .get("flow_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if message_content_has_encrypted_carrier(content) {
        return skip_encrypted_event(event, account_id, content);
    }
    let inner = content.get("content").unwrap_or(content);
    let Some(content_kind) = inner.get("kind").and_then(Value::as_str) else {
        return skip_event(
            event,
            account_id,
            CokretInboundSkipReason::MissingRequiredField("content.kind"),
        );
    };
    if content_kind == "ck.content.encrypted" {
        return skip_encrypted_event(event, account_id, content);
    }
    if content_kind != "ck.content.text" {
        return skip_event(
            event,
            account_id,
            CokretInboundSkipReason::UnsupportedContentKind(content_kind.to_owned()),
        );
    }
    let Some(body) = inner.get("body").and_then(Value::as_str) else {
        return skip_event(
            event,
            account_id,
            CokretInboundSkipReason::MissingRequiredField("content.body"),
        );
    };
    let thread_root_id = inner
        .get("thread_root_id")
        .and_then(Value::as_str)
        .or_else(|| content.get("thread_root_id").and_then(Value::as_str))
        .map(str::to_owned);

    CokretInboundEventOutcome::Dispatchable(CokretInboundEvent {
        account_id: account_id.to_owned(),
        event_id: event_id.to_owned(),
        realm_id: realm_id.to_owned(),
        flow_id,
        sender_did: sender_did.to_owned(),
        body: body.to_owned(),
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
    dispatch_skip_reason(event, account).is_none()
}

fn dispatch_skip_reason(
    event: &CokretInboundEvent,
    account: &CokretAccountConfig,
) -> Option<CokretInboundSkipReason> {
    if event.sender_did.eq_ignore_ascii_case(&account.principal_id) {
        return Some(CokretInboundSkipReason::LoopbackFromAccount);
    }
    if let Some(allowed) = account.default_realm_id.as_deref()
        && !event.realm_id.eq_ignore_ascii_case(allowed)
    {
        return Some(CokretInboundSkipReason::RealmNotAllowed);
    }
    if event.body.trim().is_empty() {
        return Some(CokretInboundSkipReason::EmptyBody);
    }
    None
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
    let mut skipped = Vec::new();
    let Some(realms) = realms_value.as_object() else {
        return CokretInboundParseResult { events, skipped };
    };
    for (_realm_id, realm_body) in realms {
        let timeline = realm_body
            .get("timeline")
            .and_then(|t| t.get("events"))
            .and_then(Value::as_array);
        let Some(timeline) = timeline else { continue };
        for raw_event in timeline {
            match classify_message_event(raw_event, &account.id) {
                CokretInboundEventOutcome::Dispatchable(parsed) => {
                    if let Some(reason) = dispatch_skip_reason(&parsed, account) {
                        skipped.push(CokretInboundSkippedEvent {
                            account_id: parsed.account_id.clone(),
                            event_id: Some(parsed.event_id.clone()),
                            realm_id: Some(parsed.realm_id.clone()),
                            sender_did: Some(parsed.sender_did.clone()),
                            encrypted_payload: None,
                            reason,
                        });
                    } else {
                        events.push(parsed);
                    }
                }
                CokretInboundEventOutcome::Skip(event) => skipped.push(event),
            }
        }
    }
    CokretInboundParseResult { events, skipped }
}

fn skip_event(
    event: &Value,
    account_id: &str,
    reason: CokretInboundSkipReason,
) -> CokretInboundEventOutcome {
    CokretInboundEventOutcome::Skip(CokretInboundSkippedEvent {
        account_id: account_id.to_owned(),
        event_id: event
            .get("event_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        realm_id: event
            .get("realm_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        sender_did: event
            .get("actor_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        encrypted_payload: None,
        reason,
    })
}

fn skip_encrypted_event(
    event: &Value,
    account_id: &str,
    content: &Value,
) -> CokretInboundEventOutcome {
    CokretInboundEventOutcome::Skip(CokretInboundSkippedEvent {
        account_id: account_id.to_owned(),
        event_id: event
            .get("event_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        realm_id: event
            .get("realm_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        sender_did: event
            .get("actor_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        encrypted_payload: extract_encrypted_payload_from_message_content(content),
        reason: CokretInboundSkipReason::EncryptedContent,
    })
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
            login_challenge: None,
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
            "content":{"flow_id":"f","content":{"kind":"ck.content.image","body":""}}
        });
        assert!(extract_message_event(&event, "x").is_none());
        assert_eq!(
            classify_message_event(&event, "x"),
            CokretInboundEventOutcome::Skip(CokretInboundSkippedEvent {
                account_id: "x".into(),
                event_id: Some("e".into()),
                realm_id: Some("r".into()),
                sender_did: Some("a".into()),
                encrypted_payload: None,
                reason: CokretInboundSkipReason::UnsupportedContentKind("ck.content.image".into()),
            })
        );
    }

    #[test]
    fn classifies_encrypted_content_block() {
        let event = json!({
            "kind":"ck.message.create","event_id":"e","realm_id":"r","actor_id":"a",
            "content":{"flow_id":"f","content":{"kind":"ck.content.encrypted","body":""}}
        });
        assert_eq!(
            classify_message_event(&event, "x"),
            CokretInboundEventOutcome::Skip(CokretInboundSkippedEvent {
                account_id: "x".into(),
                event_id: Some("e".into()),
                realm_id: Some("r".into()),
                sender_did: Some("a".into()),
                encrypted_payload: None,
                reason: CokretInboundSkipReason::EncryptedContent,
            })
        );
    }

    #[test]
    fn classifies_spec_encrypted_content_carrier() {
        let event = json!({
            "kind":"ck.message.create","event_id":"e2","realm_id":"r","actor_id":"a",
            "content":{
                "flow_id":"f",
                "encrypted_content":{
                    "scheme":"mls-rfc9420",
                    "ciphertext":"..."
                }
            }
        });
        assert_eq!(
            classify_message_event(&event, "x"),
            CokretInboundEventOutcome::Skip(CokretInboundSkippedEvent {
                account_id: "x".into(),
                event_id: Some("e2".into()),
                realm_id: Some("r".into()),
                sender_did: Some("a".into()),
                encrypted_payload: None,
                reason: CokretInboundSkipReason::EncryptedContent,
            })
        );
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
        assert_eq!(result.skipped.len(), 3);
        assert!(
            result
                .skipped
                .iter()
                .any(|event| event.reason == CokretInboundSkipReason::LoopbackFromAccount)
        );
        assert!(
            result
                .skipped
                .iter()
                .any(|event| event.reason == CokretInboundSkipReason::EmptyBody)
        );
        assert!(
            result
                .skipped
                .iter()
                .any(|event| event.reason == CokretInboundSkipReason::RealmNotAllowed)
        );
    }

    #[test]
    fn parse_delta_records_encrypted_skips() {
        let account = make_account(None);
        let realms = json!({
            "ck:realm:r1": {
                "timeline": {
                    "events": [{
                        "kind":"ck.message.create",
                        "event_id":"ck:event:encrypted",
                        "realm_id":"ck:realm:r1",
                        "actor_id":"did:webvh:bob",
                        "content":{
                            "flow_id":"ck:flow:f1",
                            "encrypted_content":{"scheme":"mls-rfc9420","ciphertext":"..."}
                        }
                    }]
                }
            }
        });
        let result = parse_delta_frame_for_account(&realms, &account);
        assert!(result.events.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(
            result.skipped[0].reason,
            CokretInboundSkipReason::EncryptedContent
        );
    }
}
