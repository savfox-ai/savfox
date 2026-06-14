//! Inbound transaction parsing.
//!
//! When the Cokret server pushes events via
//! `POST /_cokret/edge/applet/transactions`, the body is an
//! [`AppletTransactionRequestBody`] (SDK type). This module converts each
//! contained Event into a savfox-side [`AppletInboundCommand`] when it
//! matches the configured namespaces and looks dispatchable.

use cokret_core::Event;

use super::config::CokretAppletConfig;

/// One dispatchable command extracted from an inbound applet transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppletInboundCommand {
    /// Wire `event_id` of the source event — used for dedupe + tracing.
    pub event_id: String,
    /// Realm the event was emitted in.
    pub realm_id: String,
    /// Discussion flow id, extracted from `content.flow_id` when present.
    pub flow_id: Option<String>,
    /// Sender DID (native human, native bot, or ghost actor — caller
    /// decides what to do; for an applet this will usually be a *native*
    /// user, since the Cokret server pushes traffic destined for the
    /// applet's namespaces).
    pub sender_did: String,
    /// Extracted text body (currently only `ck.content.text` is handled).
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
    /// The event's `kind` is not `ck.message.create` (we only dispatch text
    /// messages in Phase 6).
    KindNotMessageCreate,
    /// `content.kind` is not `ck.content.text`.
    ContentKindUnsupported,
    /// `content.body` is missing or empty.
    EmptyBody,
    /// Event came from the applet's own bot or one of its ghost actors —
    /// don't loop back into the agent pipeline.
    LoopbackFromApplet,
}

/// Outcome of parsing a single event from an applet transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppletEventOutcome {
    Dispatch(AppletInboundCommand),
    Skip(AppletDispatchSkip),
}

/// Decide what to do with one Event from an inbound applet transaction.
#[must_use]
pub fn classify_inbound_event(cfg: &CokretAppletConfig, event: &Event) -> AppletEventOutcome {
    // Loopback: an event signed by our own bot or one of our ghost actors
    // should not be dispatched back to the agent pipeline.
    let actor = event.actor_id.as_str();
    if actor == cfg.bot_actor_id || cfg.namespaces.actor_matches(actor) {
        return AppletEventOutcome::Skip(AppletDispatchSkip::LoopbackFromApplet);
    }

    if event.kind != "ck.message.create" {
        return AppletEventOutcome::Skip(AppletDispatchSkip::KindNotMessageCreate);
    }

    // Realm namespace filter (primary filter for portal-Realm inbound).
    let realm = event.realm_id.as_str();
    if !cfg.namespaces.realm_matches(realm) {
        return AppletEventOutcome::Skip(AppletDispatchSkip::RealmNotInNamespace);
    }

    let content = &event.content;
    let content_kind = content
        .get("content")
        .and_then(|c| c.get("kind"))
        .and_then(|k| k.as_str());
    if content_kind != Some("ck.content.text") {
        return AppletEventOutcome::Skip(AppletDispatchSkip::ContentKindUnsupported);
    }
    let body = content
        .get("content")
        .and_then(|c| c.get("body"))
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    if body.is_empty() {
        return AppletEventOutcome::Skip(AppletDispatchSkip::EmptyBody);
    }

    let flow_id = content
        .get("flow_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let thread_root_id = content
        .get("thread_root_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    AppletEventOutcome::Dispatch(AppletInboundCommand {
        event_id: event.event_id.as_str().to_owned(),
        realm_id: realm.to_owned(),
        flow_id,
        sender_did: actor.to_owned(),
        body,
        thread_root_id,
    })
}

#[cfg(test)]
mod tests {
    use cokret_identifiers::{Did, Hlc, RealmId, new_prefixed_uuid7};
    use serde_json::json;

    use super::*;
    use crate::cokret::applet::namespace::{AppletNamespaces, NamespacePattern};

