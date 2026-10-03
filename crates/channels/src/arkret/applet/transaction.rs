//! Inbound transaction parsing.
//!
//! When the Arkret server pushes events via
//! `POST /_arkret/edge/applet/transactions`, the body is an
//! [`AppletTransactionRequestBody`] (SDK type). This module converts each
//! contained Event into a savfox-side [`AppletInboundCommand`] when it
//! matches the configured namespaces and looks dispatchable.

use arkret::{Event, EventPayloadExt as _};

use super::super::crypto_state::message_content_has_encrypted_carrier;
use super::config::ArkretAppletConfig;
use super::namespace::AppletNamespacesExt;

/// One dispatchable command extracted from an inbound applet transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppletInboundCommand {
    /// Wire `event_id` of the source event — used for dedupe + tracing.
    pub event_id: String,
    /// Realm the event was emitted in.
    pub realm_id: String,
    pub scope_ref: arkret::ScopeRef,
    /// Discussion strand id from the typed `ak.message.create` payload.
    pub strand_id: String,
    /// Sender DID (native human, native bot, or ghost actor — caller
    /// decides what to do; for an applet this will usually be a *native*
    /// user, since the Arkret server pushes traffic destined for the
    /// applet's namespaces).
    pub sender_did: String,
    /// Extracted text body (currently only `ak.content.text` is handled).
    pub body: String,
    /// Optional thread root.
    pub thread_root_id: Option<String>,
}

/// Reason a given event was filtered out of the dispatch path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppletDispatchSkip {
    /// `realm_id` did not match the configured `namespaces.realms`.
    RealmNotInNamespace,
    /// The event's `kind` is not `ak.message.create` (we only dispatch text
    /// messages in Phase 6).
    KindNotMessageCreate,
    /// The event carries `encrypted_content` / `encrypted_payload` or an
    /// encrypted content block. Savfox does not yet maintain Arkret crypto
    /// session state, so applet inbound decrypt must fail closed.
    EncryptedContent,
    /// `content.kind` is not `ak.content.text`.
    ContentKindUnsupported,
    /// `content.body` is missing or empty.
    EmptyBody,
    /// Event came from the applet's own bot or one of its ghost actors —
    /// don't loop back into the agent pipeline.
    LoopbackFromApplet,
    /// The Event's authoring actor could not be resolved to a DID, so its
    /// position relative to `namespaces.actors` cannot be decided.
    ///
    /// `applet-integration.md` §3.4 forbids matching a DID namespace pattern
    /// against a `did_core_id`, and §7 makes the receiver of
    /// `ak.edge.applet.command.transaction.v1` verify the verification-method
    /// projection before it verifies the namespace. With no verified DID the
    /// only sound answer is to refuse dispatch: guessing would reopen exactly
    /// the loopback the actor namespace exists to close.
    ActorDidUnresolved,
}

impl AppletDispatchSkip {
    /// Closed Arkret reason used at the typed Applet transaction boundary.
    /// The local routing classification remains an implementation detail and
    /// is never serialized through its Rust debug name.
    #[must_use]
    pub const fn reason_code(&self) -> arkret::ReasonCode {
        match self {
            Self::RealmNotInNamespace
            | Self::LoopbackFromApplet
            // `applet-integration.md` §7.3 tells a receiver that cannot line an
            // Event's actor up with the registration to fail closed and pick
            // the code of the failing layer; the layer here is the namespace.
            | Self::ActorDidUnresolved => arkret::ReasonCode::AppletNamespaceMismatch,
            Self::KindNotMessageCreate => arkret::ReasonCode::UnsupportedEventKind,
            Self::EncryptedContent => arkret::ReasonCode::DecryptionPending,
            // Valid content that this dispatcher cannot execute is a local
            // feature refusal, not an unknown protocol Event kind.
            Self::ContentKindUnsupported | Self::EmptyBody => {
                arkret::ReasonCode::UnsupportedFeature
            }
        }
    }
}

/// Outcome of parsing a single event from an applet transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppletEventOutcome {
    Dispatch(AppletInboundCommand),
    Skip(AppletDispatchSkip),
}

