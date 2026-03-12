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

// ── Skills state (persisted to skills-state.json) ───────────────────────────

/// Per-skill persisted state — parsed from SKILL.md and saved here.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillState {
    /// Skill name (from SKILL.md frontmatter).
    name: String,

    /// Human-readable description (from SKILL.md frontmatter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,

    /// Version string (from SKILL.md frontmatter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,

    /// Category (workspace, built-in, installed, extra).
    #[serde(default)]
    category: String,

    /// Filesystem path to the folder containing SKILL.md.
    #[serde(default)]
    path: String,

    /// Whether the skill is enabled by the user.
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for SkillState {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: None,
            version: None,
            category: String::new(),
            path: String::new(),
            enabled: true,
        }
    }
}

/// Top-level persisted skills state file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SkillsState {
    /// Additional root directories to scan for skills. Each entry is an
    /// absolute path. These directories themselves are **not** treated as
    /// skills — only their children are scanned.
    #[serde(default)]
    skill_roots: Vec<String>,

    /// Per-skill enabled/disabled state keyed by skill name.
    #[serde(default)]
    skills: HashMap<String, SkillState>,
}

fn skills_state_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("skills-state.json")
}

async fn load_skills_state(savfox_home: &Path) -> Result<SkillsState, String> {
    json_store::load_json(&skills_state_path(savfox_home), "skills state").await
}

async fn save_skills_state(savfox_home: &Path, state: &SkillsState) -> Result<(), String> {
    json_store::save_json(&skills_state_path(savfox_home), state, "skills state").await
}

// ── Env state ───────────────────────────────────────────────────────────────

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

// ── Helpers ─────────────────────────────────────────────────────────────────

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

/// Derive a flock grouping label from a skill directory.
///
/// Given a skill at `{skills_dir}/github.com/org/repo/SKILL.md`, the flock is
/// `github.com/org/repo`.  If the first path component contains a dot (looks
/// like a domain), we take up to 3 components (domain/org/repo) as the flock.
/// For flat installs like `{skills_dir}/my-skill/SKILL.md`, returns `None`.
fn derive_flock(skill_dir: Option<&Path>, skills_dir: &Path) -> Option<String> {
    let skill_dir = skill_dir?;
    let rel = skill_dir.strip_prefix(skills_dir).ok()?;
    let components: Vec<&str> = rel
        .components()
        .filter_map(|c| {
            if let std::path::Component::Normal(s) = c {
                s.to_str()
            } else {
                None
            }
        })
        .collect();

    // Need at least 2 components (domain/org) and the first must look like a
    // domain (contains a dot) to qualify as a flock.
    if components.len() >= 2 && components[0].contains('.') {
        // Take up to 3 components: domain/org/repo
        let flock_depth = components.len().min(3);
        Some(components[..flock_depth].join("/"))
    } else {
        None
    }
}

/// Collect SKILL.md manifests under `root`, up to `max_depth` levels deep.
///
/// When `skip_roots` is provided, directories whose names match any entry in
/// the set are skipped (not treated as skills or descended into). This is used
/// to exclude `skill_roots` directories themselves from being treated as skills
/// when they live inside the skills folder.
fn collect_skill_manifests(
    root: &Path,
    category: &str,
    max_depth: usize,
    skip_system_subtree: bool,
    skip_roots: &HashSet<PathBuf>,
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
                    // Skip the .system directory at the skills root level.
                    if skip_system_subtree && name == ".system" {
                        continue;
                    }
                }
                // Skip directories that are skill_roots (they are scan roots,
                // not skills themselves).
                if skip_roots.contains(&path) {
                    continue;
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
    /// Grouping label for skills that share the same source repository or
    /// zip archive.  e.g. `"github.com/org/repo"`.  Empty for built-in /
    /// workspace skills.
    flock: Option<String>,
}

// ── Public API ──────────────────────────────────────────────────────────────

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

