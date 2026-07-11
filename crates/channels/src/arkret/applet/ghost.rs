//! Ghost actor helpers — DID minting, profile event content, external_ref.
//!
//! A Ghost Actor is the Arkret mirror of an external network user (Slack
//! `U123`, Discord user id, GitHub login, ...). Spec
//! [`applet-integration.md`
//! §9](../../../../../../arkret/arkret-spec/spec/v1/zh/extensions/applet-integration.md)
//! requires:
//!
//! * Ghost DID MUST be distinguishable from native human DIDs at the protocol layer. We use the
//!   colon path-segment form `{service_id}:{prefix}{slug}` (e.g.
//!   `did:web:slack-bridge.example:ghost:u123`) so the controlling Applet's DID namespace is
//!   visible. Per spec applet-integration.md §3.4 a `#fragment` MUST NOT appear in an `actor_id` —
//!   fragments are reserved for DID-URL verification methods (`…:ghost:u123#key-1`).
//! * Profile MUST carry `actor_kind = "integration"`, `profile_fields.managed_by_applet`,
//!   `profile_fields.external_ref`, and `accountable_principal_ids`.

use anyhow::Context as _;
use arkret::{AppletId, Did, Event, Hlc, ProfileCreateBuilder, RealmId};
use serde_json::{Value, json};

/// Mint a stable ghost DID for an external user.
///
/// Format: `{service_id}:{ghost_did_prefix}{slug(external_user_id)}` —
/// colon path-segment form (e.g. `did:web:slack-bridge.example:ghost:u123`),
/// NOT a `#fragment` (which the spec reserves for verification methods and
/// the identifiers crate rejects in an `actor_id`). The slug step lowercases
/// the external id, replaces non-alphanumeric runs with `-`, and trims
/// surrounding hyphens. If slugging yields an empty string (extremely short /
/// fully symbolic ids), the raw `external_user_id` is used verbatim
/// (URL-percent-encoded) so we never emit a DID whose last segment is just
/// the prefix.
#[must_use]
pub fn mint_ghost_did(
    applet_service_id: &str,
    ghost_did_prefix: &str,
    external_user_id: &str,
) -> String {
    let slug = slugify(external_user_id);
    let suffix = if slug.is_empty() {
        // Worst case: keep the raw id, but percent-encode anything outside
        // [A-Za-z0-9_-] so the DID segment stays well-formed.
        percent_encode_minimal(external_user_id)
    } else {
        slug
    };
    format!("{applet_service_id}:{ghost_did_prefix}{suffix}")
}

