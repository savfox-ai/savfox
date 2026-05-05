//! Strict filename / path-segment validation helpers.
//!
//! Every site that builds a filesystem path from an RPC parameter (e.g.
//! `agents_dir.join(format!("{agent_id}.json"))`) MUST run the segment
//! through [`safe_filename_segment`] or [`safe_join`] first. Without this,
//! an authenticated attacker can escape the intended directory by passing
//! `../../etc/passwd` and read or overwrite arbitrary files inside the
//! parent's filesystem subtree.
//!
//! The helpers here are intentionally **strict** (refuse-on-anomaly)
//! rather than lenient (silently rewrite). A request that supplies an
//! invalid identifier is a bug — fail loudly so it surfaces in tests
//! and in operator logs.

use std::path::{Path, PathBuf};

/// Maximum length for a single path segment after trimming. Long enough for
/// realistic identifiers, short enough to keep filesystem operations sane.
const MAX_SEGMENT_LEN: usize = 200;

/// Reject the segment if it contains anything that could escape the parent
/// directory or alter filesystem semantics.
///
/// Returns `Some(trimmed)` for safe segments and `None` for any of:
///
/// * Empty / whitespace-only.
/// * `.` or `..` (current / parent directory references).
/// * Contains an OS path separator (`/` on Unix, `\` and `:` on Windows).
/// * Contains a NUL byte or any other control character.
/// * Contains characters Windows refuses in filenames (`<`, `>`, `*`, `?`,
///   `"`, `|`).
/// * Begins or ends with `.` or whitespace (Windows trailing-dot quirk).
/// * Longer than [`MAX_SEGMENT_LEN`] characters.
///
/// This is intentionally stricter than the Unix filesystem alone would
/// allow — the gateway is meant to run identically on Linux, macOS, and
/// Windows, so we apply the union of restrictions.
#[must_use]
pub fn safe_filename_segment(segment: &str) -> Option<&str> {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "." || trimmed == ".." {
        return None;
    }
    if trimmed.len() > MAX_SEGMENT_LEN {
        return None;
    }
    if trimmed.starts_with('.') || trimmed.ends_with('.') {
        // Leading dot would create a hidden file; trailing dot is silently
        // stripped by Windows, which can cause two distinct identifiers to
        // resolve to the same file.
        return None;
    }
    for ch in trimmed.chars() {
        let bad = matches!(
            ch,
            '/' | '\\' | ':' | '\0' | '<' | '>' | '*' | '?' | '"' | '|'
        ) || ch.is_control();
        if bad {
            return None;
        }
    }
    Some(trimmed)
}

/// Build a `<base>/<segment><suffix>` path after validating that `segment`
/// cannot escape `base`.
///
/// `suffix` is appended verbatim (e.g. `".json"`, `".md"`); it is not
/// validated because callers always pass a constant. Returns `None` when
/// [`safe_filename_segment`] rejects the segment.
///
/// # Example
///
/// ```ignore
/// let path = safe_join(base, agent_id, ".json")?;
/// ```
#[must_use]
pub fn safe_join(base: &Path, segment: &str, suffix: &str) -> Option<PathBuf> {
    let safe = safe_filename_segment(segment)?;
    let mut filename = String::with_capacity(safe.len() + suffix.len());
    filename.push_str(safe);
    filename.push_str(suffix);
    Some(base.join(filename))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_identifiers() {
        for id in ["alice", "agent-01", "my_agent", "review.notes"] {
            // Note: review.notes contains internal dot — allowed.
            // (Forbidden: leading or trailing dot.)
            assert!(
                safe_filename_segment(id).is_some(),
                "{id} should be accepted"
            );
        }
    }

    #[test]
    fn rejects_path_separators() {
        for bad in ["a/b", "a\\b", "../../etc/passwd", "x/y.json"] {
            assert!(
                safe_filename_segment(bad).is_none(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_dot_dot_and_dot() {
        assert!(safe_filename_segment(".").is_none());
        assert!(safe_filename_segment("..").is_none());
        assert!(safe_filename_segment(".hidden").is_none());
        assert!(safe_filename_segment("trailing.").is_none());
    }

    #[test]
    fn rejects_empty_or_whitespace() {
        assert!(safe_filename_segment("").is_none());
        assert!(safe_filename_segment("   ").is_none());
    }

    #[test]
    fn rejects_control_and_nul() {
        assert!(safe_filename_segment("ab\0cd").is_none());
        assert!(safe_filename_segment("line\nbreak").is_none());
        assert!(safe_filename_segment("tab\there").is_none());
    }

    #[test]
    fn rejects_windows_reserved_chars() {
        for ch in ['<', '>', '*', '?', '"', '|', ':'] {
            let s = format!("ab{ch}cd");
            assert!(safe_filename_segment(&s).is_none(), "char {ch:?}");
        }
    }

    #[test]
    fn rejects_overlong_segments() {
        let too_long = "a".repeat(MAX_SEGMENT_LEN + 1);
        assert!(safe_filename_segment(&too_long).is_none());
        let max = "a".repeat(MAX_SEGMENT_LEN);
        assert!(safe_filename_segment(&max).is_some());
    }

    #[test]
    fn safe_join_returns_path_under_base() {
        let base = Path::new("/var/lib/savfox/agents");
        let path = safe_join(base, "alice", ".json").unwrap();
        assert!(path.starts_with(base));
        assert_eq!(path.file_name().unwrap(), "alice.json");
    }

    #[test]
    fn safe_join_rejects_traversal() {
        let base = Path::new("/var/lib/savfox/agents");
        assert!(safe_join(base, "../../etc/passwd", ".json").is_none());
        assert!(safe_join(base, "..", ".json").is_none());
        assert!(safe_join(base, "a/b", ".json").is_none());
    }

    #[test]
    fn trims_outer_whitespace_then_validates() {
        let base = Path::new("/x");
        // Surrounding whitespace is stripped, then the rest is validated.
        let path = safe_join(base, "  alice  ", ".json").unwrap();
        assert_eq!(path.file_name().unwrap(), "alice.json");
        // But interior whitespace is allowed.
        assert!(safe_filename_segment("name with spaces").is_some());
    }
}
