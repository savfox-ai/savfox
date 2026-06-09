//! Build outbound `ck.message.create` Event Envelopes attributed to a
//! Ghost Actor.
//!
//! Spec [`applet-integration.md`
//! §8](../../../../../../cokret/cokret-spec/spec/v1/zh/extensions/applet-integration.md)
//! requires every applet-written Event to carry `actor_id`, applet provenance,
//! `authorization_ref`, and `proof`.
//!
//! The current SDK envelope exposes no signed top-level `applet_id` or
//! `external_ref` slots, so this helper carries that bridge provenance in
//! `unsigned` transport metadata.
//!
//! Phase 8 (T8.C): caller signs the envelope via
//! [`sign_outbound_event`] before submitting. Callers that do not yet have
//! a signer plumbed in (bearer-only Phase 1-7 deployments) can still
//! submit unsigned events at their own risk — production servers will
//! reject with `event_proofs_empty`.

use anyhow::Context;
use cokret::Ed25519MoveSigner;
use cokret::signatures::{SignEventOptions, sign_event};
use cokret_core::Event;
use cokret_identifiers::{Did, Hlc, RealmId, new_prefixed_uuid7};
use serde_json::{Value, json};

/// Inputs for an applet-attributed `ck.message.create` event.
#[derive(Debug, Clone)]
pub struct AppletMessageRequest {
    /// Stable applet id (`ck:applet:<uuidv7>`).
    pub applet_id: String,
    /// Target Realm where the message lands.
    pub realm_id: String,
    /// Target discussion Flow.
    pub flow_id: String,
    /// Ghost actor DID — the visible author of the message.
    pub ghost_actor_did: String,
    /// Message body (plain text for Phase 6).
    pub body: String,
    /// External-origin reference (protocol/network/external_id).
    pub external_ref: Value,
    /// `ck:grant:<uuidv7>` granting the ghost actor permission to write to
    /// this realm/flow. Set if available; omitted otherwise.
    pub authorization_ref: Option<String>,
    /// When the event is delegated (Applet acting *on behalf of* a native
    /// user), this is the executing agent / applet DID. For pure ghost
    /// writes (the ghost is the author), leave `None`.
    pub executed_by: Option<String>,
    /// Monotonic per-actor sequence number. Caller maintains this.
    pub actor_seq: u64,
    /// Optional thread root event id.
    pub thread_root_id: Option<String>,
}

/// Build an unsigned `ck.message.create` Event Envelope attributed to a
/// Ghost Actor.
///
/// Returned Event is suitable for `Client::events_submit` once a server
/// is available. `proofs[]` is empty (TODO: detached JWS) — for an
/// MLS / production deployment the receiver will reject; for development
/// against a permissive server it is enough to exercise the path.
pub fn build_applet_message_event(req: &AppletMessageRequest) -> anyhow::Result<Event> {
    if req.applet_id.trim().is_empty() {
        anyhow::bail!("AppletMessageRequest missing applet_id");
    }
    if req.realm_id.trim().is_empty() {
        anyhow::bail!("AppletMessageRequest missing realm_id");
    }
    if req.flow_id.trim().is_empty() {
        anyhow::bail!("AppletMessageRequest missing flow_id");
    }
    if req.body.trim().is_empty() {
        anyhow::bail!("AppletMessageRequest has empty body");
    }
    if !req.ghost_actor_did.starts_with("did:") {
        anyhow::bail!(
            "AppletMessageRequest ghost_actor_did must be a DID URI, got '{}'",
            req.ghost_actor_did
        );
    }
    let realm = RealmId::new(req.realm_id.clone())
        .with_context(|| format!("invalid realm_id: {}", req.realm_id))?;
    let actor = Did::new(req.ghost_actor_did.clone())
        .with_context(|| format!("invalid ghost actor DID: {}", req.ghost_actor_did))?;
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
        obj.insert("thread_root_id".into(), Value::String(thread_root.clone()));
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

    event
        .unsigned
        .insert("applet_id".to_owned(), Value::String(req.applet_id.clone()));
    event
        .unsigned
        .insert("external_ref".to_owned(), req.external_ref.clone());

    if let Some(grant) = &req.authorization_ref {
        event.authorization_ref = Some(grant.clone());
    }
    if let Some(exec) = &req.executed_by {
        event.executed_by = Some(
            Did::new(exec.clone()).with_context(|| format!("invalid executed_by DID: {exec}"))?,
        );
    }

    // Phase 6: no proofs attached. See module docstring.
    Ok(event)
}

