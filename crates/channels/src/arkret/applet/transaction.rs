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
    /// `actor_id` did not match the configured `namespaces.actors`.
    ///
    /// **Important**: for an applet, the inbound events generally originate
    /// from native users *sending into a portal Realm*, NOT from the
    /// applet's own ghost actors. We therefore filter primarily on
    /// `realm_id` namespace; the actor filter only kicks in when configured
    /// strictly.
    ActorNotInNamespace,
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
}

impl AppletDispatchSkip {
    /// Closed Arkret reason used at the typed Applet transaction boundary.
    /// The local routing classification remains an implementation detail and
    /// is never serialized through its Rust debug name.
    #[must_use]
    pub const fn reason_code(&self) -> arkret::ReasonCode {
        match self {
            Self::ActorNotInNamespace | Self::RealmNotInNamespace | Self::LoopbackFromApplet => {
                arkret::ReasonCode::AppletNamespaceMismatch
            }
            Self::KindNotMessageCreate => arkret::ReasonCode::UnsupportedEventKind,
            Self::EncryptedContent => arkret::ReasonCode::DecryptionFailed,
            Self::ContentKindUnsupported => arkret::ReasonCode::UnknownKind,
            Self::EmptyBody => arkret::ReasonCode::CardinalityViolation,
        }
    }
}

/// Outcome of parsing a single event from an applet transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppletEventOutcome {
    Dispatch(AppletInboundCommand),
    Skip(AppletDispatchSkip),
}

/// Decide what to do with one Event from an inbound applet transaction.
#[must_use]
pub fn classify_inbound_event(cfg: &ArkretAppletConfig, event: &Event) -> AppletEventOutcome {
    // Loopback: an event signed by our own bot or one of our ghost actors
    // should not be dispatched back to the agent pipeline.
    let actor = event.actor_id.signing_principal_id().as_str();
    if actor == cfg.bot_actor_id || cfg.namespaces.actor_matches(actor) {
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
    if content_kind == "ak.content.encrypted" {
        return AppletEventOutcome::Skip(AppletDispatchSkip::EncryptedContent);
    }
    if content_kind != "ak.content.text" {
        return AppletEventOutcome::Skip(AppletDispatchSkip::ContentKindUnsupported);
    }
    let body = content.body.trim().to_owned();
    if body.is_empty() {
        return AppletEventOutcome::Skip(AppletDispatchSkip::EmptyBody);
    }

    AppletEventOutcome::Dispatch(AppletInboundCommand {
        event_id: event.event_id.as_str().to_owned(),
        realm_id: realm.to_owned(),
        strand_id: payload.strand_id.into_string(),
        sender_did: actor.to_owned(),
        body,
        thread_root_id: payload.reply_to_id,
    })
}

#[cfg(test)]
mod tests {
    use arkret::{DidCoreId, Hlc, RealmId, ScopeRef};
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
            bot_actor_id: "did:web:bridge.example:bot".into(),
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
                // (e.g. `slack:team:T123:channel:C456`) to internal
                // `ak:realm:<uuid>` ids and filters inbound on the internal id.
                // For test setup we match a known uuid prefix family.
                realms: vec![NamespacePattern::exclusive("ak:realm:01904100-**")],
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
            grant_event_path: None,
        }
    }

    fn realm(id: &str) -> RealmId {
        RealmId::new(id.to_owned()).expect("realm id")
    }
    fn hlc() -> Hlc {
        Hlc::new("000000000000-0000-00000000").expect("test HLC should parse")
    }
    fn did(s: &str) -> DidCoreId {
        DidCoreId::new(s.to_owned()).expect("did")
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
            did("did:webvh:z6mkfixture:principal-server.example"),
            1,
            hlc(),
            content,
        )
        .expect("event new")
    }

    fn text_content(body: &str) -> serde_json::Value {
        json!({
            "strand_id": "ak:strand:01904100-0000-8000-8000-000000000001",
            "track_name": "discussion",
            "content": { "kind": "ak.content.text", "body": body },
        })
    }

    #[test]
    fn dispatches_text_message_in_realm_namespace() {
        let ev = make_event(
            "did:webvh:acme:alice",
            "ak:realm:01904100-0000-8000-8000-000000000123",
            "ak.message.create",
            text_content("hello"),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        match outcome {
            AppletEventOutcome::Dispatch(cmd) => {
                assert_eq!(
                    cmd.realm_id,
                    "ak:realm:01904100-0000-8000-8000-000000000123"
                );
                assert!(matches!(cmd.body.as_str(), "hello"));
                assert_eq!(cmd.sender_did, "did:webvh:acme:alice");
                assert_eq!(cmd.body, "hello");
                assert_eq!(
                    cmd.strand_id,
                    "ak:strand:01904100-0000-8000-8000-000000000001"
                );
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn skips_events_outside_realm_namespace() {
        let ev = make_event(
            "did:webvh:acme:alice",
            "ak:realm:99999999-0000-8000-8000-000000000abc",
            "ak.message.create",
            text_content("hi"),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::RealmNotInNamespace)
        );
    }

    #[test]
    fn skips_loopback_from_ghost_actor() {
        let ev = make_event(
            "did:web:bridge.example:ghost:u1",
            "ak:realm:01904100-0000-8000-8000-000000000456",
            "ak.message.create",
            text_content("loopback"),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::LoopbackFromApplet)
        );
    }

    #[test]
    fn skips_loopback_from_bot() {
        let ev = make_event(
            "did:web:bridge.example:bot",
            "ak:realm:01904100-0000-8000-8000-000000000456",
            "ak.message.create",
            text_content("loopback"),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::LoopbackFromApplet)
        );
    }

    #[test]
    fn skips_non_text_content() {
        let ev = make_event(
            "did:webvh:acme:alice",
            "ak:realm:01904100-0000-8000-8000-000000000456",
            "ak.message.create",
            json!({
                "strand_id": "ak:strand:01904100-0000-8000-8000-000000000001",
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
    fn skips_encrypted_content_block() {
        let ev = make_event(
            "did:webvh:acme:alice",
            "ak:realm:01904100-0000-8000-8000-000000000456",
            "ak.message.create",
            json!({
                "strand_id": "ak:strand:01904100-0000-8000-8000-000000000001",
                "track_name": "discussion",
                "content": { "kind": "ak.content.encrypted", "body": "" }
            }),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::EncryptedContent)
        );
    }

    #[test]
    fn skips_spec_encrypted_content_carrier() {
        let ev = make_event(
            "did:webvh:acme:alice",
            "ak:realm:01904100-0000-8000-8000-000000000456",
            "ak.message.create",
            json!({
                "strand_id": "ak:strand:01904100-0000-8000-8000-000000000001",
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
            "did:webvh:acme:alice",
            "ak:realm:01904100-0000-8000-8000-000000000456",
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
            "did:webvh:acme:alice",
            "ak:realm:01904100-0000-8000-8000-000000000456",
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
