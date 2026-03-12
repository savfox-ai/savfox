use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::config::RegistryConfig;

/// Information about a realm in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealmInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Information about a flock (skill collection) in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlockInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// A single skill entry within a flock's skills.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub git: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
}

/// Index of skills within a flock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsIndex {
    #[serde(default)]
    pub items: Vec<SkillEntry>,
}

/// A search result returned by `GitRegistry::search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySearchResult {
    pub realm: String,
    pub flock: String,
    pub entry: SkillEntry,
}

/// Git-based skill registry that clones a remote repository locally.
///
/// The registry is stored at `{savfox_home}/registry/{domain}/{restpath}/`.
#[derive(Debug)]
pub struct GitRegistry {
    savfox_home: PathBuf,
    config: RegistryConfig,
}

impl GitRegistry {
    pub fn new(savfox_home: PathBuf, config: RegistryConfig) -> Self {
        Self { savfox_home, config }
    }

    /// Path to the local clone of this registry, derived from the git URL.
    ///
    /// e.g. `https://github.com/savfox-ai/registry.git` → `registry/github.com/savfox-ai/registry`
    pub fn registry_path(&self) -> PathBuf {
        let dir_name = Self::dir_from_url(&self.config.git);
        self.savfox_home.join("registry").join(dir_name)
    }

    /// Derive `{domain}/{restpath}` from a git URL.
    fn dir_from_url(git_url: &str) -> String {
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

    /// Clone the registry if it doesn't already exist locally.
    pub async fn ensure_cloned(&self) -> anyhow::Result<()> {
        let repo_path = self.registry_path();
        if repo_path.is_dir() {
            return Ok(());
        }

        let git_url = self.config.git.clone();
        let target = repo_path.clone();
        info!(url = %git_url, path = %target.display(), "cloning skill registry");

        tokio::task::spawn_blocking(move || {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let output = std::process::Command::new("git")
                .args([
                    "clone",
                    "--depth",
                    "1",
                    &git_url,
                    &target.to_string_lossy(),
                ])
                .output()?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git clone failed: {}", stderr.trim());
            }
            Ok(())
        })
        .await?
    }

    /// Pull latest changes from the remote.
    pub async fn sync(&self) -> anyhow::Result<()> {
        self.ensure_cloned().await?;

        let repo_path = self.registry_path();
        info!(path = %repo_path.display(), "syncing skill registry");

        tokio::task::spawn_blocking(move || {
            let output = std::process::Command::new("git")
                .args(["pull", "--ff-only"])
                .current_dir(&repo_path)
                .output()?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(error = %stderr.trim(), "git pull failed, registry may be stale");
            }
            Ok(())
        })
        .await?
    }

    /// Search the registry for skills matching the given query.
    ///
    /// Walks `realms/*/flocks/*/skills.json` and matches against name,
    /// summary, keywords, and categories.
    pub async fn search(&self, query: &str) -> anyhow::Result<Vec<RegistrySearchResult>> {
        self.ensure_cloned().await?;

        let repo_path = self.registry_path();
        let query = query.to_string();

        tokio::task::spawn_blocking(move || search_sync(&repo_path, &query)).await?
    }

    /// List all realm names in the registry.
    pub async fn list_realms(&self) -> anyhow::Result<Vec<String>> {
        self.ensure_cloned().await?;
        let realms_dir = self.registry_path().join("realms");

        tokio::task::spawn_blocking(move || {
            let mut realms = Vec::new();
            if !realms_dir.is_dir() {
                return Ok(realms);
            }
            for entry in std::fs::read_dir(&realms_dir)?.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        realms.push(name.to_string());
                    }
                }
            }
            realms.sort();
            Ok(realms)
        })
        .await?
    }

    /// List all flock names within a realm.
    pub async fn list_flocks(&self, realm: &str) -> anyhow::Result<Vec<String>> {
        self.ensure_cloned().await?;
        let flocks_dir = self.registry_path().join("realms").join(realm).join("flocks");

        tokio::task::spawn_blocking(move || {
            let mut flocks = Vec::new();
            if !flocks_dir.is_dir() {
                return Ok(flocks);
            }
            for entry in std::fs::read_dir(&flocks_dir)?.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        flocks.push(name.to_string());
                    }
                }
            }
            flocks.sort();
            Ok(flocks)
        })
        .await?
    }

    /// List all skills within a specific realm and flock.
    pub async fn list_skills(
        &self,
        realm: &str,
        flock: &str,
    ) -> anyhow::Result<Vec<SkillEntry>> {
        self.ensure_cloned().await?;
        let skills_file = self
            .registry_path()
            .join("realms")
            .join(realm)
            .join("flocks")
            .join(flock)
            .join("skills.json");

        tokio::task::spawn_blocking(move || {
            if !skills_file.is_file() {
                return Ok(Vec::new());
            }
            let content = std::fs::read_to_string(&skills_file)?;
            let index: SkillsIndex = serde_json::from_str(&content)?;
            Ok(index.items)
        })
        .await?
    }
}

fn search_sync(repo_path: &Path, query: &str) -> anyhow::Result<Vec<RegistrySearchResult>> {
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    let realms_dir = repo_path.join("realms");
    if !realms_dir.is_dir() {
        return Ok(results);
    }

    for realm_entry in std::fs::read_dir(&realms_dir)?.flatten() {
        if !realm_entry.path().is_dir() {
            continue;
        }
        let realm_name = realm_entry
            .file_name()
            .to_string_lossy()
            .to_string();

        let flocks_dir = realm_entry.path().join("flocks");
        if !flocks_dir.is_dir() {
            continue;
        }

        for flock_entry in std::fs::read_dir(&flocks_dir)?.flatten() {
            if !flock_entry.path().is_dir() {
                continue;
            }
            let flock_name = flock_entry
                .file_name()
                .to_string_lossy()
                .to_string();

            let skills_file = flock_entry.path().join("skills.json");
            if !skills_file.is_file() {
                continue;
            }

            let content = match std::fs::read_to_string(&skills_file) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let index: SkillsIndex = match serde_json::from_str(&content) {
                Ok(idx) => idx,
                Err(_) => continue,
            };

            for entry in index.items {
                if matches_query(&entry, &query_lower) {
                    results.push(RegistrySearchResult {
                        realm: realm_name.clone(),
                        flock: flock_name.clone(),
                        entry,
                    });
                }
            }
        }
    }

    Ok(results)
}

fn matches_query(entry: &SkillEntry, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }

    if entry.name.to_lowercase().contains(query_lower) {
        return true;
    }
    if let Some(ref summary) = entry.summary {
        if summary.to_lowercase().contains(query_lower) {
            return true;
        }
    }
    if let Some(ref desc) = entry.description {
        if desc.to_lowercase().contains(query_lower) {
            return true;
        }
    }
    if entry
        .keywords
        .iter()
        .any(|k| k.to_lowercase().contains(query_lower))
    {
        return true;
    }
    if entry
        .categories
        .iter()
        .any(|c| c.to_lowercase().contains(query_lower))
    {
        return true;
    }
    false
}
