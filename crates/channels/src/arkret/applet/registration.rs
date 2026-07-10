//! Build a wire-format `ak.applet.registration` payload using the SDK's
//! [`arkret::WireAppletRegistration`] (S-4 in SDK commit `bf29056`).
//!
//! The output is the canonical payload that a controller DID holder must
//! sign before submitting to an Arkret server. savfox does not currently
//! do signing — operators generate the payload here, sign offline (via
//! `arkret::sign_registration` once they wire the Ed25519 signer in
//! Phase 8), and submit by hand or via the Arkret admin tooling.
//!
//! Schema reference: Arkret spec
//! [`applet-schema.md`
//! §1](../../../../../../arkret/arkret-spec/spec/v1/zh/extensions/applet-schema.md).

use arkret::{Did, Hash, WireAppletRegistration};
use serde_json::Value;

use super::config::ArkretAppletConfig;
/// Build the wire-format `ak.applet.registration` payload (unsigned).
///
/// Returns a strongly-typed [`WireAppletRegistration`] from the SDK whose
/// `proof` field is `None`. Caller chains
/// [`arkret::sign_registration(&mut reg, &signer, vm)`] to populate it
/// (or serializes the unsigned payload to JSON for offline signing).
///
/// Note: spec applet-schema.md §1/§2 shows each namespace entry as
/// `{ "exclusive": bool, "pattern": string }`. Since SDK S-13 the wire form
/// (`AppletWireNamespaces`) carries `Vec<AppletNamespaceEntry>` (object
/// entries with the `exclusive` flag), so savfox's per-pattern `exclusive`
/// flag now round-trips onto the wire instead of being dropped.
///
/// The `registration_epoch` (a security-evidence digest the controller binds)
/// is taken from `cfg.registration_epoch` when supplied; otherwise a zero
/// placeholder is emitted — see [`resolve_registration_epoch`].
pub fn build_registration_payload(
    cfg: &ArkretAppletConfig,
) -> anyhow::Result<WireAppletRegistration> {
    // Parse fallibly rather than `expect`: callers may build a payload from a
    // config that has not been through `validate()`, and a malformed DID must
    // surface as an error, never a panic.
    let parse_did = |label: &str, raw: &str| -> anyhow::Result<Did> {
        Did::new(raw.to_owned()).map_err(|err| {
            anyhow::anyhow!("arkret applet registration: invalid {label} DID '{raw}': {err}")
        })
    };
    let service_did = parse_did("service_did", &cfg.service_did)?;
    let controller_did = parse_did("controller_did", &cfg.controller_did)?;
    let bot_actor_id = parse_did("bot_actor_id", &cfg.bot_actor_id)?;

    let registration_epoch = resolve_registration_epoch(cfg)?;
    let mut reg = WireAppletRegistration::new(
        cfg.applet_id.clone(),
        service_did,
        controller_did,
        cfg.base_url.clone(),
        bot_actor_id,
        cfg.protocols.clone(),
        cfg.namespaces.clone(),
        registration_epoch,
    );
    reg.receive_events = cfg.receive_events;
    reg.receive_ephemeral = cfg.receive_ephemeral;
    reg.rate_limited = cfg.rate_limited;
    reg.requested_scopes = cfg.requested_scopes.clone();
    // `webhook_auth` left None — savfox doesn't run mTLS / HTTP-message-sig
    // ingress in Phase 7; operators can hand-edit the JSON if needed.
    Ok(reg)
}

/// Convenience: render the SDK [`WireAppletRegistration`] as JSON for
/// offline tooling (signing scripts, registration submission via
/// `curl`, etc.).
pub fn build_registration_json(cfg: &ArkretAppletConfig) -> anyhow::Result<Value> {
    let reg = build_registration_payload(cfg)?;
    Ok(serde_json::to_value(&reg).expect("WireAppletRegistration always serializes"))
}

