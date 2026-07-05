//! Applet namespace pattern matcher.
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
//! Delegates to the upstream SDK matcher (`cokret::namespace_pattern_matches`,
//! added in SDK T-SDK-7). The local pub fn name is preserved so other code
//! in this crate doesn't have to reach across the workspace boundary.

use cokret::AppletNamespaceDomain;
use serde::{Deserialize, Serialize};

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

/// Pattern + exclusivity flag pair (mirrors spec `namespaces.*[]` entries).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NamespacePattern {
    pub pattern: String,
    #[serde(default)]
    pub exclusive: bool,
}

impl NamespacePattern {
    #[must_use]
    pub fn new(pattern: impl Into<String>, exclusive: bool) -> Self {
        Self {
            pattern: pattern.into(),
            exclusive,
        }
    }

    #[must_use]
    pub fn matches(&self, domain: AppletNamespaceDomain, candidate: &str) -> bool {
        namespace_pattern_matches(domain, &self.pattern, candidate)
    }
}

/// Three-axis namespace declaration block for an applet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppletNamespaces {
    #[serde(default)]
    pub actors: Vec<NamespacePattern>,
    #[serde(default)]
    pub realms: Vec<NamespacePattern>,
    #[serde(default)]
    pub handles: Vec<NamespacePattern>,
}

impl AppletNamespaces {
    #[must_use]
    pub fn actor_matches(&self, did: &str) -> bool {
        self.actors
            .iter()
            .any(|p| p.matches(AppletNamespaceDomain::Actors, did))
    }

    #[must_use]
    pub fn realm_matches(&self, realm_id_or_alias: &str) -> bool {
        self.realms
            .iter()
            .any(|p| p.matches(AppletNamespaceDomain::Realms, realm_id_or_alias))
    }

    #[must_use]
    pub fn handle_matches(&self, handle: &str) -> bool {
        self.handles
            .iter()
            .any(|p| p.matches(AppletNamespaceDomain::Handles, handle))
    }

    /// Detect an exclusive-namespace conflict with another applet's declaration.
    ///
    /// Two declarations on the same axis conflict iff at least one is
    /// exclusive and their patterns overlap (one matches the other's
    /// pattern as if it were a candidate, or vice versa). This is a
    /// best-effort approximation — registry-side enforcement is the
    /// source of truth; this helper is for local pre-flight checks.
    #[must_use]
    pub fn conflicts_with(&self, other: &Self) -> Vec<(&'static str, String, String)> {
        let mut conflicts = Vec::new();
        for (axis, domain, mine, theirs) in [
            (
                "actors",
                AppletNamespaceDomain::Actors,
                &self.actors,
                &other.actors,
            ),
            (
                "realms",
                AppletNamespaceDomain::Realms,
                &self.realms,
                &other.realms,
            ),
            (
                "handles",
                AppletNamespaceDomain::Handles,
                &self.handles,
                &other.handles,
            ),
        ] {
            for a in mine {
                for b in theirs {
                    if (a.exclusive || b.exclusive)
                        && patterns_overlap(domain, &a.pattern, &b.pattern)
                    {
                        conflicts.push((axis, a.pattern.clone(), b.pattern.clone()));
                    }
                }
            }
        }
        conflicts
    }
}

fn patterns_overlap(domain: AppletNamespaceDomain, a: &str, b: &str) -> bool {
    // Approximation: two patterns overlap if a literal "probe" from one
    // matches the other. We probe with the no-wildcard prefix of each.
    let probe_a = literal_prefix(a);
    let probe_b = literal_prefix(b);
    if !probe_a.is_empty() && namespace_pattern_matches(domain, b, &probe_a) {
        return true;
    }
    if !probe_b.is_empty() && namespace_pattern_matches(domain, a, &probe_b) {
        return true;
    }
    a == b
}

fn literal_prefix(pattern: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => break,
            '\\' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                out.push('*');
                i += 2;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    out
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
            actors: vec![NamespacePattern::new(
                "did:web:bridge.example:ghost:*",
                true,
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
            realms: vec![NamespacePattern::new("slack:team:*", true)],
            ..Default::default()
        };
        let b = AppletNamespaces {
            realms: vec![NamespacePattern::new("slack:team:T1", true)],
            ..Default::default()
        };
        assert!(!a.conflicts_with(&b).is_empty());
    }
}
