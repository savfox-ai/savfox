use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracing::info;
use zip::ZipArchive;

use crate::package::{SkillManifest, SkillPackage, SkillSourceType};

#[derive(Debug, Clone)]
pub enum InstallProgress {
    Downloading {
        bytes_downloaded: u64,
        total_bytes: u64,
    },
    Extracting {
        files_extracted: usize,
        total_files: usize,
    },
    Installing {
        name: String,
    },
    Completed {
        path: PathBuf,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone)]
pub struct InstallResult {
    pub success: bool,
    pub name: String,
    pub version: String,
    pub install_path: PathBuf,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct SkillInstaller {
    skills_dir: PathBuf,
    http_client: reqwest::Client,
}

impl SkillInstaller {
    pub fn new(skills_dir: PathBuf) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .user_agent(format!(
                "savfox-skill-registry/{}",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .unwrap_or_default();

        Self {
            skills_dir,
            http_client,
        }
    }

    /// Returns the install path for a package.
    ///
    /// The name may contain path separators (e.g. `github.com/org/repo`)
    /// which `PathBuf::join` will resolve into nested directories.
    fn install_path_for(&self, package: &SkillPackage) -> PathBuf {
        self.skills_dir.join(&package.manifest.name)
    }

    pub async fn install(
        &self,
        package: &SkillPackage,
        progress_tx: Option<tokio::sync::mpsc::Sender<InstallProgress>>,
    ) -> anyhow::Result<InstallResult> {
        let name = &package.manifest.name;
        let version = package.manifest.version.to_string();
        let install_path = self.install_path_for(package);

        // Ensure parent directories exist (e.g. github.com/org/).
        if let Some(parent) = install_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        info!(name = %name, version = %version, "installing skill");

        self.send_progress(
            &progress_tx,
            InstallProgress::Installing { name: name.clone() },
        )
        .await;

        match package.source.source_type {
            SkillSourceType::Registry | SkillSourceType::Http => {
                self.install_from_url(package, &install_path, progress_tx)
                    .await
            }
            SkillSourceType::Git => self.install_from_git(package, &install_path).await,
            SkillSourceType::Local => self.install_from_local(package, &install_path).await,
            SkillSourceType::Embedded => self.install_embedded(package, &install_path).await,
        }
    }

    async fn install_from_url(
        &self,
        package: &SkillPackage,
        install_path: &Path,
        progress_tx: Option<tokio::sync::mpsc::Sender<InstallProgress>>,
    ) -> anyhow::Result<InstallResult> {
        let url = package
            .source
            .url
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("package has no download URL"))?;

        let response = self.http_client.get(url).send().await?;
        let total_bytes = response.content_length().unwrap_or(0);
        let mut downloaded: u64 = 0;

        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            bytes.extend_from_slice(&chunk);
            downloaded += chunk.len() as u64;

            self.send_progress(
                &progress_tx,
                InstallProgress::Downloading {
                    bytes_downloaded: downloaded,
                    total_bytes,
                },
            )
            .await;
        }

        if let Some(expected) = &package.source.checksum {
            let actual = format!("{:x}", Sha256::digest(&bytes));
            if actual != *expected {
                return Ok(InstallResult {
                    success: false,
                    name: package.manifest.name.clone(),
                    version: package.manifest.version.to_string(),
                    install_path: install_path.to_path_buf(),
                    error: Some(format!(
                        "checksum mismatch: expected {}, got {}",
                        expected, actual
                    )),
                });
            }
        }

        self.extract_and_install(&bytes, package, install_path, progress_tx)
            .await
    }

