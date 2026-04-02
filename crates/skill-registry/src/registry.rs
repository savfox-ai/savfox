use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::sync::RwLock;

use crate::config::RegistryConfig;
use crate::git_registry::{GitRegistry, RegistrySearchResult};
use crate::installer::{InstallProgress, InstallResult, SkillInstaller};
use crate::package::{SkillPackage, SkillSource, SkillSourceType};

#[derive(Debug)]
pub struct SkillRegistry {
    skills_dir: PathBuf,
    git_registry: GitRegistry,
    installer: SkillInstaller,
    installed_cache: RwLock<HashMap<String, SkillPackage>>,
}

impl SkillRegistry {
    #[must_use] 
    pub fn new(savfox_home: &Path) -> Self {
        Self::with_config(savfox_home, RegistryConfig::default())
    }

    #[must_use] 
    pub fn with_config(savfox_home: &Path, config: RegistryConfig) -> Self {
        let skills_dir = savfox_home.join("skills");
        Self {
            skills_dir: skills_dir.clone(),
            git_registry: GitRegistry::new(savfox_home.to_path_buf(), config),
            installer: SkillInstaller::new(skills_dir),
            installed_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Sync the git registry (pull latest changes).
    pub async fn sync_registry(&self) -> anyhow::Result<()> {
        self.git_registry.sync().await
    }

    /// Search for skills in the git registry.
    pub async fn search(&self, query: &str) -> anyhow::Result<Vec<SkillPackage>> {
        let results = self.git_registry.search(query).await?;
        let installed = self.installed_cache.read().await;

        let packages = results
            .into_iter()
            .map(|r| {
                let mut pkg = search_result_to_package(&r);
                if let Some(installed_pkg) = installed.get(&pkg.manifest.name) {
                    pkg.installed = true;
                    pkg.installed_version = installed_pkg.manifest.version.clone().into();
                    pkg.install_path = installed_pkg.install_path.clone();
                }
                pkg
            })
            .collect();

        Ok(packages)
    }

    /// List all available skills from the git registry.
    pub async fn list_available(&self, _force_refresh: bool) -> anyhow::Result<Vec<SkillPackage>> {
        // Search with empty query returns all skills.
        self.search("").await
    }

    pub async fn list_installed(&self) -> anyhow::Result<Vec<SkillPackage>> {
        let installed_list = self.installer.list_installed().await?;
        let mut packages = Vec::new();
        let mut cache = self.installed_cache.write().await;

        for (name, version) in installed_list {
            let install_path = self.skills_dir.join(&name);
            let manifest_path = install_path.join(".savfox-manifest.json");

            let manifest = if manifest_path.exists() {
                if let Ok(content) = tokio::fs::read_to_string(&manifest_path).await {
                    serde_json::from_str(&content).ok()
                } else {
                    None
                }
            } else {
                None
            };

            let pkg = SkillPackage {
                manifest: manifest.unwrap_or_else(|| crate::package::SkillManifest {
                    name: name.clone(),
                    version: semver::Version::parse(&version)
                        .unwrap_or(semver::Version::new(0, 0, 0)),
                    description: String::new(),
                    ..Default::default()
                }),
                source: SkillSource {
                    source_type: SkillSourceType::Local,
                    url: None,
                    path: Some(install_path.clone()),
                    registry: None,
                    checksum: None,
                    subdir: None,
                },
                installed: true,
                installed_version: semver::Version::parse(&version).ok(),
                install_path: Some(install_path.clone()),
            };

            cache.insert(name, pkg.clone());
            packages.push(pkg);
        }

        Ok(packages)
    }

    pub async fn get(&self, name: &str) -> Option<SkillPackage> {
        let cache = self.installed_cache.read().await;
        cache.get(name).cloned()
    }

    pub async fn install(
        &self,
        package: &SkillPackage,
        progress_tx: Option<tokio::sync::mpsc::Sender<InstallProgress>>,
    ) -> anyhow::Result<InstallResult> {
        let result = self.installer.install(package, progress_tx).await?;

        if result.success {
            let mut cache = self.installed_cache.write().await;
            let mut installed_pkg = package.clone();
            installed_pkg.installed = true;
            installed_pkg.installed_version = Some(package.manifest.version.clone());
            installed_pkg.install_path = Some(result.install_path.clone());
            cache.insert(package.manifest.name.clone(), installed_pkg);
        }

        Ok(result)
    }

    pub async fn uninstall(&self, name: &str) -> anyhow::Result<bool> {
        let result = self.installer.uninstall(name).await?;

        if result {
            let mut cache = self.installed_cache.write().await;
            cache.remove(name);
        }

        Ok(result)
    }

    pub async fn is_installed(&self, name: &str) -> bool {
        self.installer.is_installed(name).await
    }

    pub async fn refresh_installed(&self) -> anyhow::Result<()> {
        self.list_installed().await?;
        Ok(())
    }

    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }
}

/// Convert a git registry search result into a `SkillPackage`.
fn search_result_to_package(result: &RegistrySearchResult) -> SkillPackage {
    let entry = &result.entry;
    let version = entry
        .version
        .as_deref()
        .and_then(|v| semver::Version::parse(v).ok())
        .unwrap_or(semver::Version::new(0, 0, 0));

    SkillPackage {
        manifest: crate::package::SkillManifest {
            name: entry.name.clone(),
            version,
            description: entry
                .description
                .clone()
                .or_else(|| entry.summary.clone())
                .unwrap_or_default(),
            short_description: entry.summary.clone(),
            author: entry.author.clone(),
            keywords: entry.keywords.clone(),
            categories: entry.categories.clone(),
            repository: entry.git.clone(),
            ..Default::default()
        },
        source: SkillSource {
            source_type: SkillSourceType::Registry,
            url: entry.git.clone(),
            path: None,
            registry: Some(format!("{}/{}", result.realm, result.flock)),
            checksum: None,
            subdir: None,
        },
        installed: false,
        installed_version: None,
        install_path: None,
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn registry_creation() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let registry = SkillRegistry::new(tmp.path());
        assert!(registry.skills_dir().exists());
    }
}
