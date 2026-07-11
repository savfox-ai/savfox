//! Ghost actor DID minting.
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
}
