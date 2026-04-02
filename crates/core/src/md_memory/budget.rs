use super::entry::{MdMemoryEntry, MemoryLayer};

/// Default maximum bytes for the assembled memory prompt.
pub const DEFAULT_MD_MEMORY_MAX_BYTES: usize = 16_384;

/// Budget share for each layer (after pinned entries are included).
struct LayerBudget {
    layer: MemoryLayer,
    share: f64,
}

const LAYER_BUDGETS: &[LayerBudget] = &[
    LayerBudget {
        layer: MemoryLayer::Session,
        share: 0.40,
    },
    LayerBudget {
        layer: MemoryLayer::Agent,
        share: 0.25,
    },
    LayerBudget {
        layer: MemoryLayer::Project,
        share: 0.25,
    },
    LayerBudget {
        layer: MemoryLayer::Global,
        share: 0.10,
    },
];

/// Assemble the memory prompt from discovered entries within a byte budget.
///
/// 1. Pinned entries are always included first.
/// 2. Remaining budget is split across layers by share.
/// 3. Within each layer, entries are sorted by priority (desc), then updated_at (desc).
#[must_use] 
pub fn assemble_memory_prompt(entries: &[MdMemoryEntry], max_bytes: usize) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut output = String::with_capacity(max_bytes.min(4096));
    output.push_str("# Memory Context\n");

    let mut used_bytes = output.len();

    // 1. Include pinned entries first.
    let mut pinned: Vec<&MdMemoryEntry> = entries.iter().filter(|e| e.frontmatter.pinned).collect();
    pinned.sort_by(|a, b| b.frontmatter.priority.cmp(&a.frontmatter.priority));

    let mut included_slugs = Vec::new();

    for entry in &pinned {
        let section = format_entry(entry);
        if used_bytes + section.len() > max_bytes {
            break;
        }
        output.push_str(&section);
        used_bytes += section.len();
        included_slugs.push(entry.slug.as_str());
    }

    // 2. Allocate remaining budget across layers.
    let remaining = max_bytes.saturating_sub(used_bytes);

    for lb in LAYER_BUDGETS {
        let layer_budget = (remaining as f64 * lb.share) as usize;
        if layer_budget == 0 {
            continue;
        }

        let mut layer_entries: Vec<&MdMemoryEntry> = entries
            .iter()
            .filter(|e| {
                e.layer == lb.layer
                    && !e.frontmatter.pinned
                    && !included_slugs.contains(&e.slug.as_str())
            })
            .collect();

        // Sort: priority desc, then updated_at desc.
        layer_entries.sort_by(|a, b| {
            b.frontmatter
                .priority
                .cmp(&a.frontmatter.priority)
                .then_with(|| {
                    let a_time = a.frontmatter.updated_at.unwrap_or_default();
                    let b_time = b.frontmatter.updated_at.unwrap_or_default();
                    b_time.cmp(&a_time)
                })
        });

        let mut layer_used = 0usize;
        for entry in &layer_entries {
            let section = format_entry(entry);
            if layer_used + section.len() > layer_budget {
                break;
            }
            if used_bytes + section.len() > max_bytes {
                break;
            }
            output.push_str(&section);
            used_bytes += section.len();
            layer_used += section.len();
            included_slugs.push(entry.slug.as_str());
        }
    }

    output
}

fn format_entry(entry: &MdMemoryEntry) -> String {
    let layer_label = match entry.layer {
        MemoryLayer::Agent => "agent".to_owned(),
        other => other.as_str().to_owned(),
    };
    format!("\n## [{}] {}\n{}\n", layer_label, entry.slug, entry.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::md_memory::entry::MemoryFrontmatter;

    fn make_entry(slug: &str, layer: MemoryLayer, priority: u32, body: &str) -> MdMemoryEntry {
        MdMemoryEntry {
            slug: slug.to_string(),
            layer,
            frontmatter: MemoryFrontmatter {
                priority,
                ..Default::default()
            },
            body: body.to_string(),
            file_path: None,
            body_bytes: body.len(),
        }
    }

    #[test]
    fn empty_entries_returns_empty() {
        assert!(assemble_memory_prompt(&[], 1024).is_empty());
    }

    #[test]
    fn pinned_entries_first() {
        let mut pinned = make_entry("pinned", MemoryLayer::Global, 10, "Always here");
        pinned.frontmatter.pinned = true;

        let normal = make_entry("normal", MemoryLayer::Global, 5, "Maybe here");

        let result = assemble_memory_prompt(&[normal, pinned], 4096);
        let pinned_pos = result.find("[global] pinned").unwrap();
        let normal_pos = result.find("[global] normal").unwrap();
        assert!(pinned_pos < normal_pos);
    }

    #[test]
    fn respects_budget() {
        let big = make_entry("big", MemoryLayer::Global, 5, &"x".repeat(20_000));
        let result = assemble_memory_prompt(&[big], 100);
        // Should only contain the header, not the big entry body.
        assert!(result.len() <= 200); // header + maybe partial
    }
}