    fn cfg() -> CokretAppletConfig {
        CokretAppletConfig {
            id: "applet-1".into(),
            applet_id: "ck:applet:1".into(),
            service_did: "did:web:bridge.example".into(),
            controller_did: "did:webvh:acme:admin".into(),
            base_url: "https://savfox.example/applet".into(),
            bot_actor_id: "did:web:bridge.example:bot".into(),
            cokret_server_url: "https://cokret.example.org".into(),
            cokret_server_did: Some("did:webvh:cokret.example.org".into()),
            login_challenge: None,
            cokret_bearer_token: None,
            namespaces: AppletNamespaces {
                actors: vec![NamespacePattern::new(
                    "did:web:bridge.example:ghost:*",
                    true,
                )],
                // In production Cokret, the Applet maps external aliases
                // (e.g. `slack:team:T123:channel:C456`) to internal
                // `ck:realm:<uuid>` ids and filters inbound on the internal id.
                // For test setup we match a known uuid prefix family.
                realms: vec![NamespacePattern::new("ck:realm:01904100-**", true)],
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
    fn did(s: &str) -> Did {
        Did::new(s.to_owned()).expect("did")
    }

    fn make_event(actor: &str, realm_id: &str, kind: &str, content: serde_json::Value) -> Event {
        let mut ev =
            Event::new(kind, realm(realm_id), did(actor), 1, hlc(), content).expect("event new");
        // Replace event_id with a predictable one for tests by minting a fresh uuidv7.
        let ev_id_s = new_prefixed_uuid7("ck:event:");
        ev.event_id = cokret_identifiers::EventId::new(ev_id_s).expect("event id");
        ev
    }

    fn text_content(body: &str) -> serde_json::Value {
        json!({
            "message_id": new_prefixed_uuid7("ck:message:"),
            "flow_id": "ck:flow:01904100-0000-7000-8000-000000000001",
            "track": "discussion",
            "content": { "kind": "ck.content.text", "body": body },
        })
    }

    #[test]
    fn dispatches_text_message_in_realm_namespace() {
        let ev = make_event(
            "did:webvh:acme:alice",
            "ck:realm:01904100-0000-7000-8000-000000000123",
            "ck.message.create",
            text_content("hello"),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        match outcome {
            AppletEventOutcome::Dispatch(cmd) => {
                assert_eq!(
                    cmd.realm_id,
                    "ck:realm:01904100-0000-7000-8000-000000000123"
                );
                assert!(matches!(cmd.body.as_str(), "hello"));
                assert_eq!(cmd.sender_did, "did:webvh:acme:alice");
                assert_eq!(cmd.body, "hello");
                assert_eq!(
                    cmd.flow_id.as_deref(),
                    Some("ck:flow:01904100-0000-7000-8000-000000000001")
                );
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn skips_events_outside_realm_namespace() {
        let ev = make_event(
            "did:webvh:acme:alice",
            "ck:realm:99999999-0000-7000-8000-000000000abc",
            "ck.message.create",
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
            "ck:realm:01904100-0000-7000-8000-000000000456",
            "ck.message.create",
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
            "ck:realm:01904100-0000-7000-8000-000000000456",
            "ck.message.create",
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
            "ck:realm:01904100-0000-7000-8000-000000000456",
            "ck.message.create",
            json!({
                "message_id": new_prefixed_uuid7("ck:message:"),
                "flow_id": "ck:flow:1",
                "track": "discussion",
                "content": { "kind": "ck.content.image", "ref": "ck:blob:..." }
            }),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::ContentKindUnsupported)
        );
    }

    #[test]
    fn skips_empty_body() {
        let ev = make_event(
            "did:webvh:acme:alice",
            "ck:realm:01904100-0000-7000-8000-000000000456",
            "ck.message.create",
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
            "ck:realm:01904100-0000-7000-8000-000000000456",
            "ck.flow.create",
            json!({"title": "irrelevant"}),
        );
        let outcome = classify_inbound_event(&cfg(), &ev);
        assert_eq!(
            outcome,
            AppletEventOutcome::Skip(AppletDispatchSkip::KindNotMessageCreate)
        );
    }
}
