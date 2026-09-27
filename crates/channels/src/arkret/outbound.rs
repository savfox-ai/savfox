//! Typed producer authoring for Arkret message submissions.
//!
//! The governing Station assigns acceptance and order. Producer Events carry
//! neither an actor sequence nor a local authority/history checkpoint.

use anyhow::Context;
use arkret::signatures::{EventSigner, SignEventOptions, sign_event};
use arkret::{
    AccountId, ActorId, AuthoredEvent, ContentBlock, Did, DidCoreId, Event, EventId,
    MessageCreatePayload, MessageId, ScopeRef, StrandId, TypedEventDraft, event_spec,
    project_did_to_core_id,
};

use super::sidecar::{SidecarExchangeContext, build_user_facing_response_metadata};

#[derive(Debug, Clone)]
pub struct MessageCreateRequest {
    pub scope_ref: ScopeRef,
    pub strand_id: String,
    pub body: String,
    pub actor_account_id: AccountId,
    pub thread_root_id: Option<MessageId>,
    /// Private exchange identity, validated here and mounted only in encrypted
    /// metadata by the encryption pipeline. It is never an outer Event ref.
    pub sidecar_exchange: Option<SidecarExchangeContext>,
}

/// Build current SDK producer content before encryption and final signing.
pub fn build_message_create_event(req: &MessageCreateRequest) -> anyhow::Result<Event> {
    anyhow::ensure!(
        req.scope_ref.realm_id_opt().is_some(),
        "message requires an existing Realm scope"
    );
    anyhow::ensure!(
        !req.body.trim().is_empty(),
        "MessageCreateRequest has empty body"
    );
    req.actor_account_id
        .validate()
        .context("invalid author AccountId")?;
    if let Some(exchange) = &req.sidecar_exchange {
        anyhow::ensure!(
            matches!(req.scope_ref, ScopeRef::Sidecar { .. }),
            "Sidecar exchange reply requires its exact Sidecar scope"
        );
        build_user_facing_response_metadata(exchange)?;
    }
    let strand = StrandId::new(req.strand_id.clone()).context("invalid message Strand id")?;
    let mut payload = MessageCreatePayload::with_content(
        strand,
        "discussion",
        ContentBlock::text(req.body.clone()),
    );
    if let Some(reply_to) = &req.thread_root_id {
        payload = payload.with_reply_to_id(reply_to.to_string());
    }
    TypedEventDraft::<event_spec::MessageCreate>::new(
        req.scope_ref.clone(),
        ActorId::account(req.actor_account_id.clone()),
        payload,
    )?
    .author_with_digest_suite(
        chrono::Utc::now(),
        req.scope_ref
            .realm_id_opt()
            .expect("existing scope checked")
            .digest_suite_code()
            .digest_suite(),
    )
    .map(AuthoredEvent::into_event)
    .map_err(anyhow::Error::from)
}

/// Finalize the content-bound identity after every payload mutation, including
/// encryption and the private exchange binding.
pub fn finalize_outbound_event(event: Event) -> anyhow::Result<AuthoredEvent> {
    let digest_suite = event.realm_id.digest_suite_code().digest_suite();
    AuthoredEvent::finalize_with_digest_suite(event, digest_suite).map_err(anyhow::Error::from)
}

/// Attach the SDK producer proof only after checking the signer identity.
pub fn sign_outbound_event<S: EventSigner + ?Sized>(
    event: &mut AuthoredEvent,
    signer: &S,
) -> anyhow::Result<()> {
    let controller = signer
        .verification_method()
        .split_once('#')
        .filter(|(_, fragment)| !fragment.is_empty())
        .and_then(|(did, _)| Did::new(did.to_owned()).ok())
        .and_then(|did| project_did_to_core_id(&did).ok());
    anyhow::ensure!(
        controller.as_ref() == Some(event.actual_signer().signing_principal_id()),
        "outbound producer proof signer does not match the Event actor"
    );
    sign_event(event, signer, SignEventOptions::new()).map_err(anyhow::Error::from)?;
    event.verify_producer_proof_self_consistency(event.digest_suite())?;
    Ok(())
}

/// Load the configured key without synthesizing a DID from a stable core id.
pub fn sign_outbound_event_with_key(
    event: &mut AuthoredEvent,
    key_ref: &super::signer::ArkretKeyRef,
    verification_method: &str,
) -> anyhow::Result<()> {
    let signer = arkret::signatures::Ed25519DetachedJwsSigner::new(
        super::signer::load_ed25519_signing_key(key_ref)?,
        verification_method,
    );
    sign_outbound_event(event, &signer)
}

#[cfg(test)]
mod tests {
    use arkret::signatures::Ed25519DetachedJwsSigner;
    use arkret::{RealmId, SidecarId};
    use ed25519_dalek::SigningKey;

    use super::*;

    fn event_id(seed: u8) -> EventId {
        EventId::from_digest(super::super::DIGEST_SUITE, [seed; 32])
    }

    fn valid_request() -> MessageCreateRequest {
        MessageCreateRequest {
            scope_ref: ScopeRef::Realm {
                realm_id: RealmId::from_event_id(&event_id(17)),
            },
            strand_id: StrandId::from_event_id(&event_id(34)).to_string(),
            body: "hello world".to_owned(),
            actor_account_id: AccountId::new(
                DidCoreId::new("ak:did_core:web:agent.example").unwrap(),
                DidCoreId::new("ak:did_core:web:station.example").unwrap(),
            ),
            thread_root_id: None,
            sidecar_exchange: None,
        }
    }

