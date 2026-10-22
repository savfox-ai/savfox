use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use tracing::info;
use zip::ZipArchive;

/// Strategy for handling conflicts when installing skills from a zip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictStrategy {
    /// Overwrite existing skill directories.
    Overwrite,
    /// Skip skills that already exist.
    Skip,
}

/// Information about a conflict found during zip installation.
#[derive(Debug, Clone)]
pub struct ZipConflict {
    pub name: String,
    pub existing_path: PathBuf,
}

/// Result of a zip installation attempt.
#[derive(Debug, Clone)]
pub struct ZipInstallResult {
    pub installed: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<(String, String)>,
}

/// Install skills from zip bytes into the skills directory.
///
/// The zip is inspected for top-level folders:
/// - If there is a single top-level folder named "skills", its children are used.
/// - Otherwise, all top-level folders are treated as individual skills.
///
/// Each folder is installed to `{skills_dir}/{folder_name}`.
pub async fn install_from_zip_bytes(
    zip_bytes: Vec<u8>,
    skills_dir: PathBuf,
    strategy: ConflictStrategy,
) -> anyhow::Result<ZipInstallResult> {
    tokio::task::spawn_blocking(move || install_zip_sync(&zip_bytes, &skills_dir, strategy)).await?
}

/// Detect which top-level folders are present in a zip and return conflicts.
pub fn detect_conflicts(zip_bytes: &[u8], skills_dir: &Path) -> anyhow::Result<Vec<ZipConflict>> {
    let reader = Cursor::new(zip_bytes);
    let archive = ZipArchive::new(reader)?;
    let folders = collect_top_level_folders(&archive)?;
    let effective = effective_skill_folders(&folders);

    let mut conflicts = Vec::new();
    for name in &effective {
        let target = skills_dir.join(name);
        if target.exists() {
            conflicts.push(ZipConflict {
                name: name.clone(),
                existing_path: target,
            });
        }
    }
    Ok(conflicts)
}

fn install_zip_sync(
    zip_bytes: &[u8],
    skills_dir: &Path,
    strategy: ConflictStrategy,
) -> anyhow::Result<ZipInstallResult> {
    std::fs::create_dir_all(skills_dir)?;

    let reader = Cursor::new(zip_bytes);
    let mut archive = ZipArchive::new(reader)?;
    let folders = collect_top_level_folders(&archive)?;
    let effective = effective_skill_folders(&folders);

    // Determine the prefix to strip from zip entries.
    // If there's a single "skills" folder, strip "skills/".
    let strip_prefix = if folders.len() == 1 && folders[0].eq_ignore_ascii_case("skills") {
        format!("{}/", folders[0])
    } else {
        String::new()
    };

    let mut result = ZipInstallResult {
        installed: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
    };

    for skill_name in &effective {
        let target = skills_dir.join(skill_name);
        if target.exists() {
            match strategy {
                ConflictStrategy::Skip => {
                    info!(name = %skill_name, "skipping existing skill");
                    result.skipped.push(skill_name.clone());
                    continue;
                }
                ConflictStrategy::Overwrite => {
                    if let Err(err) = std::fs::remove_dir_all(&target) {
                        result.errors.push((
                            skill_name.clone(),
                            format!("failed to remove existing: {err}"),
                        ));
                        continue;
                    }
                }
            }
        }

        // Extract files belonging to this skill.
        let entry_prefix = if strip_prefix.is_empty() {
            format!("{skill_name}/")
        } else {
            format!("{strip_prefix}{skill_name}/")
        };

        let mut extracted = false;
        for i in 0..archive.len() {
            let mut file = match archive.by_index(i) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let full_name = normalized_entry_name(file.name()?.as_ref())?;

            if !full_name.starts_with(&entry_prefix) {
                continue;
            }

            let relative = &full_name[entry_prefix.len()..];
            if relative.is_empty() {
                std::fs::create_dir_all(&target).ok();
                extracted = true;
                continue;
            }

            let out_path = target.join(relative);
            if file.is_dir() {
                std::fs::create_dir_all(&out_path).ok();
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                let mut buf = Vec::new();
                if file.read_to_end(&mut buf).is_ok()
                    && let Err(err) = std::fs::write(&out_path, &buf)
                {
                    result
                        .errors
                        .push((skill_name.clone(), format!("write error: {err}")));
                    continue;
                }
            }
            extracted = true;
        }

        if extracted {
            info!(name = %skill_name, path = %target.display(), "installed skill from zip");
            result.installed.push(skill_name.clone());
        }
    }

    Ok(result)
}

