use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use savfox_keyring_store::{DefaultKeyringStore, KeyringStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::json_store;

const SKILL_ENV_KEYRING_SERVICE: &str = "savfox.skills";
const SKILL_MANIFEST_FILE: &str = "SKILL.md";

const CATEGORY_WORKSPACE: &str = "workspace";
const CATEGORY_BUILTIN: &str = "built-in";
const CATEGORY_INSTALLED: &str = "installed";
const CATEGORY_EXTRA: &str = "extra";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SkillsEnvState {
    #[serde(default)]
    values: HashMap<String, String>,
}

fn env_state_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("skills-env.json")
}

async fn load_env_state(savfox_home: &Path) -> Result<SkillsEnvState, String> {
    json_store::load_json(&env_state_path(savfox_home), "skills env state").await
}

async fn save_env_state(savfox_home: &Path, state: &SkillsEnvState) -> Result<(), String> {
    json_store::save_json(&env_state_path(savfox_home), state, "skills env state").await
}

fn command_in_path(command: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat", "com"] {
                if dir.join(format!("{command}.{ext}")).is_file() {
                    return true;
                }
            }
        }
    }
    false
}

fn keyring_value_present(key: &str, keyring: &dyn KeyringStore) -> bool {
    keyring
        .load(SKILL_ENV_KEYRING_SERVICE, key)
        .ok()
        .flatten()
        .is_some_and(|v| !v.trim().is_empty())
}

fn env_value_present(key: &str, env_state: &SkillsEnvState, keyring: &dyn KeyringStore) -> bool {
    if std::env::var(key)
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
    {
        return true;
    }
    if keyring_value_present(key, keyring) {
        return true;
    }
    env_state
        .values
        .get(key)
        .is_some_and(|v| !v.trim().is_empty())
}

fn parse_allowlist() -> Option<HashSet<String>> {
    let raw = std::env::var("SAVFOX_SKILLS_ALLOWLIST").ok()?;
    let items: HashSet<String> = raw
        .split(',')
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect();
    if items.is_empty() { None } else { Some(items) }
}

fn category_rank(category: &str) -> u8 {
    match category {
        CATEGORY_WORKSPACE => 0,
        CATEGORY_INSTALLED => 1,
        CATEGORY_BUILTIN => 2,
        CATEGORY_EXTRA => 3,
        _ => 10,
    }
}

fn collect_skill_manifests(
    root: &Path,
    category: &str,
    max_depth: usize,
    skip_system_subtree: bool,
) -> Vec<(PathBuf, &'static str)> {
    let mut manifests = Vec::new();
    if !root.is_dir() {
        return manifests;
    }

    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > max_depth {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|v| v.to_str()) {
                    // Skip dot-prefixed directories at the skills root level.
                    if skip_system_subtree
                        && (name == ".system" || name == ".registry")
                    {
                        continue;
                    }
                }
                stack.push((path, depth + 1));
            } else if path
                .file_name()
                .and_then(|v| v.to_str())
                .is_some_and(|name| name == SKILL_MANIFEST_FILE)
            {
                let category = match category {
                    CATEGORY_WORKSPACE => CATEGORY_WORKSPACE,
                    CATEGORY_BUILTIN => CATEGORY_BUILTIN,
                    CATEGORY_INSTALLED => CATEGORY_INSTALLED,
                    CATEGORY_EXTRA => CATEGORY_EXTRA,
                    _ => CATEGORY_EXTRA,
                };
                manifests.push((path, category));
            }
        }
    }
    manifests
}

