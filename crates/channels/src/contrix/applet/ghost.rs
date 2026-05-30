//! Ghost actor helpers — DID minting, profile event content, external_ref.
//!
//! A Ghost Actor is the Contrix mirror of an external network user (Slack
//! `U123`, Discord user id, GitHub login, ...). Spec
//! [`applet-integration.md` §9](../../../../../../contrix-dev/contrix-spec/spec/v1/zh/extensions/applet-integration.md)
//! requires:
//!
//! * Ghost DID MUST be distinguishable from native human DIDs at the
//!   protocol layer. We use the fragment form `{service_did}#{prefix}{slug}`
//!   so the parent DID Document namespace makes the controlling Applet
//!   visible.
//! * Profile MUST carry `actor_kind = "ghost"`, `managed_by_applet`,
//!   `external_ref`, and `accountability { mode, responsible_actor_id,
//!   operator_actor_ids[] }`.

use anyhow::Context as _;
use contrix::ProfileCreateBuilder;
use contrix_core::Event;
use contrix_identifiers::{Did, Hlc, RealmId};
use serde_json::{Value, json};

/// Mint a stable ghost DID for an external user.
///
/// Format: `{service_did}#{ghost_did_prefix}{slug(external_user_id)}`.
/// The slug step lowercases the external id, replaces non-alphanumeric
/// runs with `-`, and trims surrounding hyphens. If slugging yields an
/// empty string (extremely short / fully symbolic ids), the raw
/// `external_user_id` is used verbatim (URL-percent-encoded) so we never
/// emit a DID whose fragment is just the prefix.
#[must_use]
pub fn mint_ghost_did(
    applet_service_did: &str,
    ghost_did_prefix: &str,
    external_user_id: &str,
) -> String {
    let slug = slugify(external_user_id);
    let suffix = if slug.is_empty() {
        // Worst case: keep the raw id, but percent-encode anything outside
        // [A-Za-z0-9_-]. DID fragments allow these chars natively.
        percent_encode_minimal(external_user_id)
    } else {
        slug
    };
    format!("{applet_service_did}#{ghost_did_prefix}{suffix}")
}

/// Build a `cx.profile.create` Event Envelope for a Ghost Actor.
///
/// Uses the SDK's [`ProfileCreateBuilder`] (S-9 in SDK commit `bf29056`)
/// to stamp `actor_kind = "ghost"`, `managed_by_applet`, and the
/// `accountability` block per spec applet-integration.md §9. The returned
/// Event is unsigned (`proofs: []`) — caller attaches a `Proof` via
/// `contrix_signatures::sign_event` when an Ed25519 signer is plumbed in
/// (Phase 8).
///
/// `realm_id` is the Realm where the profile is published (typically the
/// portal Realm the ghost will write into). `actor_seq` MUST be monotonic
/// per-actor; caller maintains the counter.
pub fn build_ghost_profile_event(
    realm_id: &str,
    ghost_did: &str,
    display_name: &str,
    applet_id: &str,
    controller_did: &str,
    external_ref: Value,
    actor_seq: u64,
) -> anyhow::Result<Event> {
    let realm = RealmId::new(realm_id.to_owned())
        .with_context(|| format!("invalid realm_id: {realm_id}"))?;
    let ghost = Did::new(ghost_did.to_owned())
        .with_context(|| format!("invalid ghost actor DID: {ghost_did}"))?;
    let controller = Did::new(controller_did.to_owned())
        .with_context(|| format!("invalid controller DID: {controller_did}"))?;
    let hlc = current_hlc();
    let event = ProfileCreateBuilder::new(realm, ghost)
        .with_display_name(display_name)
        .with_ghost_kind(applet_id.to_owned(), controller)
        .with_external_ref(external_ref)
        .build(actor_seq, hlc)
        .map_err(|err| anyhow::anyhow!("ProfileCreateBuilder build failed: {err}"))?;
    Ok(event)
}

fn current_hlc() -> Hlc {
    let unix_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let value = format!("{unix_ms:012x}-0000-00000000");
    Hlc::new(value).expect("hlc shape validated")
}

/// Build the Ghost Actor profile JSON content (spec applet-integration.md §9).
///
/// **Deprecated**: prefer [`build_ghost_profile_event`], which returns a
/// fully-formed `Event` Envelope using the SDK's `ProfileCreateBuilder`
/// helper (S-9). This function returns only the `content` JSON for
/// callers that still need to assemble the Envelope themselves.
#[deprecated(note = "use build_ghost_profile_event (S-9, returns full Event)")]
#[must_use]
pub fn build_ghost_profile(
    ghost_did: &str,
    display_name: &str,
    applet_id: &str,
    service_did: &str,
    controller_did: &str,
    external_ref: Value,
) -> Value {
    json!({
        "actor_id": ghost_did,
        "actor_kind": "ghost",
        "display_name": display_name,
        "managed_by_applet": applet_id,
        "external_ref": external_ref,
        "accountability": {
            "mode": "applet_managed",
            "responsible_actor_id": controller_did,
            "operator_actor_ids": [service_did],
        },
    })
}