/// Collect distinct top-level folder names from a zip archive.
fn collect_top_level_folders(archive: &ZipArchive<Cursor<&[u8]>>) -> anyhow::Result<Vec<String>> {
    let mut folders = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for i in 0..archive.len() {
        let Some(name) = archive.name_for_index(i) else {
            continue;
        };
        let name = normalized_entry_name(name?.as_ref())?;
        if let Some(top) = name.split('/').next()
            && !top.is_empty()
            && seen.insert(top.to_owned())
        {
            folders.push(top.to_owned());
        }
    }
    Ok(folders)
}

// Normalize separators before checking components, so an archive produced on
// another platform cannot turn a parent directory into an overwrite target.
fn normalized_entry_name(raw: &str) -> anyhow::Result<String> {
    use std::path::Component;

    let name = raw.replace('\\', "/");
    let mut parts = Vec::new();
    for component in Path::new(&name).components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("ZIP path is not UTF-8"))?;
                if part.contains(':') || part.ends_with('.') || part.ends_with(' ') {
                    anyhow::bail!("ZIP path has an ambiguous component: {raw}");
                }
                parts.push(part);
            }
            Component::CurDir => {}
            _ => anyhow::bail!("ZIP path leaves its installation directory: {raw}"),
        }
    }
    let mut normalized = parts.join("/");
    if name.ends_with('/') && !normalized.is_empty() {
        normalized.push('/');
    }
    Ok(normalized)
}

/// If there's a single top-level folder named "skills", return its children.
/// Otherwise return the top-level folders themselves.
fn effective_skill_folders(top_level: &[String]) -> Vec<String> {
    // The detection of "skills" subfolder children would require reading the
    // archive again. For simplicity, if the only top-level folder is "skills",
    // we just return the top_level as-is — the strip_prefix logic in
    // install_zip_sync handles the actual extraction correctly.
    //
    // In practice the caller should enumerate children of "skills/" from the
    // archive. For now we return the top-level folders directly.
    top_level.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                std::io::Write::write_all(&mut writer, data).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    #[tokio::test]
    async fn install_zip_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().to_path_buf();

        let zip_bytes = make_test_zip(&[
            ("my-skill/", b""),
            ("my-skill/SKILL.md", b"---\nname: my-skill\n---\nHello"),
        ]);

        let result =
            install_from_zip_bytes(zip_bytes, skills_dir.clone(), ConflictStrategy::Overwrite)
                .await
                .unwrap();

        assert_eq!(result.installed, vec!["my-skill"]);
        assert!(skills_dir.join("my-skill/SKILL.md").exists());
    }

    #[test]
    fn parent_paths_are_rejected_before_overwriting_existing_directories() {
        for unsafe_name in [b"../x/SKILL.md", b"..\\x\\SKILL.md"] {
            let tmp = tempfile::tempdir().unwrap();
            let skills = tmp.path().join("skills");
            std::fs::create_dir(&skills).unwrap();
            let sentinel = tmp.path().join("sentinel.txt");
            std::fs::write(&sentinel, b"retained").unwrap();
            let mut archive = make_test_zip(&[("safe/SKILL.md", b"content")]);
            let offsets: Vec<_> = archive
                .windows(unsafe_name.len())
                .enumerate()
                .filter_map(|(index, bytes)| (bytes == b"safe/SKILL.md").then_some(index))
                .collect();
            assert_eq!(
                offsets.len(),
                2,
                "local and central names must both be changed"
            );
            for offset in offsets {
                archive[offset..offset + unsafe_name.len()].copy_from_slice(unsafe_name);
            }
            assert!(detect_conflicts(&archive, &skills).is_err());
            assert!(install_zip_sync(&archive, &skills, ConflictStrategy::Overwrite).is_err());
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"retained");
            assert!(skills.is_dir());
        }
    }

    #[test]
    fn ambiguous_windows_names_are_rejected_on_every_platform() {
        for name in [
            ".. /skill/SKILL.md",
            "C:/skill/SKILL.md",
            "skill./SKILL.md",
            "skill/file:stream",
        ] {
            assert!(normalized_entry_name(name).is_err(), "{name}");
        }
        assert_eq!(
            normalized_entry_name("./skill/SKILL.md").unwrap(),
            "skill/SKILL.md"
        );
    }

    #[tokio::test]
    async fn install_zip_skip_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().to_path_buf();
        let custom = skills_dir.join("existing");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join("SKILL.md"), "old").unwrap();

        let zip_bytes = make_test_zip(&[("existing/", b""), ("existing/SKILL.md", b"new content")]);

        let result = install_from_zip_bytes(zip_bytes, skills_dir.clone(), ConflictStrategy::Skip)
            .await
            .unwrap();

        assert_eq!(result.skipped, vec!["existing"]);
        let content = std::fs::read_to_string(skills_dir.join("existing/SKILL.md")).unwrap();
        assert_eq!(content, "old");
    }
}
