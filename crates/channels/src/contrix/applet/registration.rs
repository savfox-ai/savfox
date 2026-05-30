//! Build a wire-format `cx.applet.registration` payload using the SDK's
//! [`contrix::WireAppletRegistration`] (S-4 in SDK commit `bf29056`).
//!
//! The output is the canonical payload that a controller DID holder must
//! sign before submitting to a Contrix server. savfox does not currently
//! do signing — operators generate the payload here, sign offline (via
//! `contrix::sign_registration` once they wire the Ed25519 signer in
//! Phase 8), and submit by hand or via the Contrix admin tooling.
//!
//! Schema reference: Contrix spec
//! [`applet-schema.md` §1](../../../../../../contrix-dev/contrix-spec/spec/v1/zh/extensions/applet-schema.md).

use contrix::{AppletWireNamespaces, WireAppletRegistration};
use contrix_identifiers::Did;
use serde_json::Value;

use super::config::ContrixAppletConfig;
use super::namespace::AppletNamespaces;

/// Build the wire-format `cx.applet.registration` payload (unsigned).
///
/// Returns a strongly-typed [`WireAppletRegistration`] from the SDK whose
/// `proof` field is `None`. Caller chains
/// [`contrix::sign_registration(&mut reg, &signer, vm)`] to populate it
/// (or serializes the unsigned payload to JSON for offline signing).
///
/// Note: spec applet-schema.md §1 shows each namespace entry as
/// `{ "exclusive": bool, "pattern": string }`, but the SDK wire form
/// (`AppletWireNamespaces`) carries `Vec<String>` (just patterns). The
/// `exclusive` flag stays a savfox-local concern used by the namespace
/// matcher; on the wire we only ship the pattern strings.
#[must_use]
pub fn build_registration_payload(cfg: &ContrixAppletConfig) -> WireAppletRegistration {
    let service_did = Did::new(cfg.service_did.clone()).expect("validated DID");
    let controller_did = Did::new(cfg.controller_did.clone()).expect("validated DID");
    let bot_actor_id = Did::new(cfg.bot_actor_id.clone()).expect("validated DID");

    let mut reg = WireAppletRegistration::new(
        cfg.applet_id.clone(),
        service_did,
        controller_did,
        cfg.base_url.clone(),
        bot_actor_id,
        cfg.protocols.clone(),
        namespaces_to_wire(&cfg.namespaces),
    );
    reg.receive_events = cfg.receive_events;
    reg.receive_ephemeral = cfg.receive_ephemeral;
    reg.rate_limited = cfg.rate_limited;
    reg.requested_scopes = cfg.requested_scopes.clone();
    // `webhook_auth` left None — savfox doesn't run mTLS / HTTP-message-sig
    // ingress in Phase 7; operators can hand-edit the JSON if needed.
    reg
}

/// Convenience: render the SDK [`WireAppletRegistration`] as JSON for
/// offline tooling (signing scripts, registration submission via
/// `curl`, etc.).
#[must_use]
pub fn build_registration_json(cfg: &ContrixAppletConfig) -> Value {
    let reg = build_registration_payload(cfg);
    serde_json::to_value(&reg).expect("WireAppletRegistration always serializes")
}

fn namespaces_to_wire(ns: &AppletNamespaces) -> AppletWireNamespaces {
    AppletWireNamespaces {
        actors: ns.actors.iter().map(|p| p.pattern.clone()).collect(),
        realms: ns.realms.iter().map(|p| p.pattern.clone()).collect(),
        handles: ns.handles.iter().map(|p| p.pattern.clone()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contrix::applet::namespace::NamespacePattern;

    fn cfg() -> ContrixAppletConfig {
        ContrixAppletConfig {
            id: "applet-1".into(),
            applet_id: "cx:applet:21532600-0000-7000-8000-000000000000".into(),
            service_did: "did:web:slack-bridge.example".into(),
            controller_did: "did:webvh:example.com:admin".into(),
            base_url: "https://savfox.example/appservices/contrix/applet-1/api/v1/applet".into(),
            bot_actor_id: "did:web:slack-bridge.example#bot".into(),
            contrix_server_url: "https://contrix.example.org".into(),
            contrix_bearer_token: None,
            namespaces: AppletNamespaces {
                actors: vec![NamespacePattern::new(
                    "did:web:slack-bridge.example#ghost-*",
                    true,
                )],
                realms: vec![NamespacePattern::new("slack:team:*:channel:*", true)],
                handles: vec![NamespacePattern::new("slack.acme.example/*", false)],
            },
            protocols: vec!["slack".into()],
            ghost_did_prefix: "ghost-".into(),
            requested_scopes: vec!["cx.message.create".into(), "cx.flow.create".into()],
            receive_events: true,
            receive_ephemeral: false,
            rate_limited: true,
            authorization_grant_id: None,
            key_ref: None,
            verification_method: None,
            grant_event_path: None,
        }
    }

    #[test]
    fn kind_is_applet_registration() {
        let reg = build_registration_payload(&cfg());
        assert_eq!(reg.kind, WireAppletRegistration::KIND);
        assert_eq!(reg.kind, "cx.applet.registration");
    }

    #[test]
    fn proof_unset_until_offline_signing() {
        let reg = build_registration_payload(&cfg());
        assert!(reg.proof.is_none());
    }

    #[test]
    fn carries_top_level_required_fields() {
        let reg = build_registration_payload(&cfg());
        assert_eq!(
            reg.applet_id,
            "cx:applet:21532600-0000-7000-8000-000000000000"
        );
        assert_eq!(reg.service_did.as_str(), "did:web:slack-bridge.example");
        assert_eq!(reg.controller_did.as_str(), "did:webvh:example.com:admin");
        assert_eq!(
            reg.bot_actor_id.as_str(),
            "did:web:slack-bridge.example#bot"
        );
        assert!(reg.receive_events);
        assert!(!reg.receive_ephemeral);
        assert!(reg.rate_limited);
    }

    #[test]
    fn namespaces_flatten_to_pattern_strings() {
        let reg = build_registration_payload(&cfg());
        assert_eq!(
            reg.namespaces.actors,
            vec!["did:web:slack-bridge.example#ghost-*"]
        );
        assert_eq!(reg.namespaces.realms, vec!["slack:team:*:channel:*"]);
        assert_eq!(reg.namespaces.handles, vec!["slack.acme.example/*"]);
    }

    #[test]
    fn payload_digest_changes_with_proof_omitted() {
        // SDK API: payload_digest treats proof as absent regardless of state.
        // Two unsigned registrations of the same config should produce equal
        // digests (modulo `created_at`, which is monotonic time).
        let reg = build_registration_payload(&cfg());
        let digest = reg.payload_digest().expect("digest");
        assert!(digest.as_str().starts_with("sha256:"));
    }

    #[test]
    fn requested_scopes_passthrough() {
        let reg = build_registration_payload(&cfg());
        assert!(
            reg.requested_scopes
                .iter()
                .any(|s| s == "cx.message.create")
        );
    }

    #[test]
    fn json_form_contains_kind_field() {
        let json = build_registration_json(&cfg());
        assert_eq!(json["kind"], "cx.applet.registration");
        assert!(json["proof"].is_null());
    }
}
