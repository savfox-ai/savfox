use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use savfox_keyring_store::{DefaultKeyringStore, KeyringStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn default_true() -> bool {
    true
}

const SKILL_ENV_KEYRING_SERVICE: &str = "savfox.skills";
const SKILL_MANIFEST_FILE: &str = "SKILL.md";

const CATEGORY_WORKSPACE: &str = "workspace";
const CATEGORY_BUILTIN: &str = "built-in";
const CATEGORY_INSTALLED: &str = "installed";
const CATEGORY_EXTRA: &str = "extra";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InstalledSkill {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) source: Option<String>,
    #[serde(default)]
    pub(crate) version: Option<String>,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) disabled_reason: Option<String>,
    pub(crate) installed_at_ms: u64,
    pub(crate) updated_at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SkillsState {
    #[serde(default)]
    installed: Vec<InstalledSkill>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SkillsEnvState {
    #[serde(default)]
    values: HashMap<String, String>,
}

fn state_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("skills-state.json")
}

fn env_state_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("skills-env.json")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

async fn load_state(savfox_home: &Path) -> Result<SkillsState, String> {
    let path = state_path(savfox_home);
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(_) => return Ok(SkillsState::default()),
    };
    serde_json::from_str(&content).map_err(|err| format!("failed to parse skills state: {err}"))
}

async fn save_state(savfox_home: &Path, state: &SkillsState) -> Result<(), String> {
    let path = state_path(savfox_home);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("failed to create skills dir: {err}"))?;
    }
    let content = serde_json::to_string_pretty(state)
        .map_err(|err| format!("failed to serialize skills state: {err}"))?;
    tokio::fs::write(path, content)
        .await
        .map_err(|err| format!("failed to write skills state: {err}"))
}

async fn load_env_state(savfox_home: &Path) -> Result<SkillsEnvState, String> {
    let path = env_state_path(savfox_home);
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(_) => return Ok(SkillsEnvState::default()),
    };
    serde_json::from_str(&content).map_err(|err| format!("failed to parse skills env state: {err}"))
}

async fn save_env_state(savfox_home: &Path, state: &SkillsEnvState) -> Result<(), String> {
    let path = env_state_path(savfox_home);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("failed to create skills env dir: {err}"))?;
    }
    let content = serde_json::to_string_pretty(state)
        .map_err(|err| format!("failed to serialize skills env state: {err}"))?;
    tokio::fs::write(path, content)
        .await
        .map_err(|err| format!("failed to write skills env state: {err}"))
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
                if skip_system_subtree
                    && path
                        .file_name()
                        .and_then(|v| v.to_str())
                        .is_some_and(|name| name == ".system")
                {
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
}

pub(crate) async fn status(savfox_home: &Path) -> Result<Value, String> {
    let state = load_state(savfox_home).await?;
    let installed_count = state.installed.len();
    let available_count = bins(savfox_home)
        .await
        .ok()
        .and_then(|v| v.get("bins").and_then(|b| b.as_array()).map(Vec::len))
        .unwrap_or(installed_count);
    Ok(json!({
        "installed": state.installed,
        "installed_count": installed_count,
        "available_count": available_count,
    }))
}