async fn required_os_list(manifest_path: &Path) -> Vec<String> {
    let Ok(content) = tokio::fs::read_to_string(manifest_path).await else {
        return Vec::new();
    };
    let trimmed = content.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return Vec::new();
    };
    let Some(closing) = rest.find("\n---") else {
        return Vec::new();
    };
    let yaml = &rest[..closing];
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else {
        return Vec::new();
    };

    let Some(required_os) = value
        .get("metadata")
        .and_then(|v| v.get("savfox"))
        .and_then(|v| v.get("requires"))
        .and_then(|v| v.get("os"))
        .and_then(|v| v.as_sequence())
    else {
        return Vec::new();
    };

    required_os
        .iter()
        .filter_map(|item| item.as_str().map(|s| s.to_ascii_lowercase()))
        .collect()
}

const DISABLED_MARKER: &str = ".disabled";

/// Check whether a skill directory has the `.disabled` marker file.
fn is_skill_disabled(manifest_path: &Path) -> bool {
    manifest_path
        .parent()
        .map(|dir| dir.join(DISABLED_MARKER).exists())
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
struct SkillBinRow {
    name: String,
    version: Option<String>,
    installed: bool,
    enabled: bool,
    category: &'static str,
    description: Option<String>,
    eligible: bool,
    missing_deps: Vec<String>,
    primary_env: Option<String>,
    env_set: Option<bool>,
    disabled_reason: Option<String>,
    allowlist_blocked: bool,
    /// The directory that contains this skill's SKILL.md.
    skill_dir: Option<PathBuf>,
}

/// Return summary counts derived purely from manifest discovery.
pub(crate) async fn status(savfox_home: &Path) -> Result<Value, String> {
    let bins_val = bins(savfox_home).await?;
    let all = bins_val
        .get("bins")
        .and_then(|b| b.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    let installed_count = bins_val
        .get("bins")
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|v| {
                    v.get("installed")
                        .and_then(|i| i.as_bool())
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);
    Ok(json!({
        "installed_count": installed_count,
        "available_count": all,
    }))
}

/// Discover all skills from manifest directories. No `skills-state.json`.
///
/// "installed" is determined solely by the manifest's directory location:
/// skills under `$SAVFOX_HOME/skills/` (non-.system) are installed.
/// Skills under `.system/` are built-in.  Workspace & extra are discovered
/// from their respective directories.
pub(crate) async fn bins(savfox_home: &Path) -> Result<Value, String> {
    let env_state = load_env_state(savfox_home).await?;
    let keyring = DefaultKeyringStore;
    let allowlist = parse_allowlist();
    let current_os = std::env::consts::OS.to_ascii_lowercase();

    let mut discovered: Vec<(PathBuf, &'static str, bool)> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        discovered.extend(
            collect_skill_manifests(
                &cwd.join(".savfox").join("skills"),
                CATEGORY_WORKSPACE,
                4,
                false,
            )
            .into_iter()
            .map(|(path, category)| (path, category, false)),
        );
    }
    discovered.extend(
        collect_skill_manifests(
            &savfox_home.join("skills").join(".system"),
            CATEGORY_BUILTIN,
            4,
            false,
        )
        .into_iter()
        .map(|(path, category)| (path, category, true)),
    );
    discovered.extend(
        collect_skill_manifests(&savfox_home.join("skills"), CATEGORY_INSTALLED, 6, true)
            .into_iter()
            .map(|(path, category)| (path, category, true)),
    );
    if let Some(home) = dirs::home_dir() {
        discovered.extend(
            collect_skill_manifests(
                &home.join(".agents").join("skills"),
                CATEGORY_EXTRA,
                4,
                false,
            )
            .into_iter()
            .map(|(path, category)| (path, category, false)),
        );
    }

    let mut rows: BTreeMap<String, (u8, SkillBinRow)> = BTreeMap::new();
    for (manifest_path, category, installed_by_location) in discovered {
        let Ok(manifest) = savfox_skill_registry::load_skill_manifest_async(&manifest_path).await
        else {
            continue;
        };
        let name = manifest.name.trim().to_string();
        if name.is_empty() {
            continue;
        }

        let requirements = manifest.metadata.savfox.requires;
        let mut missing_deps: Vec<String> = Vec::new();
        for bin in &requirements.bins {
            if !command_in_path(bin) {
                missing_deps.push(format!("bin:{bin}"));
            }
        }
        for env_key in &requirements.env {
            if !env_value_present(env_key, &env_state, &keyring) {
                missing_deps.push(format!("env:{env_key}"));
            }
        }
        let required_os = required_os_list(&manifest_path).await;
        if !required_os.is_empty() && !required_os.iter().any(|os| os == &current_os) {
            missing_deps.push(format!("os:{}", required_os.join("|")));
        }

        let allowlist_blocked = allowlist
            .as_ref()
            .is_some_and(|list| !list.contains(&name.to_ascii_lowercase()));
        let primary_env = requirements.env.first().cloned();
        let env_set = primary_env
            .as_ref()
            .map(|key| env_value_present(key, &env_state, &keyring));
        let user_disabled = is_skill_disabled(&manifest_path);
        let disabled_reason = if allowlist_blocked {
            Some("blocked by allowlist".to_string())
        } else if user_disabled {
            Some("disabled by user".to_string())
        } else {
            None
        };
        let eligible = missing_deps.is_empty() && !allowlist_blocked;
        let enabled = eligible && !user_disabled;

        let row = SkillBinRow {
            name: name.clone(),
            version: manifest.version,
            installed: installed_by_location,
            enabled,
            category,
            description: if manifest.description.trim().is_empty() {
                None
            } else {
                Some(manifest.description)
            },
            eligible,
            missing_deps,
            primary_env,
            env_set,
            disabled_reason,
            allowlist_blocked,
            skill_dir: manifest_path.parent().map(Path::to_path_buf),
        };

        let rank = category_rank(category);
        match rows.get(&name) {
            Some((current_rank, _)) if *current_rank <= rank => {}
            _ => {
                rows.insert(name, (rank, row));
            }
        }
    }

    let bins: Vec<Value> = rows
        .into_values()
        .map(|(_, row)| {
            json!({
                "name": row.name,
                "version": row.version,
                "installed": row.installed,
                "enabled": row.enabled,
                "category": row.category,
                "description": row.description,
                "eligible": row.eligible,
                "missing_deps": row.missing_deps,
                "primary_env": row.primary_env,
                "env_set": row.env_set,
                "disabled_reason": row.disabled_reason,
                "allowlist_blocked": row.allowlist_blocked,
                "command": format!("savfox-skill-{}", row.name),
            })
        })
        .collect();

    Ok(json!({ "bins": bins }))
}

/// Toggle a skill's enabled state by creating/removing a `.disabled` marker
/// file in the skill's directory.  The skill is located by scanning all
/// manifest directories for a matching name.
pub(crate) async fn set_enabled(
    savfox_home: &Path,
    name: &str,
    enabled: bool,
) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("missing skill name".to_string());
    }

    // Discover skill directories to find the matching skill.
    let bins_val = bins(savfox_home).await?;
    let _bins_arr = bins_val
        .get("bins")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // We need to find the actual skill directory. Re-scan manifests to get paths.
    let skill_dir = find_skill_dir(savfox_home, name);
    let Some(dir) = skill_dir else {
        return Err(format!("skill not found: {name}"));
    };

    let marker = dir.join(DISABLED_MARKER);
    if enabled {
        // Remove the .disabled marker if it exists.
        let _ = tokio::fs::remove_file(&marker).await;
    } else {
        // Create the .disabled marker.
        let _ = tokio::fs::create_dir_all(&dir).await;
        if let Err(err) = tokio::fs::write(&marker, "disabled by user\n").await {
            return Err(format!("failed to write disabled marker: {err}"));
        }
    }

    let disabled_reason = if enabled {
        None
    } else {
        Some("disabled by user")
    };

    Ok(json!({
        "name": name,
        "enabled": enabled,
        "disabled_reason": disabled_reason,
        "status": "updated",
    }))
}