    async fn extract_and_install(
        &self,
        zip_bytes: &[u8],
        package: &SkillPackage,
        install_path: &Path,
        progress_tx: Option<tokio::sync::mpsc::Sender<InstallProgress>>,
    ) -> anyhow::Result<InstallResult> {
        let install_path = install_path.to_path_buf();
        let package_name = package.manifest.name.clone();
        let package_version = package.manifest.version.to_string();
        let zip_bytes = zip_bytes.to_vec();

        let result = tokio::task::spawn_blocking(move || {
            let reader = Cursor::new(&zip_bytes);
            let mut archive = match ZipArchive::new(reader) {
                Ok(a) => a,
                Err(err) => {
                    return InstallResult {
                        success: false,
                        name: package_name,
                        version: package_version,
                        install_path: install_path.clone(),
                        error: Some(format!("failed to open archive: {err}")),
                    };
                }
            };
            let total_files = archive.len();

            if install_path.exists() {
                if let Err(err) = std::fs::remove_dir_all(&install_path) {
                    return InstallResult {
                        success: false,
                        name: package_name,
                        version: package_version,
                        install_path: install_path.clone(),
                        error: Some(format!("failed to remove existing dir: {err}")),
                    };
                }
            }
            if let Err(err) = std::fs::create_dir_all(&install_path) {
                return InstallResult {
                    success: false,
                    name: package_name,
                    version: package_version,
                    install_path: install_path.clone(),
                    error: Some(format!("failed to create dir: {err}")),
                };
            }

            for i in 0..total_files {
                let mut file = match archive.by_index(i) {
                    Ok(f) => f,
                    Err(err) => {
                        return InstallResult {
                            success: false,
                            name: package_name,
                            version: package_version,
                            install_path: install_path.clone(),
                            error: Some(format!("failed to read file {i}: {err}")),
                        };
                    }
                };
                let outpath = match file.enclosed_name() {
                    Some(path) => install_path.join(path),
                    None => continue,
                };

                if file.name().ends_with('/') {
                    if let Err(err) = std::fs::create_dir_all(&outpath) {
                        return InstallResult {
                            success: false,
                            name: package_name,
                            version: package_version,
                            install_path: install_path.clone(),
                            error: Some(format!("failed to create subdir: {err}")),
                        };
                    }
                } else {
                    if let Some(p) = outpath.parent() {
                        if !p.exists() {
                            if let Err(err) = std::fs::create_dir_all(p) {
                                return InstallResult {
                                    success: false,
                                    name: package_name,
                                    version: package_version,
                                    install_path: install_path.clone(),
                                    error: Some(format!("failed to create parent dir: {err}")),
                                };
                            }
                        }
                    }
                    let mut buffer = Vec::new();
                    if let Err(err) = file.read_to_end(&mut buffer) {
                        return InstallResult {
                            success: false,
                            name: package_name,
                            version: package_version,
                            install_path: install_path.clone(),
                            error: Some(format!("failed to read file content: {err}")),
                        };
                    }
                    if let Err(err) = std::fs::write(&outpath, &buffer) {
                        return InstallResult {
                            success: false,
                            name: package_name,
                            version: package_version,
                            install_path: install_path.clone(),
                            error: Some(format!("failed to write file: {err}")),
                        };
                    }
                }
            }

            let manifest_path = install_path.join(".savfox-manifest.json");
            let manifest = SkillManifest {
                name: package_name.clone(),
                version: semver::Version::parse(&package_version)
                    .unwrap_or(semver::Version::new(0, 0, 0)),
                description: String::new(),
                ..Default::default()
            };
            let manifest_json = match serde_json::to_string_pretty(&manifest) {
                Ok(j) => j,
                Err(err) => {
                    return InstallResult {
                        success: false,
                        name: package_name,
                        version: package_version,
                        install_path: install_path.clone(),
                        error: Some(format!("failed to serialize manifest: {err}")),
                    };
                }
            };
            if let Err(err) = std::fs::write(&manifest_path, manifest_json) {
                return InstallResult {
                    success: false,
                    name: package_name,
                    version: package_version,
                    install_path: install_path.clone(),
                    error: Some(format!("failed to write manifest: {err}")),
                };
            }

            InstallResult {
                success: true,
                name: package_name,
                version: package_version,
                install_path: install_path.clone(),
                error: None,
            }
        })
        .await?;

        if result.success {
            self.send_progress(
                &progress_tx,
                InstallProgress::Completed {
                    path: result.install_path.clone(),
                },
            )
            .await;
        }

        Ok(result)
    }

    async fn install_from_git(
        &self,
        package: &SkillPackage,
        install_path: &Path,
    ) -> anyhow::Result<InstallResult> {
        let url = package
            .source
            .url
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("package has no git URL"))?
            .clone();
        let install_path = install_path.to_path_buf();
        let package_name = package.manifest.name.clone();
        let package_version = package.manifest.version.to_string();
        let manifest = package.manifest.clone();

        let result = tokio::task::spawn_blocking(move || {
            if install_path.exists() {
                if let Err(err) = std::fs::remove_dir_all(&install_path) {
                    return InstallResult {
                        success: false,
                        name: package_name,
                        version: package_version,
                        install_path: install_path.clone(),
                        error: Some(format!("failed to remove existing dir: {err}")),
                    };
                }
            }

            let output = std::process::Command::new("git")
                .args([
                    "clone",
                    "--depth",
                    "1",
                    &url,
                    &install_path.to_string_lossy(),
                ])
                .output();

            match output {
                Ok(o) if o.status.success() => {
                    let manifest_path = install_path.join(".savfox-manifest.json");
                    if let Ok(json) = serde_json::to_string_pretty(&manifest) {
                        let _ = std::fs::write(&manifest_path, json);
                    }
                    InstallResult {
                        success: true,
                        name: package_name,
                        version: package_version,
                        install_path,
                        error: None,
                    }
                }
                Ok(o) => InstallResult {
                    success: false,
                    name: package_name,
                    version: package_version,
                    install_path: install_path.clone(),
                    error: Some(String::from_utf8_lossy(&o.stderr).to_string()),
                },
                Err(err) => InstallResult {
                    success: false,
                    name: package_name,
                    version: package_version,
                    install_path: install_path.clone(),
                    error: Some(format!("failed to run git: {err}")),
                },
            }
        })
        .await?;

