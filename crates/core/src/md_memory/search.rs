use super::entry::MdMemoryEntry;

/// BM25-inspired search over memory entries.
///
/// Searches across slug, body, and tags. Returns entries sorted by relevance score (descending).
pub fn search_memories<'a>(
    entries: &'a [MdMemoryEntry],
    query: &str,
    limit: usize,
) -> Vec<&'a MdMemoryEntry> {
    let query_terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();

    if query_terms.is_empty() {
        return entries.iter().take(limit).collect();
    }

    let k1 = 1.2_f64;
    let b = 0.75_f64;
    let avg_len = 200.0_f64;

    let mut scored: Vec<(&MdMemoryEntry, f64)> = entries
        .iter()
        .map(|entry| {
            let text = format!(
                "{} {} {}",
                entry.slug,
                entry.body,
                entry.frontmatter.tags.join(" ")
            )
            .to_lowercase();
            let doc_len = text.split_whitespace().count() as f64;

            let score: f64 = query_terms
                .iter()
                .map(|term| {
                    let tf = text.matches(term.as_str()).count() as f64;
                    (tf * (k1 + 1.0)) / k1.mul_add(1.0 - b + b * doc_len / avg_len, tf)
                })
                .sum();

            (entry, score)
        })
        .filter(|(_, score)| *score > 0.0)
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    scored
        .into_iter()
        .take(limit)
        .map(|(entry, _)| entry)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::md_memory::entry::{MemoryFrontmatter, MemoryLayer};

    fn make_entry(slug: &str, body: &str, tags: &[&str]) -> MdMemoryEntry {
        MdMemoryEntry {
            slug: slug.to_string(),
            layer: MemoryLayer::Global,
            frontmatter: MemoryFrontmatter {
                tags: tags.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            body: body.to_string(),
            file_path: None,
            body_bytes: body.len(),
        }
    }

    #[test]
    fn search_finds_matching_entries() {
        let entries = vec![
            make_entry("rust-patterns", "Use Result for error handling", &["rust"]),
            make_entry("python-tips", "Use list comprehensions", &["python"]),
        ];
        let results = search_memories(&entries, "rust error", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].slug, "rust-patterns");
    }

    #[test]
    fn search_empty_query_returns_all() {
        let entries = vec![
            make_entry("a", "content a", &[]),
            make_entry("b", "content b", &[]),
        ];
        let results = search_memories(&entries, "", 10);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn search_respects_limit() {
        let entries = vec![
            make_entry("a", "rust rust rust", &["rust"]),
            make_entry("b", "rust", &["rust"]),
            make_entry("c", "rust patterns", &["rust"]),
        ];
        let results = search_memories(&entries, "rust", 2);
        assert_eq!(results.len(), 2);
    }
}