/// Resolve the Event's authoring signer back to its canonical resolvable DID.
///
/// A registration declares `namespaces.actors` as DID patterns, and
/// `applet-integration.md` §3.4 is explicit that a namespace matches the
/// method-evidence-verified `initial_resolution.did` and **must not** match a
/// DID pattern against the `did_core_id` adapter projection. That projection is
/// lossy — `did:webvh:<scid>:<host>:<path>` collapses to
/// `ak:did_core:webvh:<scid>` — so it cannot be inverted either.
///
/// The one DID-form identifier an inbound Event carries is its producer proof
/// verification method, and §7's `ak.edge.applet.command.transaction.v1` row
/// makes the receiver verify that "VM projection" before it verifies the
/// namespace. So the signer DID is the proof's verification-method controller,
/// accepted only when [`arkret::project_did_to_core_id`] maps it back to the
/// Event's own signing principal. `executed_by` is the signer whenever it is
/// present, matching the SDK's own producer derivation.
fn producer_signer_did(event: &Event) -> Option<arkret::Did> {
    let producer_proof = event.producer_proof.as_ref()?;
    event
        .verify_producer_proof_self_consistency(event.realm_id.digest_suite_code().digest_suite())
        .ok()?;
    let (controller, _fragment) = producer_proof
        .verification_method
        .as_str()
        .split_once('#')?;
    let controller = arkret::Did::new(controller.to_owned()).ok()?;
    let signer = event.executed_by.as_ref().unwrap_or(&event.actor_id);
    (&arkret::project_did_to_core_id(&controller).ok()? == signer.signing_principal_id())
        .then_some(controller)
}

