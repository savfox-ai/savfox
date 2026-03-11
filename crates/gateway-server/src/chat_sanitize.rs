//! Chat message sanitization  - prevents XSS, script injection, and dangerous URL schemes.

/// Sanitize a chat message for safe display.
/// Strips potentially dangerous content while preserving safe markdown.
pub fn sanitize_message(input: &str) -> String {
    let mut output = input.to_string();

    // Strip HTML tags (keep content)
    output = strip_html_tags(&output);

    // Neutralize dangerous URL schemes
    output = neutralize_dangerous_urls(&output);

    // Strip zero-width characters that could be used for obfuscation
    output = strip_zero_width_chars(&output);

    output
}

/// Remove HTML tags but keep inner text
fn strip_html_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_tag = false;

    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

/// Neutralize dangerous URL schemes (javascript:, data:, vbscript:)
fn neutralize_dangerous_urls(input: &str) -> String {
    let dangerous_schemes = ["javascript:", "vbscript:", "data:text/html"];
    let mut output = input.to_string();

    for scheme in &dangerous_schemes {
        // Case-insensitive replacement
        let lower = output.to_lowercase();
        while let Some(pos) = lower.find(scheme) {
            let end = pos + scheme.len();
            output.replace_range(pos..end, &"_".repeat(scheme.len()));
            break; // re-check after replacement
        }
    }
    output
}

/// Strip zero-width and invisible Unicode characters
fn strip_zero_width_chars(input: &str) -> String {
    input
        .chars()
        .filter(|c| {
            !matches!(
                *c,
                '\u{200B}'  // zero-width space
                | '\u{200C}' // zero-width non-joiner
                | '\u{200D}' // zero-width joiner
                | '\u{FEFF}' // BOM / zero-width no-break space
                | '\u{2060}' // word joiner
                | '\u{2061}' // function application
                | '\u{2062}' // invisible times
                | '\u{2063}' // invisible separator
                | '\u{2064}' // invisible plus
            )
        })
        .collect()
}

/// Validate that a message is not empty after sanitization
pub fn is_valid_message(msg: &str) -> bool {
    let trimmed = msg.trim();
    !trimmed.is_empty() && trimmed.len() <= 100_000 // 100KB max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html() {
        assert_eq!(
            sanitize_message("<script>alert('xss')</script>hello"),
            "alert('xss')hello"
        );
        assert_eq!(sanitize_message("<b>bold</b>"), "bold");
    }

    #[test]
    fn test_dangerous_urls() {
        let result = sanitize_message("click [here](javascript:alert(1))");
        assert!(!result.contains("javascript:"));
    }

    #[test]
    fn test_valid_message() {
        assert!(is_valid_message("hello"));
        assert!(!is_valid_message(""));
        assert!(!is_valid_message("   "));
    }
}
