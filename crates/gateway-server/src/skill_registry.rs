//! Skill registry  - discover, search, and install skills from a remote registry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::info;

/// Default registry URL
pub const DEFAULT_REGISTRY_URL: &str = "https://registry.savfox.ai/v1";

/// Skill manifest from registry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub min_version: Option<String>,
}

/// Installed skill info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkill {
    pub manifest: SkillManifest,
    pub installed_at: u64,
    pub install_path: PathBuf,
    pub update_available: Option<String>,
}

/// Skill registry client
#[derive(Debug)]
pub struct SkillRegistry {
    registry_url: String,
    installed_path: PathBuf,
    installed: RwLock<HashMap<String, InstalledSkill>>,
}

impl SkillRegistry {
    pub fn new(savfox_home: &Path, registry_url: Option<String>) -> Self {
        let installed_path = savfox_home.join("skills").join("registry.json");
        Self {
            registry_url: registry_url.unwrap_or_else(|| DEFAULT_REGISTRY_URL.to_string()),
            installed_path,
            installed: RwLock::new(HashMap::new()),
        }
    }

    /// Load installed skills index from disk
    pub async fn load(&self) {
        if let Ok(content) = tokio::fs::read_to_string(&self.installed_path).await {
            if let Ok(installed) = serde_json::from_str::<HashMap<String, InstalledSkill>>(&content)
            {
                *self.installed.write().await = installed;
            }
        }
    }

    async fn save(&self) -> Result<(), String> {
        let installed = self.installed.read().await;
        let json = serde_json::to_string_pretty(&*installed)
            .map_err(|e| format!("serialize error: {e}"))?;
        if let Some(parent) = self.installed_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        tokio::fs::write(&self.installed_path, json)
            .await
            .map_err(|e| format!("write error: {e}"))
    }

    /// Search registry for skills
    pub async fn search(&self, query: &str) -> Result<Vec<SkillManifest>, String> {
        // In a real implementation, this would HTTP GET the registry
        // For now, return empty  - the actual HTTP call would be:
        // GET {registry_url}/skills?q={query}
        info!(query = %query, registry = %self.registry_url, "searching skill registry");
        Ok(Vec::new())
    }

    /// Get skill details from registry
    pub async fn get_manifest(&self, skill_id: &str) -> Result<SkillManifest, String> {
        // GET {registry_url}/skills/{skill_id}
        Err(format!("Skill not found in registry: {skill_id}"))
    }

    /// List installed skills
    pub async fn list_installed(&self) -> Vec<InstalledSkill> {
        self.installed.read().await.values().cloned().collect()
    }

    /// Check if a skill is installed
    pub async fn is_installed(&self, skill_id: &str) -> bool {
        self.installed.read().await.contains_key(skill_id)
    }

    /// Record a skill installation
    pub async fn record_install(
        &self,
        manifest: SkillManifest,
        install_path: PathBuf,
    ) -> Result<(), String> {
        let id = manifest.id.clone();
        let installed = InstalledSkill {
            manifest,
            installed_at: now_secs(),
            install_path,
            update_available: None,
        };
        self.installed.write().await.insert(id.clone(), installed);
        self.save().await?;
        info!(skill_id = %id, "skill installation recorded");
        Ok(())
    }

    /// Remove a skill installation record
    pub async fn record_uninstall(&self, skill_id: &str) -> Result<(), String> {
        self.installed
            .write()
            .await
            .remove(skill_id)
            .ok_or_else(|| format!("Skill not installed: {skill_id}"))?;
        self.save().await?;
        info!(skill_id = %skill_id, "skill uninstalled");
        Ok(())
    }

    /// Check for updates to installed skills
    pub async fn check_updates(&self) -> Vec<(String, String, String)> {
        // In real impl: query registry for each installed skill, compare versions
        // Returns: Vec<(skill_id, current_version, available_version)>
        Vec::new()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
