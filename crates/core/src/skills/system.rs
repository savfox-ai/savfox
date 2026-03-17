use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use include_dir::Dir;
use savfox_utils::absolute_path::AbsolutePathBuf;
use thiserror::Error;

const SYSTEM_SKILLS_DIR: Dir =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/src/skills/assets/samples");

const SYSTEM_SKILLS_DIR_NAME: &str = ".system";
const SKILLS_DIR_NAME: &str = "skills";
const SYSTEM_SKILLS_MARKER_FILENAME: &str = ".system.marker";
const SYSTEM_SKILLS_MARKER_SALT: &str = "v1";

/// Returns the on-disk cache location for embedded system skills.
///
/// This is typically located at `SAVFOX_HOME/skills/.system`.
pub(crate) fn system_cache_root_dir(savfox_home: &Path) -> PathBuf {
    AbsolutePathBuf::try_from(savfox_home)
        .and_then(|savfox_home| system_cache_root_dir_abs(&savfox_home))
        .map(AbsolutePathBuf::into_path_buf)
        .unwrap_or_else(|_| {
            savfox_home
                .join(SKILLS_DIR_NAME)
                .join(SYSTEM_SKILLS_DIR_NAME)
        })
}

fn system_cache_root_dir_abs(savfox_home: &AbsolutePathBuf) -> std::io::Result<AbsolutePathBuf> {
    savfox_home
        .join(SKILLS_DIR_NAME)?
        .join(SYSTEM_SKILLS_DIR_NAME)
}

/// Installs embedded system skills into `SAVFOX_HOME/skills/.system`.
///
/// Clears any existing system skills directory first and then writes the embedded
/// skills directory into place.
///
/// To avoid doing unnecessary work on every startup, a marker file is written
/// with a fingerprint of the embedded directory. When the marker matches, the
/// install is skipped.
pub(crate) fn install_system_skills(
    savfox_home: &Path,
    registry_config: Option<&savfox_config::types::RegistryConfig>,
) -> Result<(), SystemSkillsError> {
    let savfox_home = AbsolutePathBuf::try_from(savfox_home)
        .map_err(|source| SystemSkillsError::io("normalize savfox home dir", source))?;
    let skills_root_dir = savfox_home
        .join(SKILLS_DIR_NAME)
        .map_err(|source| SystemSkillsError::io("resolve skills root dir", source))?;
    fs::create_dir_all(skills_root_dir.as_path())
        .map_err(|source| SystemSkillsError::io("create skills root dir", source))?;

    let dest_system = system_cache_root_dir_abs(&savfox_home)
        .map_err(|source| SystemSkillsError::io("resolve system skills cache root dir", source))?;

    let marker_path = dest_system
        .join(SYSTEM_SKILLS_MARKER_FILENAME)
        .map_err(|source| SystemSkillsError::io("resolve system skills marker path", source))?;
    let expected_fingerprint = embedded_system_skills_fingerprint();
    if dest_system.as_path().is_dir()
        && read_marker(&marker_path).is_ok_and(|marker| marker == expected_fingerprint)
    {
        // System skills are up to date. Still ensure the default registry is cloned.
        ensure_default_registry_cloned(savfox_home.as_path(), registry_config);
        return Ok(());
    }

    if dest_system.as_path().exists() {
        fs::remove_dir_all(dest_system.as_path())
            .map_err(|source| SystemSkillsError::io("remove existing system skills dir", source))?;
    }

    write_embedded_dir(&SYSTEM_SKILLS_DIR, &dest_system)?;
    fs::write(marker_path.as_path(), format!("{expected_fingerprint}\n"))
        .map_err(|source| SystemSkillsError::io("write system skills marker", source))?;

    // Clone the default registry on first install.
    ensure_default_registry_cloned(skills_root_dir.as_path(), registry_config);

    Ok(())
}