/// Phase 8 (T8.C): attach a detached-JWS [`Proof`] to an outbound event using
/// the supplied Ed25519 signer.
///
/// Wraps SDK `cokret::signatures::sign_event` (S-1). The receiver's
/// reducer validates `proof.event_digest == event.event_digest()` and
/// the JWS signature — so the produced event satisfies the
/// `event_proofs_empty` and `proof_verification_failed` checks that
/// reject any Phase 6/7 unsigned envelope.
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
    let unix_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let value = format!("{unix_ms:012x}-0000-00000000");
    Hlc::new(value).expect("hlc shape validated")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn valid() -> AppletMessageRequest {
        AppletMessageRequest {
            applet_id: "ck:applet:21532600-0000-7000-8000-000000000000".into(),
            realm_id: "ck:realm:01904100-0000-7000-8000-000000000001".into(),
            flow_id: "ck:flow:01904100-0000-7000-8000-000000000001".into(),
            ghost_actor_did: "did:web:slack-bridge.example:ghost:u123".into(),
            body: "hi from Slack".into(),
            external_ref: json!({
                "protocol": "slack",
                "network_id": "T123",
                "external_id": "U1"
            }),
            authorization_ref: Some("ck:grant:01904100-0000-7000-8000-000000000099".into()),
            executed_by: None,
            actor_seq: 7,
            thread_root_id: None,
        }
    }

    #[test]
    fn builds_event_with_ghost_actor_id() {
        let ev = build_applet_message_event(&valid()).expect("build");
        assert_eq!(ev.kind, "ck.message.create");
        assert_eq!(
            ev.actor_id.as_str(),
            "did:web:slack-bridge.example:ghost:u123"
        );
        assert_eq!(
            ev.realm_id.as_str(),
            "ck:realm:01904100-0000-7000-8000-000000000001"
        );
    }

    #[test]
    fn sets_applet_id_unsigned_metadata() {
        let ev = build_applet_message_event(&valid()).expect("build");
        assert_eq!(
            ev.unsigned.get("applet_id").and_then(Value::as_str),
            Some("ck:applet:21532600-0000-7000-8000-000000000000")
        );
    }

    #[test]
    fn sets_external_ref_unsigned_metadata() {
        let ev = build_applet_message_event(&valid()).expect("build");
        let ext = ev.unsigned.get("external_ref").expect("present");
        assert_eq!(ext["protocol"], "slack");
        assert_eq!(ext["network_id"], "T123");
    }

    #[test]
    fn authorization_ref_promoted_to_envelope_field() {
        let ev = build_applet_message_event(&valid()).expect("build");
        assert_eq!(
            ev.authorization_ref.as_deref(),
            Some("ck:grant:01904100-0000-7000-8000-000000000099")
        );
    }

    #[test]
    fn executed_by_promoted_when_set() {
        let mut req = valid();
        req.executed_by = Some("did:web:bridge.example:bot".into());
        let ev = build_applet_message_event(&req).expect("build");
        assert_eq!(
            ev.executed_by.as_ref().map(|d| d.as_str()),
            Some("did:web:bridge.example:bot")
        );
    }

    #[test]
    fn rejects_missing_flow_id() {
        let mut req = valid();
        req.flow_id = String::new();
        assert!(build_applet_message_event(&req).is_err());
    }

    #[test]
    fn rejects_non_did_ghost_actor() {
        let mut req = valid();
        req.ghost_actor_did = "not-a-did".into();
        assert!(build_applet_message_event(&req).is_err());
    }

    #[test]
    fn rejects_empty_body() {
        let mut req = valid();
        req.body = "  ".into();
        assert!(build_applet_message_event(&req).is_err());
    }

    #[test]
    fn rejects_missing_applet_id() {
        let mut req = valid();
        req.applet_id = String::new();
        assert!(build_applet_message_event(&req).is_err());
    }

    #[test]
    fn signed_event_validates_proof_bindings() {
        // Build an event, sign it, then assert the SDK's
        // `Event::validate_proof_bindings()` accepts the result.
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD_NO_PAD;

        use crate::cokret::signer::{CokretKeyRef, load_ed25519_signer};

        const SEED: [u8; 32] = [9; 32];
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "savfox-cokret-test-sign-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, STANDARD_NO_PAD.encode(SEED)).expect("write");
        let key_ref = CokretKeyRef::File { path: path.clone() };
        let did = "did:web:slack-bridge.example:ghost:u123";
        let vm = "did:web:slack-bridge.example:ghost:u123#key-1";
        let signer = load_ed25519_signer(&key_ref, did, vm).expect("load");
        let _ = std::fs::remove_file(&path);

        let mut ev = build_applet_message_event(&valid()).expect("build");
        sign_outbound_event(&mut ev, &signer, vm).expect("sign");
        assert_eq!(ev.proofs.len(), 1);
        ev.validate_proof_bindings()
            .expect("proof should bind to event digest");
    }

    #[test]
    fn thread_root_id_passes_through() {
        let mut req = valid();
        req.thread_root_id = Some("ck:event:abc".into());
        let ev = build_applet_message_event(&req).expect("build");
        assert_eq!(
            ev.content.get("thread_root_id").and_then(Value::as_str),
            Some("ck:event:abc")
        );
    }
}
