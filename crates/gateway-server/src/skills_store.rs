use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use savfox_keyring_store::{DefaultKeyringStore, KeyringStore};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::cached_db;

const SKILL_ENV_KEYRING_SERVICE: &str = "savfox.skills";
const SKILL_MANIFEST_FILE: &str = "SKILL.md";

const CATEGORY_WORKSPACE: &str = "workspace";
const CATEGORY_BUILTIN: &str = "built-in";
const CATEGORY_INSTALLED: &str = "installed";
const CATEGORY_EXTRA: &str = "extra";

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

async fn env_value_present(key: &str, pool: &SqlitePool, keyring: &dyn KeyringStore) -> bool {
    if std::env::var(key)
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
    {
        return true;
    }
    if keyring_value_present(key, keyring) {
        return true;
    }
    // Check skill_env table
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM skill_env WHERE key = ?1")
            .bind(key)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    row.is_some_and(|(v,)| !v.trim().is_empty())
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

    if components.len() >= 2 && components[0].contains('.') {
        let flock_depth = components.len().min(3);
        Some(components[..flock_depth].join("/"))
    } else {
        None
    }
}

/// Collect SKILL.md manifests under `root`, up to `max_depth` levels deep.
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
                    if skip_system_subtree && name == ".system" {
                        continue;
                    }
                }
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
    skill_dir: Option<PathBuf>,
    flock: Option<String>,
}

/// Load skill_roots from the DB.
async fn load_skill_roots(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_as::<_, (String,)>("SELECT path FROM skill_roots")
        .fetch_all(pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(p,)| p)
        .collect()
}

/// Get a skill's persisted enabled state from DB. Returns None if skill not found.
async fn get_persisted_enabled(pool: &SqlitePool, name: &str) -> Option<bool> {
    let row: Option<(bool,)> =
        sqlx::query_as("SELECT enabled FROM skills WHERE name = ?1")
            .bind(name)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    row.map(|(e,)| e)
}

fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Return summary counts derived purely from the cached skills table.
pub(crate) async fn status(savfox_home: &Path) -> Result<Value, String> {
    let pool = cached_db::get_pool(savfox_home).await?;

    let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM skills")
        .fetch_one(&pool)
        .await
        .map_err(|e| format!("count skills: {e}"))?;

    let installed: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM skills WHERE installed = 1")
            .fetch_one(&pool)
            .await
            .map_err(|e| format!("count installed: {e}"))?;

    // If DB is empty, do a full scan first
    if total.0 == 0 {
        let bins_val = bins(savfox_home, None).await?;
        let all = bins_val
            .get("bins")
            .and_then(|b| b.as_array())
            .map(Vec::len)
            .unwrap_or(0);
        let inst = bins_val
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
        return Ok(json!({
            "installed_count": inst,
            "available_count": all,
        }));
    }

    Ok(json!({
        "installed_count": installed.0,
        "available_count": total.0,
    }))
}