/// Find the skill directory for a given skill name by scanning manifest dirs.
fn find_skill_dir(savfox_home: &Path, name: &str) -> Option<PathBuf> {
    let system_dir = savfox_home.join("skills").join(".system");
    let skills_dir = savfox_home.join("skills");

    let dirs_to_scan: Vec<(&Path, bool)> = vec![(&system_dir, false), (&skills_dir, true)];

    for (root, skip_system) in dirs_to_scan {
        for (manifest_path, _) in collect_skill_manifests(root, CATEGORY_INSTALLED, 4, skip_system)
        {
            if let Some(parsed_name) = quick_read_skill_name(&manifest_path) {
                if parsed_name.eq_ignore_ascii_case(name) {
                    return manifest_path.parent().map(Path::to_path_buf);
                }
            }
        }
    }

    // Also check workspace and extra dirs.
    if let Ok(cwd) = std::env::current_dir() {
        let ws_dir = cwd.join(".savfox").join("skills");
        for (manifest_path, _) in collect_skill_manifests(&ws_dir, CATEGORY_WORKSPACE, 4, false) {
            if let Some(parsed_name) = quick_read_skill_name(&manifest_path) {
                if parsed_name.eq_ignore_ascii_case(name) {
                    return manifest_path.parent().map(Path::to_path_buf);
                }
            }
        }
    }

    None
}