/// Decide what to do with one Event from an inbound applet transaction.
#[must_use]
pub fn classify_inbound_event(cfg: &ArkretAppletConfig, event: &Event) -> AppletEventOutcome {
    // Loopback, exact arm: the Bot is a single long-lived principal, so its
    // complete account compares byte-for-byte and needs no DID resolution.
    let actor = event.actor_id.signing_principal_id().as_str();
    let signer = event.executed_by.as_ref().unwrap_or(&event.actor_id);
    if signer.as_account_id() == Some(&cfg.bot_account_id) {
        return AppletEventOutcome::Skip(AppletDispatchSkip::LoopbackFromApplet);
    }

    if event.kind != "ak.message.create" {
        return AppletEventOutcome::Skip(AppletDispatchSkip::KindNotMessageCreate);
    }

    // Realm namespace filter (primary filter for portal-Realm inbound).
    let realm = event.realm_id.as_str();
    if !cfg.namespaces.realm_matches(realm) {
        return AppletEventOutcome::Skip(AppletDispatchSkip::RealmNotInNamespace);
    }

    if message_content_has_encrypted_carrier(&event.payload) {
        return AppletEventOutcome::Skip(AppletDispatchSkip::EncryptedContent);
    }
    let Ok(payload) = event.as_message_create() else {
        return AppletEventOutcome::Skip(AppletDispatchSkip::ContentKindUnsupported);
    };
    let Some(content) = payload.content else {
        return AppletEventOutcome::Skip(AppletDispatchSkip::ContentKindUnsupported);
    };
    let content_kind = content.kind.as_str();
    if content_kind != "ak.content.text" {
        return AppletEventOutcome::Skip(AppletDispatchSkip::ContentKindUnsupported);
    }
    let body = content.body.trim().to_owned();
    if body.is_empty() {
        return AppletEventOutcome::Skip(AppletDispatchSkip::EmptyBody);
    }

    // Loopback, namespace arm. This is the last gate before dispatch so the
    // only Event that ever reaches the agent pipeline is one whose author was
    // positively attributed to a DID outside this registration's own actor
    // namespace. An unresolvable signer fails closed here rather than being
    // treated as foreign.
    let Some(signer_did) = producer_signer_did(event) else {
        return AppletEventOutcome::Skip(AppletDispatchSkip::ActorDidUnresolved);
    };
    if cfg.namespaces.actor_matches(signer_did.as_str()) {
        return AppletEventOutcome::Skip(AppletDispatchSkip::LoopbackFromApplet);
    }

    AppletEventOutcome::Dispatch(AppletInboundCommand {
        event_id: event.event_id.as_str().to_owned(),
        realm_id: realm.to_owned(),
        scope_ref: event.scope_ref.clone(),
        strand_id: payload.strand_id.into_string(),
        sender_did: actor.to_owned(),
        body,
        thread_root_id: payload.reply_to_id,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use arkret::{DidCoreId, RealmId, ScopeRef};
    use serde_json::json;

    use super::*;
    use crate::arkret::applet::namespace::{AppletNamespaces, NamespacePattern};

    fn cfg() -> ArkretAppletConfig {
        ArkretAppletConfig {
            id: "applet-1".into(),
            applet_id: "ak:applet:1".into(),
            service_id: "did:web:bridge.example".into(),
            controller_principal_id: "did:webvh:acme:admin".into(),
            base_url: "https://savfox.example/applet".into(),
            bot_account_id: arkret::AccountId::new(
                arkret::DidCoreId::new("ak:did_core:web:bridge.example:bot").unwrap(),
                arkret::DidCoreId::new("ak:did_core:webvh:z6mkstation").unwrap(),
            ),
            service_did: arkret::Did::new("did:webvh:z6mkbridge:bridge.example").unwrap(),
            trust_domain: arkret::TrustDomainId::new("ak:trust_domain:example.net").unwrap(),
            managed_actor_authoring: super::super::config::ManagedActorAuthoringSettings {
                principal_endpoint: "https://actors.example".into(),
                key_encryption_key_hex: "22".repeat(32),
            },
            device_id: None,
            arkret_server_url: "https://arkret.example.org".into(),
            arkret_server_did: Some("did:webvh:arkret.example.org".into()),
            trusted_verification_methods: Vec::new(),
            login_challenge: None,
            arkret_bearer_token: None,
            namespaces: AppletNamespaces {
                actors: vec![NamespacePattern::exclusive(
                    "did:web:bridge.example:ghost:*",
                )],
                // In production Arkret, the Applet maps external aliases
                // (e.g. `slack:team:T123:channel:C456`) to the internal
                // Event-derived Realm id and filters inbound on that id.
                realms: vec![
                    NamespacePattern::exclusive(REALM_IN_A.as_str()),
                    NamespacePattern::exclusive(REALM_IN_B.as_str()),
                ],
                handles: vec![],
            },
            protocols: vec!["slack".into()],
            ghost_did_prefix: "ghost:".into(),
            requested_scopes: vec![],
            receive_events: true,
            receive_ephemeral: false,
            rate_limited: true,
            authorization_grant_id: None,
            registration_epoch: None,
            key_ref: None,
            verification_method: None,
            signer_resolution_evidence_ref: None,
            grant_event_path: None,
        }
    }

    #[test]
    fn bot_loopback_uses_the_complete_account() {
        let mut config = cfg();
        config.namespaces.actors.clear();
        let mut event = arkret_wire::test_support::raw_event(
            "ak.message.create",
            ScopeRef::Realm {
                realm_id: REALM_IN_A.clone(),
            },
            config.bot_account_id.principal_id.clone(),
            config.bot_account_id.station_id.clone(),
            serde_json::json!({"body":"hello"}),
        )
        .unwrap();
        assert_eq!(
            classify_inbound_event(&config, &event),
            AppletEventOutcome::Skip(AppletDispatchSkip::LoopbackFromApplet)
        );
        let mut foreign = config.bot_account_id.clone();
        foreign.station_id = arkret::DidCoreId::new("ak:did_core:web:other.example").unwrap();
        event.actor_id = arkret::ActorId::account(foreign);
        assert_ne!(
            classify_inbound_event(&config, &event),
            AppletEventOutcome::Skip(AppletDispatchSkip::LoopbackFromApplet)
        );
    }

    fn fixture_event_id(seed: u8) -> arkret::EventId {
        arkret::EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [seed; 32])
    }

    /// Realms the fixture Applet declares in its namespace.
    static REALM_IN_A: LazyLock<RealmId> =
        LazyLock::new(|| RealmId::from_event_id(&fixture_event_id(0x31)));
    static REALM_IN_B: LazyLock<RealmId> =
        LazyLock::new(|| RealmId::from_event_id(&fixture_event_id(0x32)));
    /// Realm deliberately outside the declared namespace.
    static REALM_OUT: LazyLock<RealmId> =
        LazyLock::new(|| RealmId::from_event_id(&fixture_event_id(0x33)));
    static STRAND: LazyLock<arkret::StrandId> =
        LazyLock::new(|| arkret::StrandId::from_event_id(&fixture_event_id(0x34)));

    fn realm(id: &str) -> RealmId {
        RealmId::new(id.to_owned()).expect("realm id")
    }
    fn did(s: &str) -> DidCoreId {
        DidCoreId::new(s.to_owned()).expect("did")
    }

    /// Sign the fixture through the same one-shot SDK proof seam as production.
    fn attach_producer_proof(event: &mut Event, verification_method: &str) {
        let mut authored = arkret::AuthoredEvent::finalize_with_digest_suite(
            event.clone(),
            event.realm_id.digest_suite_code().digest_suite(),
        )
        .expect("finalized fixture");
        let signer = arkret::signatures::Ed25519DetachedJwsSigner::new(
            ed25519_dalek::SigningKey::from_bytes(&[71; 32]),
            verification_method,
        );
        arkret::signatures::sign_event(
            &mut authored,
            &signer,
            arkret::signatures::SignEventOptions::new(),
        )
        .expect("signed fixture");
        *event = authored.into_event();
    }

    fn signed_event(
        actor: &str,
        verification_method: &str,
        realm_id: &str,
        kind: &str,
        content: serde_json::Value,
    ) -> Event {
        let mut event = make_event(actor, realm_id, kind, content);
        attach_producer_proof(&mut event, verification_method);
        event
    }

    fn make_event(actor: &str, realm_id: &str, kind: &str, content: serde_json::Value) -> Event {
        // `Event::new` derives `event_id` from the Event's own content; an id
        // can no longer be minted for it.
        arkret_wire::test_support::raw_event(
            kind,
            ScopeRef::Realm {
                realm_id: realm(realm_id),
            },
            did(actor),
            did("ak:did_core:webvh:z6mkfixtureserver"),
            content,
        )
        .expect("event new")
    }

    fn text_content(body: &str) -> serde_json::Value {
        json!({
            "strand_id": STRAND.as_str(),
            "track_name": "discussion",
            "content": { "kind": "ak.content.text", "body": body },
        })
    }

    #[test]
    fn dispatches_text_message_in_realm_namespace() {
        let ev = signed_event(
            "ak:did_core:webvh:z6mkfixturealice",
            "did:webvh:z6mkfixturealice:alice.example#key-1",
            REALM_IN_A.as_str(),
            "ak.message.create",
            text_content("hello"),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        match outcome {
            AppletEventOutcome::Dispatch(cmd) => {
                assert_eq!(cmd.realm_id, REALM_IN_A.as_str());
                assert!(matches!(cmd.body.as_str(), "hello"));
                assert_eq!(cmd.sender_did, "ak:did_core:webvh:z6mkfixturealice");
                assert_eq!(cmd.body, "hello");
                assert_eq!(cmd.strand_id, STRAND.as_str());
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn skips_events_outside_realm_namespace() {
        let ev = make_event(
            "ak:did_core:webvh:z6mkfixturealice",
            REALM_OUT.as_str(),
            "ak.message.create",
            text_content("hi"),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::RealmNotInNamespace)
        );
    }

    /// `applet-integration.md` §3.4: the actor namespace matches the resolvable
    /// DID, never the `did_core_id` adapter projection. The ghost's DID reaches
    /// the classifier through its producer proof verification method, whose
    /// controller must project back onto the Event's own signing principal.
    #[test]
    fn skips_loopback_from_ghost_actor() {
        let ev = signed_event(
            "ak:did_core:web:bridge.example:ghost:u1",
            "did:web:bridge.example:ghost:u1#key-1",
            REALM_IN_B.as_str(),
            "ak.message.create",
            text_content("loopback"),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::LoopbackFromApplet)
        );
    }

    /// A ghost DID is not recoverable from the `did_core_id` the Event carries
    /// (`did:webvh:<scid>:<host>:<path>` projects down to the SCID alone), so an
    /// Event whose signer cannot be resolved must fail closed instead of being
    /// treated as a foreign actor and dispatched.
    #[test]
    fn refuses_dispatch_when_the_signer_did_is_unresolvable() {
        let unsigned = make_event(
            "ak:did_core:webvh:z6mkfixturealice",
            REALM_IN_A.as_str(),
            "ak.message.create",
            text_content("hello"),
        );
        assert_eq!(
            classify_inbound_event(&cfg(), &unsigned),
            AppletEventOutcome::Skip(AppletDispatchSkip::ActorDidUnresolved)
        );

        // A proof whose controller projects onto some *other* principal does
        // not attribute this Event either.
        let mismatched = signed_event(
            "ak:did_core:webvh:z6mkfixturealice",
            "did:webvh:z6mkfixturemallory:mallory.example#key-1",
            REALM_IN_A.as_str(),
            "ak.message.create",
            text_content("hello"),
        );
        assert_eq!(
            classify_inbound_event(&cfg(), &mismatched),
            AppletEventOutcome::Skip(AppletDispatchSkip::ActorDidUnresolved)
        );
    }

    /// The Bot arm is exact and covers delegated authoring: when the Applet's
    /// own Bot is the `executed_by` signer the Event is ours, whoever the
    /// `actor_id` names.
    #[test]
    fn skips_loopback_when_the_bot_is_the_delegated_signer() {
        let config = cfg();
        let mut ev = signed_event(
            "ak:did_core:webvh:z6mkfixturealice",
            "did:webvh:z6mkfixturealice:alice.example#key-1",
            REALM_IN_A.as_str(),
            "ak.message.create",
            text_content("delegated"),
        );
        ev.executed_by = Some(arkret::ActorId::account(config.bot_account_id.clone()));
        assert_eq!(
            classify_inbound_event(&config, &ev),
            AppletEventOutcome::Skip(AppletDispatchSkip::LoopbackFromApplet)
        );
    }

    #[test]
    fn skips_loopback_from_bot() {
        let config = cfg();
        let ev = arkret_wire::test_support::raw_event(
            "ak.message.create",
            ScopeRef::Realm {
                realm_id: REALM_IN_B.clone(),
            },
            config.bot_account_id.principal_id.clone(),
            config.bot_account_id.station_id.clone(),
            text_content("loopback"),
        )
        .expect("event new");
        let outcome = classify_inbound_event(&config, &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::LoopbackFromApplet)
        );
    }

    #[test]
    fn skips_non_text_content() {
        let ev = make_event(
            "ak:did_core:webvh:z6mkfixturealice",
            REALM_IN_B.as_str(),
            "ak.message.create",
            json!({
                "strand_id": STRAND.as_str(),
                "track_name": "discussion",
                "content": { "kind": "ak.content.image", "ref": "ak:blob:..." }
            }),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::ContentKindUnsupported)
        );
    }

    #[test]
    fn skips_spec_encrypted_content_carrier() {
        let ev = make_event(
            "ak:did_core:webvh:z6mkfixturealice",
            REALM_IN_B.as_str(),
            "ak.message.create",
            json!({
                "strand_id": STRAND.as_str(),
                "track_name": "discussion",
                "encrypted_content": {
                    "scheme": "mls_rfc9420",
                    "ciphertext": "..."
                }
            }),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::EncryptedContent)
        );
    }

    #[test]
    fn skips_empty_body() {
        let ev = make_event(
            "ak:did_core:webvh:z6mkfixturealice",
            REALM_IN_B.as_str(),
            "ak.message.create",
            text_content("   "),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::EmptyBody)
        );
    }

    #[test]
    fn skips_non_message_kind() {
        let ev = make_event(
            "ak:did_core:webvh:z6mkfixturealice",
            REALM_IN_B.as_str(),
            "ak.strand.create",
            json!({"title": "irrelevant"}),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::KindNotMessageCreate)
        );
    }
}