/// Build a `ak.profile.create` Event Envelope for a Ghost Actor.
///
/// Uses the SDK's [`ProfileCreateBuilder`] (S-9 in SDK commit `bf29056`)
/// to stamp `actor_kind = "integration"`, `profile_fields.managed_by_applet`,
/// `profile_fields.external_ref`, and `accountable_principal_ids` per spec
/// applet-integration.md §9. The returned
/// Event is unsigned (`proofs: []`) — caller attaches a `Proof` via
/// `arkret_signatures::sign_event` when an Ed25519 signer is plumbed in
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
    controller_id: &str,
    external_ref: Value,
    actor_seq: u64,
) -> anyhow::Result<Event> {
    let realm = RealmId::new(realm_id.to_owned())
        .with_context(|| format!("invalid realm_id: {realm_id}"))?;
    let ghost = Did::new(ghost_did.to_owned())
        .with_context(|| format!("invalid ghost actor DID: {ghost_did}"))?;
    let controller = Did::new(controller_id.to_owned())
        .with_context(|| format!("invalid controller DID: {controller_id}"))?;
    let applet = AppletId::new(applet_id.to_owned())
        .with_context(|| format!("invalid applet_id: {applet_id}"))?;
    let hlc = current_hlc();
    ProfileCreateBuilder::new(realm, ghost)
        .with_display_name(display_name)
        .with_ghost_actor_profile(applet, vec![controller])
        .with_external_ref(external_ref)
        .build(actor_seq, hlc)
        .map_err(|err| anyhow::anyhow!("ProfileCreateBuilder build failed: {err}"))
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
    service_id: &str,
    controller_id: &str,
    external_ref: Value,
) -> Value {
    json!({
        "actor_id": ghost_did,
        "actor_kind": "integration",
        "display_name": display_name,
        "accountable_principal_ids": [controller_id, service_id],
        "profile_fields": {
            "managed_by_applet": applet_id,
            "external_ref": external_ref,
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
        let did = mint_ghost_did("did:web:slack-bridge.example", "ghost:", "U123");
        assert_eq!(did, "did:web:slack-bridge.example:ghost:u123");
    }

    #[test]
    fn mint_ghost_did_with_spaces_and_punctuation() {
        let did = mint_ghost_did("did:web:bridge.example", "g_", "Alice Smith!");
        assert_eq!(did, "did:web:bridge.example:g_alice-smith");
    }

    #[test]
    fn mint_ghost_did_stability_for_same_input() {
        let a = mint_ghost_did("did:web:b.example", "ghost:", "U999");
        let b = mint_ghost_did("did:web:b.example", "ghost:", "U999");
        assert_eq!(a, b);
    }

    #[test]
    fn mint_ghost_did_percent_encodes_when_slug_empty() {
        let did = mint_ghost_did("did:web:b.example", "ghost:", "@@@");
        assert!(did.starts_with("did:web:b.example:ghost:"));
        // The `@` chars should be percent-encoded.
        assert!(did.contains("%40"));
    }

    #[test]
    fn ghost_profile_event_uses_sdk_builder() {
        let event = build_ghost_profile_event(
            "ak:realm:01904100-0000-7000-8000-000000000001",
            "did:web:bridge.example:ghost:u1",
            "Alice on Slack",
            "ak:applet:21532600-0000-7000-8000-000000000000",
            "did:webvh:acme:admin",
            build_external_ref("slack", "T123", "U1"),
            5,
        )
        .expect("build");
        assert_eq!(event.kind, "ak.profile.create");
        assert_eq!(event.actor_id.as_str(), "did:web:bridge.example:ghost:u1");
        let object = &event.payload["object"];
        assert_eq!(object["actor_kind"], "integration");
        assert_eq!(
            object["profile_fields"]["managed_by_applet"],
            "ak:applet:21532600-0000-7000-8000-000000000000"
        );
        assert_eq!(
            object["accountable_principal_ids"][0],
            "did:webvh:acme:admin"
        );
        let ext = &object["profile_fields"]["external_ref"];
        assert_eq!(ext["protocol"], "slack");
        assert!(!event.unsigned.contains_key("external_ref"));
    }

    #[test]
    fn ghost_profile_event_rejects_invalid_did() {
        let result = build_ghost_profile_event(
            "ak:realm:01904100-0000-7000-8000-000000000001",
            "not-a-did",
            "Alice",
            "ak:applet:1",
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
            "did:web:bridge.example:ghost:u1",
            "Alice on Slack",
            "ak:applet:1",
            "did:web:bridge.example",
            "did:webvh:acme:admin",
            build_external_ref("slack", "T123", "U1"),
        );
        assert_eq!(profile["actor_kind"], "integration");
        assert_eq!(
            profile["profile_fields"]["managed_by_applet"],
            "ak:applet:1"
        );
        assert_eq!(
            profile["accountable_principal_ids"][0],
            "did:webvh:acme:admin"
        );
        let operators = profile["accountable_principal_ids"]
            .as_array()
            .expect("array");
        assert_eq!(operators.len(), 2);
        assert_eq!(operators[1], "did:web:bridge.example");
    }

    #[test]
    fn external_ref_has_three_canonical_fields() {
        let ext = build_external_ref("slack", "T123", "U1");
        assert_eq!(ext["protocol"], "slack");
        assert_eq!(ext["network_id"], "T123");
        assert_eq!(ext["external_id"], "U1");
    }
}
