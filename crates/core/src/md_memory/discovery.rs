use std::path::{Path, PathBuf};

use tracing::warn;

use super::entry::{MAX_MEMORY_FILE_BYTES, MdMemoryEntry, MemoryLayer};
use super::frontmatter::parse_md_file;

/// Sanitize a slug: only allow `[a-z0-9_-]`, reject path separators.
pub fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !slug.starts_with('-')
        && !slug.starts_with('_')
}

/// Sanitize a user-provided slug by replacing invalid characters.
pub fn sanitize_slug(input: &str) -> String {
    let slug: String = input
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Remove leading dashes/underscores.
    slug.trim_start_matches(|c: char| c == '-' || c == '_')
        .to_string()
}

/// Resolve the directory paths for each memory layer.
pub fn layer_dirs(
    savfox_home: &Path,
    project_root: Option<&Path>,
    agent_name: &str,
) -> Vec<(MemoryLayer, PathBuf)> {
    let mut dirs = Vec::with_capacity(3);
    dirs.push((
        MemoryLayer::Global,
        savfox_home.join("memory").join("global"),
    ));
    if let Some(root) = project_root {
        dirs.push((MemoryLayer::Project, root.join(".savfox").join("memory")));
    }
    dirs.push((
        MemoryLayer::Agent,
        savfox_home.join("memory").join("agents").join(agent_name),
    ));
    dirs
}

/// Discover all `.md` memory files across Global, Project, and Agent layers.
///
/// Session-layer entries are managed in-memory and not scanned from disk.
pub async fn discover_md_memories(
    savfox_home: &Path,
    project_root: Option<&Path>,
    agent_name: &str,
) -> Vec<MdMemoryEntry> {
    let dirs = layer_dirs(savfox_home, project_root, agent_name);
    let mut entries = Vec::new();

    for (layer, dir) in &dirs {
        if !dir.exists() {
            continue;
        }
        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(err) => {
                warn!(path = %dir.display(), error = %err, "failed to read memory directory");
                continue;
            }
        };

        for entry_result in read_dir {
            let Ok(dir_entry) = entry_result else {
                continue;
            };
            let path = dir_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };

            // Read the file with size guard.
            let metadata = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if metadata.len() as usize > MAX_MEMORY_FILE_BYTES {
                warn!(path = %path.display(), "memory file exceeds 64KB limit, skipping");
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(err) => {
                    warn!(path = %path.display(), error = %err, "failed to read memory file");
                    continue;
                }
            };

            let (frontmatter, body) = parse_md_file(&content);

            // Skip expired entries.
            if let Some(expires) = frontmatter.expires_at {
                if expires < chrono::Utc::now() {
                    continue;
                }
            }

            let body_bytes = body.len();
            entries.push(MdMemoryEntry {
                slug: slug.to_string(),
                layer: *layer,
                frontmatter,
                body,
                file_path: Some(path),
                body_bytes,
            });
        }
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_slug() {
        assert!(is_valid_slug("my-memory"));
        assert!(is_valid_slug("coding-conventions"));
        assert!(is_valid_slug("a123"));
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("My Memory")); // uppercase + space
        assert!(!is_valid_slug("-leading"));
        assert!(!is_valid_slug("path/sep"));
        assert!(!is_valid_slug("back\\slash"));
    }

    #[test]
    fn test_sanitize_slug() {
        assert_eq!(sanitize_slug("My Cool Memory!"), "my-cool-memory-");
        assert_eq!(sanitize_slug("--leading"), "leading");
        assert_eq!(sanitize_slug("path/to/file"), "path-to-file");
    }
}