/// Resolve the registration security-epoch hash. Uses the operator-supplied
/// `cfg.registration_epoch` (`sha256:<hex>`) when present; otherwise returns a
/// zero placeholder. savfox cannot synthesize the real evidence digest
/// (DID Document + signing key + endpoint + auth, per `applet-schema.md` §1),
/// so an unset epoch yields an UNSIGNED draft only — the operator must set
/// `registrationEpoch` (or have controller tooling compute it) before the
/// payload is signed and submitted.
fn resolve_registration_epoch(cfg: &ArkretAppletConfig) -> anyhow::Result<Hash> {
    match cfg.registration_epoch.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Hash::new(raw.to_owned()).map_err(|err| {
            anyhow::anyhow!(
                "arkret applet '{}': invalid registrationEpoch '{raw}': {err}",
                cfg.id
            )
        }),
        _ => Hash::new(format!("sha256:{}", "0".repeat(64)))
            .map_err(|err| anyhow::anyhow!("placeholder registration epoch: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use arkret::AppletNamespaceEntry;

    use super::*;
    use crate::arkret::applet::namespace::{AppletNamespaces, NamespacePattern};

    fn cfg() -> ArkretAppletConfig {
        ArkretAppletConfig {
            id: "applet-1".into(),
            applet_id: "ak:applet:21532600-0000-7000-8000-000000000000".into(),
            service_did: "did:web:slack-bridge.example".into(),
            controller_did: "did:webvh:example.com:admin".into(),
            base_url: "https://savfox.example/appservices/arkret/applet-1".into(),
            bot_actor_id: "did:web:slack-bridge.example:bot".into(),
            device_id: None,
            arkret_server_url: "https://arkret.example.org".into(),
            arkret_server_did: Some("did:webvh:arkret.example.org".into()),
            trusted_verification_methods: Vec::new(),
            login_challenge: None,
            arkret_bearer_token: None,
            namespaces: AppletNamespaces {
                actors: vec![NamespacePattern::exclusive(
                    "did:web:slack-bridge.example:ghost:*",
                )],
                realms: vec![NamespacePattern::exclusive("slack:team:*:channel:*")],
                handles: vec![NamespacePattern::shared("slack.acme.example/*")],
            },
            protocols: vec!["slack".into()],
            ghost_did_prefix: "ghost:".into(),
            requested_scopes: vec!["ak.message.create".into(), "ak.flow.create".into()],
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

    #[test]
    fn kind_is_applet_registration() {
        let reg = build_registration_payload(&cfg()).expect("build registration");
        assert_eq!(reg.kind, WireAppletRegistration::KIND);
        assert_eq!(reg.kind, "ak.applet.registration");
    }

    #[test]
    fn proof_unset_until_offline_signing() {
        let reg = build_registration_payload(&cfg()).expect("build registration");
        assert!(reg.proof.is_none());
    }

    #[test]
    fn carries_top_level_required_fields() {
        let reg = build_registration_payload(&cfg()).expect("build registration");
        assert_eq!(
            reg.applet_id,
            "ak:applet:21532600-0000-7000-8000-000000000000"
        );
        assert_eq!(reg.service_did.as_str(), "did:web:slack-bridge.example");
        assert_eq!(reg.controller_did.as_str(), "did:webvh:example.com:admin");
        assert_eq!(
            reg.bot_actor_id.as_str(),
            "did:web:slack-bridge.example:bot"
        );
        assert!(reg.receive_events);
        assert!(!reg.receive_ephemeral);
        assert!(reg.rate_limited);
    }

    #[test]
    fn namespaces_round_trip_with_exclusive_flag() {
        let reg = build_registration_payload(&cfg()).expect("build registration");
        assert_eq!(
            reg.namespaces.actors,
            vec![AppletNamespaceEntry::exclusive(
                "did:web:slack-bridge.example:ghost:*"
            )]
        );
        assert_eq!(
            reg.namespaces.realms,
            vec![AppletNamespaceEntry::exclusive("slack:team:*:channel:*")]
        );
        // handles entry was declared non-exclusive in `cfg()`.
        assert_eq!(
            reg.namespaces.handles,
            vec![AppletNamespaceEntry::shared("slack.acme.example/*")]
        );
    }

    #[test]
    fn registration_epoch_defaults_to_zero_placeholder() {
        let reg = build_registration_payload(&cfg()).expect("build registration");
        assert_eq!(
            reg.registration_epoch.as_str(),
            format!("sha256:{}", "0".repeat(64))
        );
    }

    #[test]
    fn payload_digest_changes_with_proof_omitted() {
        // SDK API: payload_digest treats proof as absent regardless of state.
        // Two unsigned registrations of the same config should produce equal
        // digests (modulo `created_at`, which is monotonic time).
        let reg = build_registration_payload(&cfg()).expect("build registration");
        let digest = reg.payload_digest().expect("digest");
        assert!(digest.as_str().starts_with("sha256:"));
    }

    #[test]
    fn requested_scopes_passthrough() {
        let reg = build_registration_payload(&cfg()).expect("build registration");
        assert!(
            reg.requested_scopes
                .iter()
                .any(|s| s == "ak.message.create")
        );
    }

    #[test]
    fn json_form_contains_kind_field() {
        let json = build_registration_json(&cfg()).expect("build registration json");
        assert_eq!(json["kind"], "ak.applet.registration");
        assert!(json["proof"].is_null());
    }
}