/// Discover all skills from manifest directories, upsert into SQLite, and
/// return a paginated, filterable response.
///
/// `params` may contain: `page` (1-indexed), `page_size`, `category`, `search`.
pub(crate) async fn bins(savfox_home: &Path, params: Option<&Value>) -> Result<Value, String> {
    let pool = cached_db::get_pool(savfox_home).await?;
    let keyring = DefaultKeyringStore;
    let allowlist = parse_allowlist();
    let current_os = std::env::consts::OS.to_ascii_lowercase();
    let skills_dir = savfox_home.join("skills");

    // Load skill_roots from DB.
    let skill_roots = load_skill_roots(&pool).await;
    let skip_roots: HashSet<PathBuf> = skill_roots.iter().map(PathBuf::from).collect();

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

    for root_path in &skill_roots {
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
            if !env_value_present(env_key, &pool, &keyring).await {
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
        let env_set = if let Some(ref key) = primary_env {
            Some(env_value_present(key, &pool, &keyring).await)
        } else {
            None
        };

        let is_new = get_persisted_enabled(&pool, &name).await.is_none();
        let persisted_enabled = get_persisted_enabled(&pool, &name).await.unwrap_or(true);

        let disabled_reason = if allowlist_blocked {
            Some("blocked by allowlist".to_string())
        } else if !persisted_enabled {
            Some("disabled by user".to_string())
        } else {
            None
        };
        let eligible = missing_deps.is_empty() && !allowlist_blocked;
        let enabled = eligible && persisted_enabled;

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

    // Auto-disable if >10 new skills at once.
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

    // Upsert all discovered skills into SQLite.
    let now = now_epoch_secs();
    let known_names: HashSet<&String> = rows.keys().collect();
    for (name, (_, row)) in &rows {
        let persisted_enabled = get_persisted_enabled(&pool, name)
            .await
            .unwrap_or(auto_enable_new);
        let missing_deps_json = serde_json::to_string(&row.missing_deps).unwrap_or_default();
        let path_str = row
            .skill_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        sqlx::query(
            r#"INSERT INTO skills (name, description, version, category, path, enabled, installed, eligible, flock, primary_env, env_set, allowlist_blocked, missing_deps, disabled_reason, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
               ON CONFLICT(name) DO UPDATE SET
                   description = excluded.description,
                   version = excluded.version,
                   category = excluded.category,
                   path = excluded.path,
                   enabled = skills.enabled,
                   installed = excluded.installed,
                   eligible = excluded.eligible,
                   flock = excluded.flock,
                   primary_env = excluded.primary_env,
                   env_set = excluded.env_set,
                   allowlist_blocked = excluded.allowlist_blocked,
                   missing_deps = excluded.missing_deps,
                   disabled_reason = excluded.disabled_reason,
                   updated_at = excluded.updated_at
            "#,
        )
        .bind(name)
        .bind(&row.description)
        .bind(&row.version)
        .bind(row.category)
        .bind(&path_str)
        .bind(persisted_enabled)
        .bind(row.installed)
        .bind(row.eligible)
        .bind(&row.flock)
        .bind(&row.primary_env)
        .bind(row.env_set)
        .bind(row.allowlist_blocked)
        .bind(&missing_deps_json)
        .bind(&row.disabled_reason)
        .bind(now)
        .execute(&pool)
        .await
        .map_err(|e| format!("upsert skill {name}: {e}"))?;
    }

    // Prune stale skills no longer on disk.
    let all_db_names: Vec<(String,)> = sqlx::query_as("SELECT name FROM skills")
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
    for (db_name,) in &all_db_names {
        if !known_names.contains(db_name) {
            let _ = sqlx::query("DELETE FROM skills WHERE name = ?1")
                .bind(db_name)
                .execute(&pool)
                .await;
        }
    }

    // Parse pagination / filter params.
    let page = params
        .and_then(|p| p.get("page"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .max(1) as usize;
    let page_size = params
        .and_then(|p| p.get("page_size"))
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .min(200) as usize;
    let category_filter = params
        .and_then(|p| p.get("category"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let search_filter = params
        .and_then(|p| p.get("search"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Build final bins from in-memory rows (already sorted by BTreeMap key).
    let all_bins: Vec<Value> = rows
        .into_values()
        .filter(|(_, row)| {
            if !category_filter.is_empty() && row.category != category_filter {
                return false;
            }
            if !search_filter.is_empty() {
                let q = search_filter.to_lowercase();
                let name_match = row.name.to_lowercase().contains(&q);
                let desc_match = row
                    .description
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&q);
                let cat_match = row.category.to_lowercase().contains(&q);
                if !name_match && !desc_match && !cat_match {
                    return false;
                }
            }
            true
        })
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

    let total = all_bins.len();
    let total_pages = if total == 0 { 1 } else { (total + page_size - 1) / page_size };
    let offset = (page - 1) * page_size;
    let bins: Vec<Value> = all_bins.into_iter().skip(offset).take(page_size).collect();

    Ok(json!({
        "bins": bins,
        "page": page,
        "page_size": page_size,
        "total": total,
        "total_pages": total_pages,
    }))
}

/// Toggle a skill's enabled state. Persisted to SQLite.
pub(crate) async fn set_enabled(
    savfox_home: &Path,
    name: &str,
    enabled: bool,
) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("missing skill name".to_string());
    }

    let pool = cached_db::get_pool(savfox_home).await?;

    // Upsert: if skill doesn't exist in DB yet, insert a minimal row.
    sqlx::query(
        r#"INSERT INTO skills (name, enabled, updated_at)
           VALUES (?1, ?2, ?3)
           ON CONFLICT(name) DO UPDATE SET enabled = excluded.enabled, updated_at = excluded.updated_at"#,
    )
    .bind(name)
    .bind(enabled)
    .bind(now_epoch_secs())
    .execute(&pool)
    .await
    .map_err(|e| format!("set enabled: {e}"))?;

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
    let pool = cached_db::get_pool(savfox_home).await?;
    let keyring = DefaultKeyringStore;
    let storage = if keyring.save(SKILL_ENV_KEYRING_SERVICE, key, value).is_ok() {
        // Remove from DB if saved to keyring.
        let _ = sqlx::query("DELETE FROM skill_env WHERE key = ?1")
            .bind(key)
            .execute(&pool)
            .await;
        "keyring"
    } else {
        sqlx::query(
            "INSERT INTO skill_env (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&pool)
        .await
        .map_err(|e| format!("save env: {e}"))?;
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
    let pool = cached_db::get_pool(savfox_home).await?;
    let keyring = DefaultKeyringStore;
    Ok(json!({
        "key": key,
        "set": env_value_present(key, &pool, &keyring).await,
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

        let bins_ret = bins(&home, None).await.expect("bins");
        let bins_arr = bins_ret
            .get("bins")
            .and_then(|v| v.as_array())
            .map_or(0, Vec::len);
        assert_eq!(bins_arr, 0);
        // Check pagination fields
        assert_eq!(bins_ret.get("page").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(bins_ret.get("total").and_then(|v| v.as_u64()), Some(0));
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
    async fn set_enabled_persists_to_db() {
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

        // Scan to populate DB
        let _ = bins(&home, None).await.expect("bins");

        // Verify skill is enabled by default
        let pool = cached_db::get_pool(&home).await.unwrap();
        let enabled = get_persisted_enabled(&pool, "test-skill").await;
        assert_eq!(enabled, Some(true));

        // Disable
        set_enabled(&home, "test-skill", false).await.unwrap();
        let enabled = get_persisted_enabled(&pool, "test-skill").await;
        assert_eq!(enabled, Some(false));

        // Re-enable
        set_enabled(&home, "test-skill", true).await.unwrap();
        let enabled = get_persisted_enabled(&pool, "test-skill").await;
        assert_eq!(enabled, Some(true));
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

        let result = bins(&home, None).await.expect("bins");
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
        let pool = cached_db::get_pool(&home).await.unwrap();
        for i in 0..11 {
            let key = format!("bulk-skill-{i}");
            let enabled = get_persisted_enabled(&pool, &key).await;
            assert_eq!(
                enabled,
                Some(false),
                "{key} should be disabled in DB"
            );
        }
    }
}
