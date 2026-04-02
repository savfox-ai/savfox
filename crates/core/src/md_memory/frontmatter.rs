use super::entry::MemoryFrontmatter;

const FRONTMATTER_DELIMITER: &str = "---";

/// Parse a Markdown file into (frontmatter, body).
///
/// If the file starts with `---`, everything between the first and second `---`
/// is treated as YAML frontmatter. Otherwise, the entire content is the body.
#[must_use] 
pub fn parse_md_file(content: &str) -> (MemoryFrontmatter, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with(FRONTMATTER_DELIMITER) {
        return (MemoryFrontmatter::default(), content.to_owned());
    }

    // Find the closing delimiter.
    let after_first = &trimmed[FRONTMATTER_DELIMITER.len()..];
    if let Some(end_idx) = after_first.find(&format!("\n{FRONTMATTER_DELIMITER}")) {
        let yaml_str = &after_first[..end_idx];
        let body_start = end_idx + 1 + FRONTMATTER_DELIMITER.len();
        let body = after_first[body_start..]
            .trim_start_matches('\n')
            .trim_start_matches('\r');

        let frontmatter: MemoryFrontmatter = serde_yaml::from_str(yaml_str).unwrap_or_default();

        (frontmatter, body.to_owned())
    } else {
        // No closing delimiter found; treat everything as body.
        (MemoryFrontmatter::default(), content.to_owned())
    }
}

/// Render a `MemoryFrontmatter` + body back into a Markdown string with YAML frontmatter.
#[must_use] 
pub fn render_md_file(frontmatter: &MemoryFrontmatter, body: &str) -> String {
    let yaml = serde_yaml::to_string(frontmatter).unwrap_or_default();
    format!("---\n{yaml}---\n\n{body}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_frontmatter() {
        let input = r#"---
tags: [rust, patterns]
priority: 8
pinned: true
author: user
---

# My Notes

Some content here.
"#;
        let (fm, body) = parse_md_file(input);
        assert_eq!(fm.tags, vec!["rust", "patterns"]);
        assert_eq!(fm.priority, 8);
        assert!(fm.pinned);
        assert_eq!(fm.author, "user");
        assert!(body.starts_with("# My Notes"));
    }

    #[test]
    fn parse_without_frontmatter() {
        let input = "# Just a heading\n\nSome text.";
        let (fm, body) = parse_md_file(input);
        assert_eq!(fm.priority, 5); // default
        assert!(!fm.pinned);
        assert_eq!(body, input);
    }

    #[test]
    fn roundtrip() {
        let fm = MemoryFrontmatter {
            tags: vec!["test".to_string()],
            priority: 7,
            pinned: false,
            author: "agent".to_string(),
            ..Default::default()
        };
        let body = "# Test\n\nHello world.";
        let rendered = render_md_file(&fm, body);
        let (fm2, body2) = parse_md_file(&rendered);
        assert_eq!(fm2.tags, vec!["test"]);
        assert_eq!(fm2.priority, 7);
        assert!(body2.contains("# Test"));
        assert!(body2.contains("Hello world."));
    }
}
