//! Applet namespace helpers.
//!
//! Grammar (Cokret spec [`applet-schema.md`
//! §2](../../../../../../cokret/cokret-spec/spec/v1/zh/extensions/applet-schema.md)):
//!
//! * `*` matches one segment — one or more chars that are not a separator for the domain.
//! * `**` matches one or more `/`-separated segments but never crosses a `:` (SDK T-SDK-7 / S-13
//!   semantics).
//! * Literal `*` is escaped as `\*`.
//! * Separators are domain-specific: actors split on `:` only; realms and handles split on `:` and
//!   `/`. A DID `#fragment` is ignored for actors.
//!
//! The wire DTOs live in the Cokret SDK. This module keeps the local helper
//! names used by the channel code while aliasing the SDK types directly.

use cokret::{AppletNamespaceDomain, AppletNamespaceEntry, AppletWireNamespaces};

pub type NamespacePattern = AppletNamespaceEntry;
pub type AppletNamespaces = AppletWireNamespaces;

/// True iff `candidate` matches `pattern` under the applet namespace grammar
/// for `domain`.
///
/// Thin alias for [`cokret::namespace_pattern_matches`]. Since SDK T-SDK-7 the
/// matcher is domain-aware: the actor domain separates only on `:`, while the
/// realm / handle domains separate on `:` and `/`.
#[must_use]
pub fn namespace_pattern_matches(
    domain: AppletNamespaceDomain,
    pattern: &str,
    candidate: &str,
) -> bool {
    cokret::namespace_pattern_matches(domain, pattern, candidate)
}

pub trait NamespacePatternExt {
    #[must_use]
    fn matches(&self, domain: AppletNamespaceDomain, candidate: &str) -> bool;
}

impl NamespacePatternExt for NamespacePattern {
    fn matches(&self, domain: AppletNamespaceDomain, candidate: &str) -> bool {
        namespace_pattern_matches(domain, &self.pattern, candidate)
    }
}

pub trait AppletNamespacesExt {
    #[must_use]
    fn actor_matches(&self, did: &str) -> bool;

    #[must_use]
    fn realm_matches(&self, realm_id_or_alias: &str) -> bool;

    #[must_use]
    fn handle_matches(&self, handle: &str) -> bool;
}

impl AppletNamespacesExt for AppletNamespaces {
    fn actor_matches(&self, did: &str) -> bool {
        self.actors
            .iter()
            .any(|p| p.matches(AppletNamespaceDomain::Actors, did))
    }

    fn realm_matches(&self, realm_id_or_alias: &str) -> bool {
        self.realms
            .iter()
            .any(|p| p.matches(AppletNamespaceDomain::Realms, realm_id_or_alias))
    }

    fn handle_matches(&self, handle: &str) -> bool {
        self.handles
            .iter()
            .any(|p| p.matches(AppletNamespaceDomain::Handles, handle))
    }
}

#[cfg(test)]
mod tests {
    // Spec applet-schema.md §2 grammar table — all 9 rows from _cokret_todos.md.
    use AppletNamespaceDomain::{Actors, Realms};

    use super::*;

    #[test]
    fn single_star_matches_ghost_did() {
        assert!(namespace_pattern_matches(
            Actors,
            "did:web:slack-bridge.example:ghost:*",
            "did:web:slack-bridge.example:ghost:u123"
        ));
    }
    #[test]
    fn single_star_rejects_different_service() {
        assert!(!namespace_pattern_matches(
            Actors,
            "did:web:slack-bridge.example:ghost:*",
            "did:web:other.example:ghost:u123"
        ));
    }
    #[test]
    fn single_star_rejects_different_fragment_prefix() {
        assert!(!namespace_pattern_matches(
            Actors,
            "did:web:slack-bridge.example:ghost:*",
            "did:web:slack-bridge.example:bot"
        ));
    }
    #[test]
    fn single_star_matches_multi_segment_realm() {
        assert!(namespace_pattern_matches(
            Realms,
            "slack:team:*:channel:*",
            "slack:team:T123:channel:C456"
        ));
    }
    #[test]
    fn single_star_rejects_trailing_extra_segment() {
        assert!(!namespace_pattern_matches(
            Realms,
            "slack:team:*:channel:*",
            "slack:team:T123:channel:C456:thread:1"
        ));
    }
    #[test]
    fn double_star_crosses_slash_but_not_colon() {
        // SDK T-SDK-7 semantics: `**` spans `/`-separated segments but never
        // crosses a `:` separator.
        assert!(namespace_pattern_matches(
            Realms,
            "slack:team:**",
            "slack:team:T123/channel/C456"
        ));
        assert!(!namespace_pattern_matches(
            Realms,
            "slack:team:**",
            "slack:team:T123:channel:C456"
        ));
    }
    #[test]
    fn escaped_star_matches_literal_star() {
        assert!(namespace_pattern_matches(
            Actors,
            "literal\\*pattern",
            "literal*pattern"
        ));
    }
    #[test]
    fn escaped_star_rejects_non_star_char() {
        assert!(!namespace_pattern_matches(
            Actors,
            "literal\\*pattern",
            "literalXpattern"
        ));
    }
    #[test]
    fn empty_pattern_rejects_non_empty_candidate() {
        // SDK semantics (T-SDK-7): empty pattern rejects any non-empty
        // candidate. Empty/empty matches as a degenerate identity case —
        // we tolerate it because in practice we never invoke matchers with
        // an empty pattern at runtime (`load_cokret_applet_configs` filters
        // them out).
        assert!(!namespace_pattern_matches(Actors, "", "anything"));
    }

    // Additional sanity tests.
    #[test]
    fn pattern_must_consume_full_candidate() {
        assert!(!namespace_pattern_matches(Actors, "foo", "foobar"));
    }
    #[test]
    fn star_requires_at_least_one_char() {
        // Pattern requires "ghost:" then >=1 segment chars.
        assert!(!namespace_pattern_matches(
            Actors,
            "did:web:bridge.example:ghost:*",
            "did:web:bridge.example:ghost:"
        ));
    }
    #[test]
    fn applet_namespaces_actor_match() {
        let ns = AppletNamespaces {
            actors: vec![NamespacePattern::exclusive(
                "did:web:bridge.example:ghost:*",
            )],
            ..Default::default()
        };
        assert!(ns.actor_matches("did:web:bridge.example:ghost:u1"));
        assert!(!ns.actor_matches("did:web:bridge.example:bot"));
    }
    #[test]
    fn applet_namespaces_conflict_detection() {
        // `slack:team:*` (realm domain, `:`/`/` separators) overlaps the more
        // specific `slack:team:T1`; both exclusive → conflict.
        let a = AppletNamespaces {
            realms: vec![NamespacePattern::exclusive("slack:team:*")],
            ..Default::default()
        };
        let b = AppletNamespaces {
            realms: vec![NamespacePattern::exclusive("slack:team:T1")],
            ..Default::default()
        };
        let conflicts = a.conflicts_with(&b);
        assert!(!conflicts.is_empty());
        assert_eq!(conflicts[0].domain, Realms);
    }
}