        Ok(result)
    }

    async fn install_from_local(
        &self,
        package: &SkillPackage,
        install_path: &Path,
    ) -> anyhow::Result<InstallResult> {
        let source_path = package
            .source
            .path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("package has no local path"))?
            .clone();
        let install_path = install_path.to_path_buf();
        let package_name = package.manifest.name.clone();
        let package_version = package.manifest.version.to_string();
        let manifest = package.manifest.clone();

        let result = tokio::task::spawn_blocking(move || {
            if install_path.exists() {
                if let Err(err) = std::fs::remove_dir_all(&install_path) {
                    return InstallResult {
                        success: false,
                        name: package_name,
                        version: package_version,
                        install_path: install_path.clone(),
                        error: Some(format!("failed to remove existing dir: {err}")),
                    };
                }
            }

            if let Err(err) = fs_extra::dir::copy(
                &source_path,
                &install_path,
                &fs_extra::dir::CopyOptions::new(),
            ) {
                return InstallResult {
                    success: false,
                    name: package_name,
                    version: package_version,
                    install_path: install_path.clone(),
                    error: Some(format!("failed to copy directory: {err}")),
                };
            }

            let manifest_path = install_path.join(".savfox-manifest.json");
            if let Ok(json) = serde_json::to_string_pretty(&manifest) {
                let _ = std::fs::write(&manifest_path, json);
            }

            InstallResult {
                success: true,
                name: package_name,
                version: package_version,
                install_path,
                error: None,
            }
        })
        .await?;

        Ok(result)
    }

    async fn install_embedded(
        &self,
        package: &SkillPackage,
        install_path: &Path,
    ) -> anyhow::Result<InstallResult> {
        let skill_md = package.manifest.to_skill_md();
        let install_path = install_path.to_path_buf();
        let package_name = package.manifest.name.clone();
        let package_version = package.manifest.version.to_string();
        let manifest = package.manifest.clone();

        let result = tokio::task::spawn_blocking(move || {
            if install_path.exists() {
                if let Err(err) = std::fs::remove_dir_all(&install_path) {
                    return InstallResult {
                        success: false,
                        name: package_name,
                        version: package_version,
                        install_path: install_path.clone(),
                        error: Some(format!("failed to remove existing dir: {err}")),
                    };
                }
            }
            if let Err(err) = std::fs::create_dir_all(&install_path) {
                return InstallResult {
                    success: false,
                    name: package_name,
                    version: package_version,
                    install_path: install_path.clone(),
                    error: Some(format!("failed to create dir: {err}")),
                };
            }

            let skill_file = install_path.join("SKILL.md");
            if let Err(err) = std::fs::write(&skill_file, &skill_md) {
                return InstallResult {
                    success: false,
                    name: package_name,
                    version: package_version,
                    install_path: install_path.clone(),
                    error: Some(format!("failed to write SKILL.md: {err}")),
                };
            }

            let manifest_path = install_path.join(".savfox-manifest.json");
            if let Ok(json) = serde_json::to_string_pretty(&manifest) {
                let _ = std::fs::write(&manifest_path, json);
            }

            InstallResult {
                success: true,
                name: package_name,
                version: package_version,
                install_path,
                error: None,
            }
        })
        .await?;

        Ok(result)
    }

    pub async fn uninstall(&self, name: &str) -> anyhow::Result<bool> {
        let install_path = self.skills_dir.join(name);
        let name = name.to_string();
        let result = tokio::task::spawn_blocking(move || {
            if install_path.exists() {
                match std::fs::remove_dir_all(&install_path) {
                    Ok(()) => {
                        info!(name = %name, "skill uninstalled");
                        true
                    }
                    Err(_) => false,
                }
            } else {
                false
            }
        })
        .await?;
        Ok(result)
    }

    pub async fn is_installed(&self, name: &str) -> bool {
        let path = self.skills_dir.join(name);
        path.join("SKILL.md").exists() || path.join(".savfox-manifest.json").exists()
    }

    pub async fn list_installed(&self) -> anyhow::Result<Vec<(String, String)>> {
        let skills_dir = self.skills_dir.clone();
        tokio::task::spawn_blocking(move || {
            let mut installed = Vec::new();
            if !skills_dir.exists() {
                return Ok(installed);
            }

            let entries = match std::fs::read_dir(&skills_dir) {
                Ok(e) => e,
                Err(_) => return Ok(installed),
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let manifest_path = path.join(".savfox-manifest.json");
                    if manifest_path.exists() {
                        if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                            if let Ok(manifest) = serde_json::from_str::<SkillManifest>(&content) {
                                installed.push((manifest.name, manifest.version.to_string()));
                                continue;
                            }
                        }
                    }
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    installed.push((name, "unknown".to_string()));
                }
            }

            Ok(installed)
        })
        .await?
    }

    async fn send_progress(
        &self,
        tx: &Option<tokio::sync::mpsc::Sender<InstallProgress>>,
        progress: InstallProgress,
    ) {
        if let Some(tx) = tx {
            let _ = tx.send(progress).await;
        }
    }
}