/// Derive a `{domain}/{restpath}` folder name from a git URL.
///
/// Example: `https://github.com/savhub-ai/registry.git` → `github.com/savhub-ai/registry`
fn registry_dir_from_url(git_url: &str) -> String {
    let stripped = git_url
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let after_scheme = stripped
        .split("://")
        .nth(1)
        .unwrap_or(stripped);
    after_scheme
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Clone the skill registry if its folder doesn't exist yet.
///
/// The registry is cloned to `{savfox_home}/registry/{domain}/{org}/{repo}`
/// derived from the git URL. Uses the config if provided, otherwise falls
/// back to the default registry URL.
///
/// This is a best-effort operation — failures are logged but do not
/// prevent startup.
fn ensure_default_registry_cloned(
    savfox_home: &Path,
    registry_config: Option<&savfox_config::types::RegistryConfig>,
) {
    let default_config = savfox_config::types::RegistryConfig::default();
    let config = registry_config.unwrap_or(&default_config);
    let git_url = &config.git;
    let dir_name = registry_dir_from_url(git_url);
    let registry_dir = savfox_home.join("registry").join(&dir_name);
    if registry_dir.is_dir() {
        return;
    }

    tracing::info!(url = %git_url, path = %registry_dir.display(), "cloning skill registry");

    if let Some(parent) = registry_dir.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            tracing::warn!(error = %err, "failed to create registry dir");
            return;
        }
    }

    match std::process::Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            git_url,
            &registry_dir.to_string_lossy(),
        ])
        .output()
    {
        Ok(output) if output.status.success() => {
            tracing::info!("skill registry cloned successfully");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(error = %stderr.trim(), "failed to clone registry");
        }
        Err(err) => {
            tracing::warn!(error = %err, "git not available, skipping registry clone");
        }
    }
}

fn read_marker(path: &AbsolutePathBuf) -> Result<String, SystemSkillsError> {
    Ok(fs::read_to_string(path.as_path())
        .map_err(|source| SystemSkillsError::io("read system skills marker", source))?
        .trim()
        .to_string())
}

fn embedded_system_skills_fingerprint() -> String {
    let mut items = Vec::new();
    collect_fingerprint_items(&SYSTEM_SKILLS_DIR, &mut items);
    items.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));

    let mut hasher = DefaultHasher::new();
    SYSTEM_SKILLS_MARKER_SALT.hash(&mut hasher);
    for (path, contents_hash) in items {
        path.hash(&mut hasher);
        contents_hash.hash(&mut hasher);
    }
    format!("{:x}", hasher.finish())
}

fn collect_fingerprint_items(dir: &Dir<'_>, items: &mut Vec<(String, Option<u64>)>) {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                items.push((subdir.path().to_string_lossy().to_string(), None));
                collect_fingerprint_items(subdir, items);
            }
            include_dir::DirEntry::File(file) => {
                let mut file_hasher = DefaultHasher::new();
                file.contents().hash(&mut file_hasher);
                items.push((
                    file.path().to_string_lossy().to_string(),
                    Some(file_hasher.finish()),
                ));
            }
        }
    }
}

/// Writes the embedded `include_dir::Dir` to disk under `dest`.
///
/// Preserves the embedded directory structure.
fn write_embedded_dir(dir: &Dir<'_>, dest: &AbsolutePathBuf) -> Result<(), SystemSkillsError> {
    fs::create_dir_all(dest.as_path())
        .map_err(|source| SystemSkillsError::io("create system skills dir", source))?;

    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                let subdir_dest = dest.join(subdir.path()).map_err(|source| {
                    SystemSkillsError::io("resolve system skills subdir", source)
                })?;
                fs::create_dir_all(subdir_dest.as_path()).map_err(|source| {
                    SystemSkillsError::io("create system skills subdir", source)
                })?;
                write_embedded_dir(subdir, dest)?;
            }
            include_dir::DirEntry::File(file) => {
                let path = dest.join(file.path()).map_err(|source| {
                    SystemSkillsError::io("resolve system skills file", source)
                })?;
                if let Some(parent) = path.as_path().parent() {
                    fs::create_dir_all(parent).map_err(|source| {
                        SystemSkillsError::io("create system skills file parent", source)
                    })?;
                }
                fs::write(path.as_path(), file.contents())
                    .map_err(|source| SystemSkillsError::io("write system skill file", source))?;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum SystemSkillsError {
    #[error("io error while {action}: {source}")]
    Io {
        action: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl SystemSkillsError {
    fn io(action: &'static str, source: std::io::Error) -> Self {
        Self::Io { action, source }
    }
}

#[cfg(test)]
mod tests {
    use super::{SYSTEM_SKILLS_DIR, collect_fingerprint_items};

    #[test]
    fn fingerprint_traverses_nested_entries() {
        let mut items = Vec::new();
        collect_fingerprint_items(&SYSTEM_SKILLS_DIR, &mut items);
        let mut paths: Vec<String> = items.into_iter().map(|(path, _)| path).collect();
        paths.sort_unstable();

        assert!(
            paths
                .binary_search_by(|probe| probe.as_str().cmp("skill-creator/SKILL.md"))
                .is_ok()
        );
        assert!(
            paths
                .binary_search_by(|probe| probe.as_str().cmp("skill-creator/scripts/init_skill.py"))
                .is_ok()
        );
    }
}