/// Build an `external_ref` object for a Ghost Actor or bridged Realm.
///
/// `protocol` is the registered bridge protocol (`"slack"`, `"discord"`, ...).
/// `network_id` is the workspace/tenant id (`"T123"` for Slack, guild id
/// for Discord, etc.). `external_id` is the user / channel / location id.
#[must_use]
pub fn build_external_ref(protocol: &str, network_id: &str, external_id: &str) -> Value {
    json!({
        "protocol": protocol,
        "network_id": network_id,
        "external_id": external_id,
    })
}

fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_sep = true; // collapse leading separators
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('-');
            last_was_sep = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn percent_encode_minimal(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            for b in c.to_string().bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_ghost_did_basic() {
        let did = mint_ghost_did("did:web:slack-bridge.example", "ghost-", "U123");
        assert_eq!(did, "did:web:slack-bridge.example#ghost-u123");
    }

    #[test]
    fn mint_ghost_did_with_spaces_and_punctuation() {
        let did = mint_ghost_did("did:web:bridge.example", "g_", "Alice Smith!");
        assert_eq!(did, "did:web:bridge.example#g_alice-smith");
    }

    #[test]
    fn mint_ghost_did_stability_for_same_input() {
        let a = mint_ghost_did("did:web:b.example", "ghost-", "U999");
        let b = mint_ghost_did("did:web:b.example", "ghost-", "U999");
        assert_eq!(a, b);
    }

    #[test]
    fn mint_ghost_did_percent_encodes_when_slug_empty() {
        let did = mint_ghost_did("did:web:b.example", "ghost-", "@@@");
        assert!(did.starts_with("did:web:b.example#ghost-"));
        // The `@` chars should be percent-encoded.
        assert!(did.contains("%40"));
    }

    #[test]
    fn ghost_profile_event_uses_sdk_builder() {
        let event = build_ghost_profile_event(
            "cx:realm:01904100-0000-7000-8000-000000000001",
            "did:web:bridge.example#ghost-u1",
            "Alice on Slack",
            "cx:applet:21532600-0000-7000-8000-000000000000",
            "did:webvh:acme:admin",
            build_external_ref("slack", "T123", "U1"),
            5,
        )
        .expect("build");
        assert_eq!(event.kind, "cx.profile.create");
        assert_eq!(event.actor_id.as_str(), "did:web:bridge.example#ghost-u1");
        assert_eq!(event.content["actor_kind"], "ghost");
        assert_eq!(
            event.content["managed_by_applet"],
            "cx:applet:21532600-0000-7000-8000-000000000000"
        );
        assert_eq!(
            event.content["accountability"]["accountable_to"],
            "did:webvh:acme:admin"
        );
        // S-7: external_ref lives at top level, not in content.
        let ext = event.external_ref.as_ref().expect("external_ref");
        assert_eq!(ext["protocol"], "slack");
    }

    #[test]
    fn ghost_profile_event_rejects_invalid_did() {
        let result = build_ghost_profile_event(
            "cx:realm:01904100-0000-7000-8000-000000000001",
            "not-a-did",
            "Alice",
            "cx:applet:1",
            "did:webvh:acme:admin",
            build_external_ref("slack", "T123", "U1"),
            1,
        );
        assert!(result.is_err());
    }

    #[test]
    #[allow(deprecated)]
    fn ghost_profile_marks_actor_kind_and_accountability() {
        let profile = build_ghost_profile(
            "did:web:bridge.example#ghost-u1",
            "Alice on Slack",
            "cx:applet:1",
            "did:web:bridge.example",
            "did:webvh:acme:admin",
            build_external_ref("slack", "T123", "U1"),
        );
        assert_eq!(profile["actor_kind"], "ghost");
        assert_eq!(profile["managed_by_applet"], "cx:applet:1");
        assert_eq!(profile["accountability"]["mode"], "applet_managed");
        assert_eq!(
            profile["accountability"]["responsible_actor_id"],
            "did:webvh:acme:admin"
        );
        let operators = profile["accountability"]["operator_actor_ids"]
            .as_array()
            .expect("array");
        assert_eq!(operators.len(), 1);
        assert_eq!(operators[0], "did:web:bridge.example");
    }

    #[test]
    fn external_ref_has_three_canonical_fields() {
        let ext = build_external_ref("slack", "T123", "U1");
        assert_eq!(ext["protocol"], "slack");
        assert_eq!(ext["network_id"], "T123");
        assert_eq!(ext["external_id"], "U1");
    }
}