pub(crate) async fn bins(savfox_home: &Path) -> Result<Value, String> {
    let state = load_state(savfox_home).await?;
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
        .map(|(path, category)| (path, category, false)),
    );
    discovered.extend(
        collect_skill_manifests(&savfox_home.join("skills"), CATEGORY_INSTALLED, 4, true)
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
        let disabled_reason = if allowlist_blocked {
            Some("blocked by allowlist".to_string())
        } else {
            None
        };

        let row = SkillBinRow {
            name: name.clone(),
            version: manifest.version,
            installed: installed_by_location,
            enabled: true,
            category,
            description: if manifest.description.trim().is_empty() {
                None
            } else {
                Some(manifest.description)
            },
            eligible: missing_deps.is_empty() && !allowlist_blocked,
            missing_deps,
            primary_env,
            env_set,
            disabled_reason,
            allowlist_blocked,
        };

        let rank = category_rank(category);
        match rows.get(&name) {
            Some((current_rank, _)) if *current_rank <= rank => {}
            _ => {
                rows.insert(name, (rank, row));
            }
        }
    }

    for skill in state.installed {
        let entry = rows.entry(skill.name.clone()).or_insert_with(|| {
            (
                category_rank(CATEGORY_INSTALLED),
                SkillBinRow {
                    name: skill.name.clone(),
                    version: skill.version.clone(),
                    installed: true,
                    enabled: skill.enabled,
                    category: CATEGORY_INSTALLED,
                    description: None,
                    eligible: true,
                    missing_deps: Vec::new(),
                    primary_env: None,
                    env_set: None,
                    disabled_reason: skill.disabled_reason.clone(),
                    allowlist_blocked: false,
                },
            )
        });

        let (_, row) = entry;
        row.installed = true;
        row.enabled = skill.enabled;
        if row.category != CATEGORY_WORKSPACE {
            row.category = CATEGORY_INSTALLED;
        }
        if row.version.is_none() {
            row.version = skill.version.clone();
        }
        row.disabled_reason = if !skill.enabled {
            skill
                .disabled_reason
                .clone()
                .or(Some("disabled by user".to_string()))
        } else if row.allowlist_blocked {
            Some("blocked by allowlist".to_string())
        } else {
            None
        };
        row.eligible = row.missing_deps.is_empty() && !row.allowlist_blocked;
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

pub(crate) async fn install(
    savfox_home: &Path,
    name: &str,
    source: Option<&str>,
) -> Result<Value, String> {
    let mut state = load_state(savfox_home).await?;
    let now = now_ms();
    if let Some(existing) = state.installed.iter_mut().find(|s| s.name == name) {
        existing.updated_at_ms = now;
        existing.enabled = true;
        existing.disabled_reason = None;
        if let Some(src) = source {
            existing.source = Some(src.to_owned());
        }
    } else {
        state.installed.push(InstalledSkill {
            name: name.to_owned(),
            source: source.map(ToOwned::to_owned),
            version: None,
            enabled: true,
            disabled_reason: None,
            installed_at_ms: now,
            updated_at_ms: now,
        });
    }
    save_state(savfox_home, &state).await?;
    Ok(json!({ "name": name, "status": "installed" }))
}

pub(crate) async fn update(
    savfox_home: &Path,
    name: Option<&str>,
    enabled: Option<bool>,
    disabled_reason: Option<&str>,
) -> Result<Value, String> {
    let mut state = load_state(savfox_home).await?;
    let now = now_ms();
    let mut updated = Vec::new();
    if let Some(target) = name.filter(|v| !v.is_empty()) {
        if let Some(existing) = state.installed.iter_mut().find(|s| s.name == target) {
            existing.updated_at_ms = now;
            if let Some(enabled_flag) = enabled {
                existing.enabled = enabled_flag;
                if enabled_flag {
                    existing.disabled_reason = None;
                } else {
                    existing.disabled_reason = Some(
                        disabled_reason
                            .filter(|v| !v.trim().is_empty())
                            .unwrap_or("disabled by user")
                            .to_string(),
                    );
                }
            }
            updated.push(target.to_owned());
        } else {
            return Err(format!("skill not installed: {target}"));
        }
    } else if enabled.is_some() {
        return Err("missing 'name' parameter when setting enabled state".to_string());
    } else {
        for item in &mut state.installed {
            item.updated_at_ms = now;
            updated.push(item.name.clone());
        }
    }
    save_state(savfox_home, &state).await?;
    Ok(json!({
        "name": name.unwrap_or("all"),
        "status": "updated",
        "updated": updated,
        "enabled": enabled,
        "disabled_reason": disabled_reason,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn skills_roundtrip() {
        let home =
            std::env::temp_dir().join(format!("savfox-skills-test-{}", uuid::Uuid::now_v7()));
        let install_ret = install(&home, "demo", Some("local"))
            .await
            .expect("install");
        assert_eq!(
            install_ret.get("status").and_then(|v| v.as_str()),
            Some("installed")
        );

        let status_ret = status(&home).await.expect("status");
        let installed_len = status_ret
            .get("installed")
            .and_then(|v| v.as_array())
            .map_or(0, Vec::len);
        assert_eq!(installed_len, 1);

        let update_ret = update(&home, Some("demo"), Some(false), None)
            .await
            .expect("update");
        assert_eq!(
            update_ret.get("status").and_then(|v| v.as_str()),
            Some("updated")
        );
        assert_eq!(
            update_ret.get("enabled").and_then(|v| v.as_bool()),
            Some(false)
        );

        let set_env_ret = set_env(&home, "DEMO_API_KEY", "secret")
            .await
            .expect("set env");
        assert_eq!(set_env_ret.get("set").and_then(|v| v.as_bool()), Some(true));

        let env_status = get_env_status(&home, "DEMO_API_KEY")
            .await
            .expect("get env");
        assert_eq!(env_status.get("set").and_then(|v| v.as_bool()), Some(true));

        let bins_ret = bins(&home).await.expect("bins");
        let bins_len = bins_ret
            .get("bins")
            .and_then(|v| v.as_array())
            .map_or(0, Vec::len);
        assert_eq!(bins_len, 1);

        let _ = tokio::fs::remove_dir_all(home).await;
    }
}
