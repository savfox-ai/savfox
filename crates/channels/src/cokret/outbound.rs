//! Build a `ck.message.create` Event Envelope for a Cokret actor.
//!
//! Callers with a signer plumbed in should call [`sign_outbound_event`] after
//! `build_message_create_event` and before `Client::events_submit`.
//! Production servers are expected to reject unsigned writes.

use anyhow::Context;
use cokret::signatures::{SignEventOptions, sign_event};
use cokret::{Did, Ed25519MoveSigner, Event, EventRequirements, Hlc, RealmId, new_prefixed_uuid7};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct MessageCreateRequest {
    pub realm_id: String,
    pub flow_id: String,
    pub body: String,
    pub principal_id: String,
    pub actor_seq: u64,
    pub thread_root_id: Option<String>,
}

/// Build an unsigned `ck.message.create` Event Envelope ready to be POSTed to
/// `/api/v1/events`.
///
/// **Caveat:** this returns the envelope with `proofs[]` empty. The server
/// will reject submission if it enforces per-event detached-JWS signing
/// (`event_proofs_empty`); see `_cokret_todos.md` §"不在本阶段做".
pub fn build_message_create_event(req: &MessageCreateRequest) -> anyhow::Result<Event> {
    if req.realm_id.trim().is_empty() {
        anyhow::bail!("MessageCreateRequest missing realm_id");
    }
    if req.flow_id.trim().is_empty() {
        anyhow::bail!("MessageCreateRequest missing flow_id");
    }
    if req.body.trim().is_empty() {
        anyhow::bail!("MessageCreateRequest has empty body");
    }
    let realm = RealmId::new(req.realm_id.clone())
        .with_context(|| format!("invalid realm_id: {}", req.realm_id))?;
    let actor = Did::new(req.principal_id.clone())
        .with_context(|| format!("invalid principal DID: {}", req.principal_id))?;
    let hlc = current_hlc();

    let mut content = json!({
        "message_id": new_prefixed_uuid7("ck:message:"),
        "flow_id": req.flow_id,
        "track": "discussion",
        "content": {
            "kind": "ck.content.text",
            "body": req.body,
        }
    });
    if let Some(thread_root) = &req.thread_root_id
        && let Some(obj) = content.as_object_mut()
    {
        obj.insert(
            "thread_root_id".into(),
            serde_json::Value::String(thread_root.clone()),
        );
    }

    let mut event = Event::new(
        "ck.message.create",
        realm,
        actor,
        req.actor_seq,
        hlc,
        content,
    )
    .map_err(|err| anyhow::anyhow!("failed to build event envelope: {err}"))?;

    // Phase 1: no proofs attached. See module docstring.
    event.requirements = EventRequirements::default();
    Ok(event)
}

/// Phase 8 (T8.C): attach a detached-JWS [`Proof`] to an outbound event.
///
/// Wraps SDK `cokret::signatures::sign_event` (S-1). Same semantics as
/// the applet-mode helper in [`super::applet::sign_outbound_event`].
pub fn sign_outbound_event(
    event: &mut Event,
    signer: &Ed25519MoveSigner,
    verification_method: &str,
) -> anyhow::Result<()> {
    sign_event(
        event,
        signer,
        verification_method,
        SignEventOptions::default(),
    )
    .map_err(|err| anyhow::anyhow!("sign_event failed: {err}"))?;
    Ok(())
}

fn current_hlc() -> Hlc {
    // HLC format: `unix_ms_hex(12) - logical_hex(4) - node_hex(8)`. We don't
    // own a logical clock here, so emit `(now, 0, 00000000)` — Cokret v1
    // tolerates monotonic-by-time stamps from a single emitter.
    let unix_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let value = format!("{unix_ms:012x}-0000-00000000");
    Hlc::new(value).expect("hlc shape validated")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_request() -> MessageCreateRequest {
        MessageCreateRequest {
            realm_id: "ck:realm:01904100-0000-7000-8000-000000000001".into(),
            flow_id: "ck:flow:01904100-0000-7000-8000-000000000001".into(),
            body: "hello world".into(),
            principal_id: "did:webvh:example.org:agents:support".into(),
            actor_seq: 1,
            thread_root_id: None,
        }
    }

    #[test]
    fn builds_basic_message_event() {
        let event = build_message_create_event(&valid_request()).expect("build");
        assert_eq!(event.kind, "ck.message.create");
        assert_eq!(event.realm_id.as_str(), valid_request().realm_id);
        assert_eq!(event.actor_id.as_str(), valid_request().principal_id);
        // content shape sanity
        let body = event
            .content
            .get("content")
            .and_then(|c| c.get("body"))
            .and_then(|b| b.as_str())
            .unwrap_or("");
        assert_eq!(body, "hello world");
    }

    #[test]
    fn rejects_missing_flow_id() {
        let mut req = valid_request();
        req.flow_id = String::new();
        assert!(build_message_create_event(&req).is_err());
    }

    #[test]
    fn rejects_missing_realm_id() {
        let mut req = valid_request();
        req.realm_id = String::new();
        assert!(build_message_create_event(&req).is_err());
    }

    #[test]
    fn rejects_invalid_principal_did() {
        let mut req = valid_request();
        req.principal_id = "not-a-did".into();
        assert!(build_message_create_event(&req).is_err());
    }

    #[test]
    fn rejects_empty_body() {
        let mut req = valid_request();
        req.body = "   ".into();
        assert!(build_message_create_event(&req).is_err());
    }

    #[test]
    fn thread_root_id_appears_when_supplied() {
        let mut req = valid_request();
        req.thread_root_id = Some("ck:event:01H...".into());
        let event = build_message_create_event(&req).expect("build");
        let tr = event.content.get("thread_root_id").and_then(|v| v.as_str());
        assert_eq!(tr, Some("ck:event:01H..."));
    }
}
