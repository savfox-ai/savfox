//! Build a `ak.message.create` Event Envelope for an Arkret actor.
//!
//! Callers with a signer plumbed in should call [`sign_outbound_event`] after
//! `build_message_create_event` and before `Client::events_submit`.
//! Production servers are expected to reject unsigned writes.

use anyhow::Context;
use arkret::events::EventKind;
use arkret::signatures::{SignEventOptions, sign_event};
use arkret::{
    AuthContext, ContentBlock, Did, DidUrl, Ed25519PayloadSigner, Event, EventDraftKindRegistry,
    EventId, EventRef, Hlc, MessageCreatePayload, OperationEnvelopeBuilder,
    OperationEventConversion, OperationId, RealmId, ScopeRef, SealId, StrandId, new_prefixed_uuid7,
};

use super::sidecar::{EVENT_REF_ROLE_AFTER, SidecarExchangeContext};

#[derive(Debug, Clone)]
pub struct MessageCreateRequest {
    pub realm_id: String,
    pub strand_id: String,
    pub body: String,
    pub principal_id: String,
    pub actor_seq: u64,
    pub thread_root_id: Option<String>,
    /// When replying inside an Agent Sidecar exchange, the verified exchange
    /// identity from the inbound request Event. The built Event carries a
    /// top-level `refs[role=after]` to the request Event id
    /// (`zh/models/sidecar.md` §7.2.1 item 2); the encrypted
    /// `role=user_facing_response` binding itself is mounted at encryption
    /// time (`encrypted_metadata`), never in plaintext payload fields.
    pub sidecar_exchange: Option<SidecarExchangeContext>,
}

/// Build an unsigned `ak.message.create` Event Envelope ready to be POSTed to
/// `/api/v1/events`.
///
/// **Caveat:** this returns the envelope with `proofs[]` empty. The server
/// will reject submission if it enforces per-event detached-JWS signing
/// (`event_proofs_empty`); see `_arkret_todos.md` §"不在本阶段做".
pub fn build_message_create_event(req: &MessageCreateRequest) -> anyhow::Result<Event> {
    if req.realm_id.trim().is_empty() {
        anyhow::bail!("MessageCreateRequest missing realm_id");
    }
    if req.strand_id.trim().is_empty() {
        anyhow::bail!("MessageCreateRequest missing strand_id");
    }
    if req.body.trim().is_empty() {
        anyhow::bail!("MessageCreateRequest has empty body");
    }
    let realm = RealmId::new(req.realm_id.clone())
        .with_context(|| format!("invalid realm_id: {}", req.realm_id))?;
    let actor = Did::new(req.principal_id.clone())
        .with_context(|| format!("invalid principal DID: {}", req.principal_id))?;
    let hlc = current_hlc();

    let strand_id = StrandId::new(req.strand_id.clone())
        .with_context(|| format!("invalid strand_id: {}", req.strand_id))?;
    let content = ContentBlock::text(req.body.clone());
    let mut payload = MessageCreatePayload::with_content(strand_id, "discussion", content);
    if let Some(thread_root) = &req.thread_root_id {
        payload = payload.with_reply_to(thread_root.clone());
    }
    let envelope = OperationEnvelopeBuilder::new(
        OperationId::new(new_prefixed_uuid7("ak:operation:"))
            .context("failed to generate message operation id")?,
        ScopeRef::Realm { realm_id: realm },
        actor,
        EventKind::MESSAGE_CREATE,
        req.actor_seq,
        hlc,
    )
    .with_payload(
        payload
            .to_value()
            .map_err(|err| anyhow::anyhow!("failed to build message-create payload: {err}"))?,
    )
    .build(&EventDraftKindRegistry::default())
    .map_err(|err| anyhow::anyhow!("failed to build message-create envelope: {err}"))?;
    let mut conversion = OperationEventConversion::default();
    if let Some(exchange) = &req.sidecar_exchange {
        // Sidecar exchange Events MUST reference the accepted request Event
        // via a top-level `refs[role=after]` (zh/models/sidecar.md §7.2.1).
        let request_event_id = EventId::new(exchange.request_event_id.clone())
            .map_err(|err| anyhow::anyhow!("invalid Sidecar request Event id: {err}"))?;
        conversion.refs.push(EventRef::new(
            request_event_id.to_string(),
            EVENT_REF_ROLE_AFTER,
        ));
    }
    envelope
        .into_event_envelope(conversion)
        .map_err(|err| anyhow::anyhow!("failed to build event envelope: {err}"))
}

/// Phase 8 (T8.C): attach a detached-JWS [`Proof`] to an outbound event.
///
/// Wraps SDK `arkret::signatures::sign_event` (S-1). Same semantics as
/// the applet-mode helper in [`super::applet::sign_outbound_event`].
pub fn sign_outbound_event(
    event: &mut Event,
    signer: &Ed25519PayloadSigner,
    verification_method: &str,
) -> anyhow::Result<()> {
    let verification_method = DidUrl::new(verification_method.to_owned()).map_err(|err| {
        anyhow::anyhow!("invalid verification method '{verification_method}': {err}")
    })?;
    sign_event(
        event,
        signer,
        &verification_method,
        SignEventOptions::default(),
    )
    .map_err(|err| anyhow::anyhow!("sign_event failed: {err}"))?;
    Ok(())
}