/// Discover all skills from manifest directories and persist their state to
/// `skills-state.json`.
///
/// "installed" is determined solely by the manifest's directory location:
/// skills under `$SAVFOX_HOME/skills/` (non-.system) are installed.
/// Skills under `.system/` are built-in.  Workspace & extra are discovered
/// from their respective directories.
///
/// Newly discovered skills are enabled by default unless more than 10 new
/// skills appear in a single scan — in that case they start disabled.
pub(crate) async fn bins(savfox_home: &Path) -> Result<Value, String> {
    let env_state = load_env_state(savfox_home).await?;
    let mut skills_state = load_skills_state(savfox_home).await?;
    let keyring = DefaultKeyringStore;
    let allowlist = parse_allowlist();
    let current_os = std::env::consts::OS.to_ascii_lowercase();
    let skills_dir = savfox_home.join("skills");

    // Build set of skill_roots paths so we can skip them during scanning.
    let skip_roots: HashSet<PathBuf> = skills_state
        .skill_roots
        .iter()
        .map(PathBuf::from)
        .collect();

    let mut discovered: Vec<(PathBuf, &'static str, bool)> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        discovered.extend(
            collect_skill_manifests(
                &cwd.join(".savfox").join("skills"),
                CATEGORY_WORKSPACE,
                4,
                false,
                &skip_roots,
            )
            .into_iter()
            .map(|(path, category)| (path, category, false)),
        );
    }
    discovered.extend(
        collect_skill_manifests(
            &skills_dir.join(".system"),
            CATEGORY_BUILTIN,
            4,
            false,
            &skip_roots,
        )
        .into_iter()
        .map(|(path, category)| (path, category, true)),
    );
    discovered.extend(
        collect_skill_manifests(
            &skills_dir,
            CATEGORY_INSTALLED,
            6,
            true,
            &skip_roots,
        )
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
                &skip_roots,
            )
            .into_iter()
            .map(|(path, category)| (path, category, false)),
        );
    }

    // Also scan each skill_root as an installed category.
    for root_path in &skills_state.skill_roots {
        let root = PathBuf::from(root_path);
        discovered.extend(
            collect_skill_manifests(&root, CATEGORY_INSTALLED, 6, false, &skip_roots)
                .into_iter()
                .map(|(path, category)| (path, category, true)),
        );
    }

    let mut rows: BTreeMap<String, (u8, SkillBinRow)> = BTreeMap::new();
    let mut new_skill_names: HashSet<String> = HashSet::new();
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

        // Look up persisted enabled state. For new skills use true as a
        // tentative default; this may be revised below if too many new
        // skills are discovered at once.
        let is_new = !skills_state.skills.contains_key(&name);
        let persisted_enabled = skills_state
            .skills
            .get(&name)
            .map(|s| s.enabled)
            .unwrap_or(true);

        let disabled_reason = if allowlist_blocked {
            Some("blocked by allowlist".to_string())
        } else if !persisted_enabled {
            Some("disabled by user".to_string())
        } else {
            None
        };
        let eligible = missing_deps.is_empty() && !allowlist_blocked;
        let enabled = eligible && persisted_enabled;

        // Compute flock grouping for installed skills.
        // For a skill at `skills/github.com/org/repo/SKILL.md` the flock is
        // `github.com/org/repo`.  For `skills/my-skill/SKILL.md` there is no
        // flock (flat install).
        let flock = if category == CATEGORY_INSTALLED {
            derive_flock(manifest_path.parent(), &skills_dir)
        } else {
            None
        };

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
            flock,
        };

        if is_new {
            new_skill_names.insert(name.clone());
        }
        let rank = category_rank(category);
        match rows.get(&name) {
            Some((current_rank, _)) if *current_rank <= rank => {}
            _ => {
                rows.insert(name, (rank, row));
            }
        }
    }

    // When more than 10 new skills appear at once (e.g. bulk git clone or
    // zip install), disable them by default so they don't overwhelm the
    // session. The user can enable them individually afterwards.
    let auto_enable_new = new_skill_names.len() <= 10;
    if !auto_enable_new {
        for name in &new_skill_names {
            if let Some((_, row)) = rows.get_mut(name) {
                row.enabled = false;
                if row.disabled_reason.is_none() {
                    row.disabled_reason =
                        Some("auto-disabled: too many new skills at once".to_string());
                }
            }
        }
    }

    // Persist all discovered skills to skills-state.json.
    // Existing skills keep their persisted enabled state but update
    // name/description/version/category/path from the freshly parsed SKILL.md.
    // New skills are enabled by default unless auto_enable_new is false.
    let known_names: HashSet<&String> = rows.keys().collect();
    let prev_len = skills_state.skills.len();
    for (name, (_, row)) in &rows {
        let persisted_enabled = skills_state
            .skills
            .get(name)
            .map(|s| s.enabled)
            .unwrap_or(auto_enable_new);
        skills_state.skills.insert(
            name.clone(),
            SkillState {
                name: row.name.clone(),
                description: row.description.clone(),
                version: row.version.clone(),
                category: row.category.to_string(),
                path: row
                    .skill_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                enabled: persisted_enabled,
            },
        );
    }
    // Remove skills from state that are no longer discovered.
    skills_state
        .skills
        .retain(|name, _| known_names.contains(name));
    // Always save — we update metadata on every scan.
    if !rows.is_empty() || prev_len != skills_state.skills.len() {
        let _ = save_skills_state(savfox_home, &skills_state).await;
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
                "flock": row.flock,
                "command": format!("savfox-skill-{}", row.name),
            })
        })
        .collect();

    Ok(json!({ "bins": bins }))
}

