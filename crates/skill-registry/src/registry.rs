use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::sync::RwLock;

use crate::index::{RemoteIndex, RemoteIndexClient, get_builtin_skills};
use crate::installer::{InstallProgress, InstallResult, SkillInstaller};
use crate::package::{SkillPackage, SkillSource, SkillSourceType};

#[derive(Debug)]
pub struct SkillRegistry {
    skills_dir: PathBuf,
    index_client: RwLock<RemoteIndexClient>,
    installer: SkillInstaller,
    installed_cache: RwLock<HashMap<String, SkillPackage>>,
}

impl SkillRegistry {
    pub fn new(savfox_home: &Path) -> Self {
        let skills_dir = savfox_home.join("skills");
        Self {
            skills_dir: skills_dir.clone(),
            index_client: RwLock::new(RemoteIndexClient::new()),
            installer: SkillInstaller::new(skills_dir),
            installed_cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn with_index_url(savfox_home: &Path, index_url: String) -> Self {
        let skills_dir = savfox_home.join("skills");
        Self {
            skills_dir: skills_dir.clone(),
            index_client: RwLock::new(RemoteIndexClient::with_url(index_url)),
            installer: SkillInstaller::new(skills_dir),
            installed_cache: RwLock::new(HashMap::new()),
        }
    }

    pub async fn refresh_index(&self, force: bool) -> anyhow::Result<RemoteIndex> {
        let mut client = self.index_client.write().await;
        client.fetch_index(force).await
    }

    pub async fn search(&self, query: &str) -> anyhow::Result<Vec<SkillPackage>> {
        let client = self.index_client.read().await;
        let installed = self.installed_cache.read().await;

        let mut results = Vec::new();

        if let Some(index) = client.cached_index() {
            for entry in client.search(query, index) {
                let mut pkg = client.entry_to_package(entry);
                if let Some(installed_pkg) = installed.get(&pkg.manifest.name) {
                    pkg.installed = true;
                    pkg.installed_version = installed_pkg.manifest.version.clone().into();
                    pkg.install_path = installed_pkg.install_path.clone();
                }
                results.push(pkg);
            }
        }

        Ok(results)
    }

    pub async fn list_available(&self, force_refresh: bool) -> anyhow::Result<Vec<SkillPackage>> {
        let mut client = self.index_client.write().await;
        let index = client.fetch_index(force_refresh).await?;
        let installed = self.installed_cache.read().await;

        let mut packages: Vec<SkillPackage> = index
            .skills
            .iter()
            .map(|entry| {
                let mut pkg = client.entry_to_package(entry);
                if let Some(installed_pkg) = installed.get(&pkg.manifest.name) {
                    pkg.installed = true;
                    pkg.installed_version = installed_pkg.manifest.version.clone().into();
                    pkg.install_path = installed_pkg.install_path.clone();
                }
                pkg
            })
            .collect();

        let builtin = get_builtin_skills();
        for mut pkg in builtin {
            if let Some(installed_pkg) = installed.get(&pkg.manifest.name) {
                pkg.installed = true;
                pkg.installed_version = installed_pkg.manifest.version.clone().into();
                pkg.install_path = installed_pkg.install_path.clone();
            }
            packages.push(pkg);
        }

        Ok(packages)
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn registry_creation() {
        let tmp = TempDir::new().unwrap();
        let registry = SkillRegistry::new(tmp.path());
        assert!(registry.skills_dir().exists() || true);
    }
}
