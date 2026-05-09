// Truncate a &str to a byte budget at a char boundary (prefix)
#[inline]
#[must_use]
pub fn take_bytes_at_char_boundary(s: &str, maxb: usize) -> &str {
    if s.len() <= maxb {
        return s;
    }
    let mut last_ok = 0;
    for (i, ch) in s.char_indices() {
        let nb = i + ch.len_utf8();
        if nb > maxb {
            break;
        }
        last_ok = nb;
    }
    &s[..last_ok]
}

// Take a suffix of a &str within a byte budget at a char boundary
#[inline]
#[must_use]
pub fn take_last_bytes_at_char_boundary(s: &str, maxb: usize) -> &str {
    if s.len() <= maxb {
        return s;
    }
    let mut start = s.len();
    let mut used = 0usize;
    for (i, ch) in s.char_indices().rev() {
        let nb = ch.len_utf8();
        if used + nb > maxb {
            break;
        }
        start = i;
        used += nb;
        if start == 0 {
            break;
        }
    }
    &s[start..]
}

/// Normalize text into a lowercase ASCII slug, collapsing separators to `-`.
///
/// Keeps only ASCII letters and digits; treats whitespace and `- _ : / \ .`
/// as separators; trims leading and trailing separators.
#[inline]
#[must_use]
pub fn normalize_slug(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
            continue;
        }

        if (ch.is_ascii_whitespace() || matches!(ch, '-' | '_' | ':' | '/' | '\\' | '.'))
            && !out.is_empty()
            && !prev_dash
        {
            out.push('-');
            prev_dash = true;
        }
    }

    let normalized = out.trim_matches('-');
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_owned())
    }
}

/// Generate a unique account id from a provider id and a human-readable name.
///
/// The name is lowercased, non-alphanumeric characters are replaced with
/// hyphens, and consecutive hyphens are collapsed. When a normalized slug is
/// available, the returned id is `{provider_id}-{slug}`; otherwise the bare
/// `provider_id` is returned.
#[must_use]
pub fn slugify_account_id(provider_id: &str, name: &str) -> String {
    let provider_id = provider_id.trim();
    let slug = normalize_slug(name).unwrap_or_default();

    if slug.is_empty() {
        return provider_id.to_owned();
    }

    format!("{provider_id}-{slug}")
}

#[cfg(test)]
mod tests {
    use super::{normalize_slug, slugify_account_id};

    #[test]
    fn normalize_slug_compacts_separators() {
        assert_eq!(
            normalize_slug("  Matrix Home / Main_Server "),
            Some("matrix-home-main-server".to_owned())
        );
    }

    #[test]
    fn normalize_slug_returns_none_for_empty_result() {
        assert_eq!(normalize_slug(" \t / _ "), None);
        assert_eq!(normalize_slug(""), None);
        assert_eq!(normalize_slug("  "), None);
    }

    #[test]
    fn normalize_slug_basic() {
        assert_eq!(
            normalize_slug("Work Account").as_deref(),
            Some("work-account")
        );
        assert_eq!(
            normalize_slug("My  Work---Account!").as_deref(),
            Some("my-work-account")
        );
        assert_eq!(normalize_slug("OpenAI").as_deref(), Some("openai"));
    }

    #[test]
    fn slugify_account_id_basic() {
        assert_eq!(
            slugify_account_id("openai", "Work Account"),
            "openai-work-account"
        );
    }

    #[test]
    fn slugify_account_id_same_as_provider() {
        assert_eq!(slugify_account_id("openai", "OpenAI"), "openai-openai");
    }

    #[test]
    fn slugify_account_id_empty_name() {
        assert_eq!(slugify_account_id("openai", ""), "openai");
        assert_eq!(slugify_account_id("openai", "  "), "openai");
    }

    #[test]
    fn slugify_account_id_special_chars() {
        assert_eq!(
            slugify_account_id("openai", "My  Work---Account!"),
            "openai-my-work-account"
        );
    }
}