/// Toggle a skill's enabled state. Persisted to `skills-state.json`.
pub(crate) async fn set_enabled(
    savfox_home: &Path,
    name: &str,
    enabled: bool,
) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("missing skill name".to_string());
    }

    let mut state = load_skills_state(savfox_home).await?;
    state
        .skills
        .entry(name.to_string())
        .or_insert_with(|| SkillState {
            name: name.to_string(),
            ..Default::default()
        })
        .enabled = enabled;
    save_skills_state(savfox_home, &state).await?;

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
    let empty_skip = HashSet::new();

    let dirs_to_scan: Vec<(&Path, bool)> = vec![(&system_dir, false), (&skills_dir, true)];

    for (root, skip_system) in dirs_to_scan {
        for (manifest_path, _) in
            collect_skill_manifests(root, CATEGORY_INSTALLED, 6, skip_system, &empty_skip)
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
        for (manifest_path, _) in
            collect_skill_manifests(&ws_dir, CATEGORY_WORKSPACE, 4, false, &empty_skip)
        {
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

        let bins_ret = bins(&home).await.expect("bins");
        let bins_arr = bins_ret
            .get("bins")
            .and_then(|v| v.as_array())
            .map_or(0, Vec::len);
        assert_eq!(bins_arr, 0);
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

    #[tokio::test]
    async fn set_enabled_persists_to_state() {
        let tmp = tempdir().expect("tmpdir");
        let home = tmp.path().to_path_buf();

        // Create a skill manifest
        let skill_dir = home.join("skills").join("test-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: test-skill\n---\nTest",
        )
        .unwrap();

        // Scan to populate state
        let _ = bins(&home).await.expect("bins");

        // Verify skill is enabled by default in state file
        let state = load_skills_state(&home).await.unwrap();
        assert_eq!(state.skills.get("test-skill").unwrap().enabled, true);

        // Disable
        set_enabled(&home, "test-skill", false).await.unwrap();
        let state = load_skills_state(&home).await.unwrap();
        assert_eq!(state.skills.get("test-skill").unwrap().enabled, false);

        // Re-enable
        set_enabled(&home, "test-skill", true).await.unwrap();
        let state = load_skills_state(&home).await.unwrap();
        assert_eq!(state.skills.get("test-skill").unwrap().enabled, true);
    }

    #[tokio::test]
    async fn bulk_new_skills_auto_disabled() {
        let tmp = tempdir().expect("tmpdir");
        let home = tmp.path().to_path_buf();

        // Create 11 skill manifests (exceeds the threshold of 10).
        for i in 0..11 {
            let skill_dir = home.join("skills").join(format!("bulk-skill-{i}"));
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: bulk-skill-{i}\n---\nBulk test"),
            )
            .unwrap();
        }

        let result = bins(&home).await.expect("bins");
        let bins_arr = result.get("bins").and_then(|v| v.as_array()).unwrap();

        // All 11 new skills should be disabled.
        for bin in bins_arr {
            let name = bin.get("name").and_then(|v| v.as_str()).unwrap();
            if name.starts_with("bulk-skill-") {
                assert_eq!(
                    bin.get("enabled").and_then(|v| v.as_bool()),
                    Some(false),
                    "skill {name} should be auto-disabled"
                );
            }
        }

        // Persisted state should also reflect disabled.
        let state = load_skills_state(&home).await.unwrap();
        for i in 0..11 {
            let key = format!("bulk-skill-{i}");
            assert_eq!(
                state.skills.get(&key).unwrap().enabled,
                false,
                "{key} should be disabled in state"
            );
        }
    }
}