    fn exchange() -> SidecarExchangeContext {
        SidecarExchangeContext {
            exchange_id: "controller.private_exchange~001==".to_owned(),
            request_event_id: event_id(68).to_string(),
            coordinator_assignment_event_id: None,
        }
    }

    #[test]
    fn builds_current_typed_message_without_retired_authority_fields() {
        let request = valid_request();
        let event = build_message_create_event(&request).unwrap();
        assert_eq!(event.kind, "ak.message.create");
        assert_eq!(event.scope_ref, request.scope_ref);
        assert_eq!(
            event.actor_id.as_account_id(),
            Some(&request.actor_account_id)
        );
        assert_eq!(event.payload["content"]["body"], "hello world");
        assert_eq!(event.payload["track_name"], "discussion");
        assert_eq!(event.payload["strand_id"], request.strand_id);
        let wire = serde_json::to_value(&event).unwrap();
        for field in [
            "actor_seq",
            "hlc",
            "refs",
            "prev_refs",
            "auth_context",
            "seal_basis",
            "proofs",
        ] {
            assert!(wire.get(field).is_none(), "retired field {field}");
        }
    }

    #[test]
    fn rejects_genesis_scope_or_invalid_strand_or_empty_body() {
        let mut request = valid_request();
        request.scope_ref = ScopeRef::RealmGenesis;
        assert!(build_message_create_event(&request).is_err());
        request = valid_request();
        request.strand_id.clear();
        assert!(build_message_create_event(&request).is_err());
        request = valid_request();
        request.body = "   ".to_owned();
        assert!(build_message_create_event(&request).is_err());
    }

    #[test]
    fn sidecar_exchange_requires_exact_private_scope() {
        let mut request = valid_request();
        request.sidecar_exchange = Some(exchange());
        let error = build_message_create_event(&request).unwrap_err();
        assert!(error.to_string().contains("exact Sidecar scope"));
        let realm_id = request.scope_ref.realm_id_opt().unwrap().clone();
        request.scope_ref = ScopeRef::Sidecar {
            realm_id,
            sidecar_id: SidecarId::from_event_id(&event_id(85)),
        };
        let event = build_message_create_event(&request).unwrap();
        assert_eq!(event.scope_ref, request.scope_ref);
        assert!(event.semantic_refs.is_empty());
        let wire = serde_json::to_string(&event).unwrap();
        assert!(!wire.contains("sidecar_exchange_binding"));
        assert!(!wire.contains(&exchange().exchange_id));
    }

    #[test]
    fn message_reply_uses_sdk_message_identity() {
        let mut request = valid_request();
        let reply = MessageId::from_event_id(&event_id(102));
        request.thread_root_id = Some(reply.clone());
        let event = build_message_create_event(&request).unwrap();
        assert_eq!(event.payload["reply_to_id"], reply.to_string());
        assert!(event.payload.get("thread_root_id").is_none());
    }

    #[test]
    fn final_producer_proof_covers_mutated_payload_and_final_identity() {
        let mut event = build_message_create_event(&valid_request()).unwrap();
        event.payload.get_mut("content").unwrap()["body"] = serde_json::json!("final body");
        let mut authored = finalize_outbound_event(event).unwrap();
        let signer = Ed25519DetachedJwsSigner::new(
            SigningKey::from_bytes(&[41; 32]),
            "did:web:agent.example#runtime-1",
        );
        sign_outbound_event(&mut authored, &signer).unwrap();
        authored.verify_identity().unwrap();
        let mut changed = authored.into_event();
        changed.payload.get_mut("content").unwrap()["body"] = serde_json::json!("tampered body");
        assert!(
            changed
                .verify_producer_proof_self_consistency(super::super::DIGEST_SUITE)
                .is_err()
        );
    }

    #[test]
    fn wrong_signer_is_rejected_before_attaching_a_proof() {
        let event = build_message_create_event(&valid_request()).unwrap();
        let mut authored = finalize_outbound_event(event).unwrap();
        let signer = Ed25519DetachedJwsSigner::new(
            SigningKey::from_bytes(&[42; 32]),
            "did:web:another-agent.example#runtime-1",
        );
        assert!(sign_outbound_event(&mut authored, &signer).is_err());
        assert!(authored.producer_proof.is_none());
    }

    #[test]
    fn admission_wrapper_rejects_unsigned_and_preserves_exact_signed_event() {
        let inner = arkret::http_client::ClientBuilder::new(
            url::Url::parse("https://station.example").unwrap(),
        )
        .build()
        .unwrap();
        let client = super::super::client::ArkretHttpClient::from_inner(inner);
        let event = build_message_create_event(&valid_request()).unwrap();
        let mut authored = finalize_outbound_event(event).unwrap();
        assert!(client.prepare_submission(&authored).is_err());
        let signer = Ed25519DetachedJwsSigner::new(
            SigningKey::from_bytes(&[43; 32]),
            "did:web:agent.example#runtime-1",
        );
        sign_outbound_event(&mut authored, &signer).unwrap();
        let submission = client.prepare_submission(&authored).unwrap();
        assert_eq!(&submission.event, authored.event());
        assert!(submission.approval_signatures.is_none());
        let wire = serde_json::to_value(submission).unwrap();
        assert_eq!(wire.as_object().unwrap().len(), 1);
        assert!(wire.get("event").is_some());
    }
}