/// Stamp the accepted Realm Seal and signing identity required by the CBA
/// DataEvent shape before encryption and signing.
pub fn apply_data_event_basis(
    event: &mut Event,
    seal_id: SealId,
    signer_did: Did,
    key_id: String,
) -> anyhow::Result<()> {
    if event.seal_basis.is_some() || !event.preconditions.is_empty() {
        anyhow::bail!("DataEvent cannot carry seal_basis or preconditions");
    }
    event.seal_ref = Some(seal_id);
    event.auth_context = Some(AuthContext {
        did: signer_did,
        key_id,
        key_epoch: 0,
        credential_epoch: None,
    });
    Ok(())
}

fn current_hlc() -> Hlc {
    // HLC format: `unix_ms_hex(12) - logical_hex(4) - node_hex(8)`. We don't
    // own a logical clock here, so emit `(now, 0, 00000000)` — Arkret v1
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
            realm_id: "ak:realm:01904100-0000-8000-8000-000000000001".into(),
            strand_id: "ak:strand:01904100-0000-8000-8000-000000000001".into(),
            body: "hello world".into(),
            principal_id: "did:webvh:example.org:agents:support".into(),
            actor_seq: 1,
            thread_root_id: None,
            sidecar_exchange: None,
        }
    }

    #[test]
    fn builds_basic_message_event() {
        let event = build_message_create_event(&valid_request()).expect("build");
        assert_eq!(event.kind, "ak.message.create");
        assert_eq!(event.realm_id.as_str(), valid_request().realm_id);
        assert_eq!(event.actor_id.as_str(), valid_request().principal_id);
        // payload shape sanity
        let body = event
            .payload
            .get("content")
            .and_then(|c| c.get("body"))
            .and_then(|b| b.as_str())
            .unwrap_or("");
        assert_eq!(body, "hello world");
        assert_eq!(
            event
                .payload
                .get("track_name")
                .and_then(serde_json::Value::as_str),
            Some("discussion")
        );
        assert_eq!(
            event
                .payload
                .get("strand_id")
                .and_then(serde_json::Value::as_str),
            Some(valid_request().strand_id.as_str())
        );
        assert!(event.payload.get("track").is_none());
    }

    #[test]
    fn data_event_basis_is_explicit_before_signing() {
        let mut event = build_message_create_event(&valid_request()).expect("build");
        apply_data_event_basis(
            &mut event,
            SealId::new(format!("ak:seal:sha256:{}", "11".repeat(32))).unwrap(),
            Did::new(valid_request().principal_id).unwrap(),
            "agent-device".to_owned(),
        )
        .expect("basis");

        assert!(event.seal_ref.is_some());
        let auth_context = event.auth_context.expect("auth context");
        assert_eq!(auth_context.key_id, "agent-device");
        assert_eq!(auth_context.did.as_str(), valid_request().principal_id);
        assert!(event.seal_basis.is_none());
    }

    #[test]
    fn uses_strand_id_for_sdk_payload() {
        let mut req = valid_request();
        req.strand_id = "ak:strand:01904100-0000-8000-8000-000000000002".into();
        let event = build_message_create_event(&req).expect("build");
        assert_eq!(
            event
                .payload
                .get("strand_id")
                .and_then(serde_json::Value::as_str),
            Some("ak:strand:01904100-0000-8000-8000-000000000002")
        );
    }

    #[test]
    fn rejects_missing_strand_id() {
        let mut req = valid_request();
        req.strand_id = String::new();
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
    fn sidecar_exchange_adds_after_ref_to_request_event() {
        let request_event_id = "ak:event:01904100-0000-8000-8000-000000000031";
        let mut req = valid_request();
        req.sidecar_exchange = Some(SidecarExchangeContext {
            exchange_id: "01904100-0000-7000-8000-0000000000aa".into(),
            request_event_id: request_event_id.into(),
            coordinator_assignment_event_id: None,
        });
        let event = build_message_create_event(&req).expect("build");
        assert!(
            event
                .refs
                .iter()
                .any(|r| r.role == EVENT_REF_ROLE_AFTER && r.id == request_event_id),
            "expected refs[role=after] -> request event id, got {:?}",
            event.refs
        );
        // The binding itself is mounted only into encrypted_metadata at
        // encryption time; the plaintext payload must not carry it.
        assert!(event.payload.get("metadata").is_none());
        assert!(event.payload.get("encrypted_metadata").is_none());
    }

    #[test]
    fn sidecar_exchange_rejects_invalid_request_event_id() {
        let mut req = valid_request();
        req.sidecar_exchange = Some(SidecarExchangeContext {
            exchange_id: "01904100-0000-7000-8000-0000000000aa".into(),
            request_event_id: "not-an-event-id".into(),
            coordinator_assignment_event_id: None,
        });
        assert!(build_message_create_event(&req).is_err());
    }

    #[test]
    fn thread_root_id_maps_to_sdk_reply_to_when_supplied() {
        let mut req = valid_request();
        req.thread_root_id = Some("ak:event:01H...".into());
        let event = build_message_create_event(&req).expect("build");
        let tr = event.payload.get("reply_to").and_then(|v| v.as_str());
        assert_eq!(tr, Some("ak:event:01H..."));
        assert!(event.payload.get("thread_root_id").is_none());
    }
}