/// Read just the skill name from a SKILL.md manifest (sync, for scanning).
fn quick_read_skill_name(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim_start();
    let rest = trimmed.strip_prefix("---")?;
    let closing = rest.find("\n---")?;
    let yaml = &rest[..closing];
    for line in yaml.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("name:") {
            let name = value.trim().trim_matches('"').trim_matches('\'').trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

pub(crate) async fn set_env(savfox_home: &Path, key: &str, value: &str) -> Result<Value, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("missing env key".to_string());
    }
    let keyring = DefaultKeyringStore;
    let storage = if keyring.save(SKILL_ENV_KEYRING_SERVICE, key, value).is_ok() {
        let mut state = load_env_state(savfox_home).await?;
        state.values.remove(key);
        save_env_state(savfox_home, &state).await?;
        "keyring"
    } else {
        let mut state = load_env_state(savfox_home).await?;
        state.values.insert(key.to_string(), value.to_string());
        save_env_state(savfox_home, &state).await?;
        "file"
    };
    Ok(json!({
        "key": key,
        "status": "saved",
        "set": true,
        "storage": storage,
    }))
}

pub(crate) async fn get_env_status(savfox_home: &Path, key: &str) -> Result<Value, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("missing env key".to_string());
    }
    let state = load_env_state(savfox_home).await?;
    let keyring = DefaultKeyringStore;
    Ok(json!({
        "key": key,
        "set": env_value_present(key, &state, &keyring),
    }))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn bins_discovers_from_manifests() {
        let tmp = tempdir().expect("tmpdir");
        let home = tmp.path().to_path_buf();

        // No skills-state.json needed — bins should succeed with empty results.
        let bins_ret = bins(&home).await.expect("bins");
        let bins_arr = bins_ret
            .get("bins")
            .and_then(|v| v.as_array())
            .map_or(0, Vec::len);
        assert_eq!(bins_arr, 0);
        // skills-state.json should NOT be created.
        assert!(!home.join("skills-state.json").is_file());
    }

    #[tokio::test]
    async fn env_roundtrip() {
        let tmp = tempdir().expect("tmpdir");
        let home = tmp.path().to_path_buf();

        let set_env_ret = set_env(&home, "DEMO_API_KEY", "secret")
            .await
            .expect("set env");
        assert_eq!(set_env_ret.get("set").and_then(|v| v.as_bool()), Some(true));

        let env_status = get_env_status(&home, "DEMO_API_KEY")
            .await
            .expect("get env");
        assert_eq!(env_status.get("set").and_then(|v| v.as_bool()), Some(true));
    }
}
