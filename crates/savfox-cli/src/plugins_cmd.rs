//! `savfox plugins` -- plugin install/update/uninstall and registry management.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use savfox_core::config::find_savfox_home;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

const PRIMARY_MANIFEST_FILE: &str = "savfox.plugin.toml";
const LOCK_FILE_NAME: &str = "plugins-lock.json";
const DEFAULT_REGISTRY_FILE: &str = "plugins-registry.json";
const REGISTRY_URL_ENV: &str = "SAVFOX_PLUGIN_REGISTRY_URL";

#[derive(Debug, Parser)]
pub struct PluginsCommand {
    #[clap(subcommand)]
    pub action: PluginsAction,
}

#[derive(Debug, clap::Subcommand)]
pub enum PluginsAction {
    /// List installed plugins.
    List {
        /// Output raw JSON.
        #[clap(long)]
        json: bool,
    },
    /// Install a plugin by local path, zip file, or registry id.
    Install {
        /// Plugin id (registry) or local path (directory/zip).
        plugin: String,
        /// Optional registry URL or file path (JSON).
        #[clap(long)]
        registry: Option<String>,
        /// Pin installed version to prevent updates unless --force.
        #[clap(long)]
        pin: bool,
        /// Overwrite an existing install if present.
        #[clap(long)]
        force: bool,
    },
    /// Update a plugin from registry metadata.
    Update {
        /// Plugin id.
        plugin: String,
        /// Optional registry URL or file path (JSON).
        #[clap(long)]
        registry: Option<String>,
        /// Update even if plugin is pinned.
        #[clap(long)]
        force: bool,
    },
    /// Uninstall a plugin.
    Uninstall {
        /// Plugin id.
        plugin: String,
        /// Preview changes without deleting.
        #[clap(long)]
        dry_run: bool,
        /// Remove even if required by other installed plugins.
        #[clap(long)]
        force: bool,
    },
    /// Pin an installed plugin to current version.
    Pin {
        /// Plugin id.
        plugin: String,
    },
    /// Remove version pin from a plugin.
    Unpin {
        /// Plugin id.
        plugin: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct LocalPluginManifest {
    name: String,
    version: String,
    #[serde(default)]
    dependencies: Vec<PluginDependencySpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PluginRegistryDoc {
    #[serde(default)]
    plugins: Vec<RegistryPluginEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RegistryPluginEntry {
    id: String,
    version: String,
    url: String,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    dependencies: Vec<PluginDependencySpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum PluginDependencySpec {
    Id(String),
    Detailed {
        id: String,
        #[serde(default)]
        version: Option<String>,
    },
}

impl PluginDependencySpec {
    fn id(&self) -> &str {
        match self {
            Self::Id(id) => id,
            Self::Detailed { id, .. } => id,
        }
    }

    fn version_req(&self) -> Option<&str> {
        match self {
            Self::Id(_) => None,
            Self::Detailed { version, .. } => version.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct InstalledPlugin {
    id: String,
    name: String,
    version: String,
    path: String,
    dependencies: Vec<String>,
    pinned: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct PluginLockFile {
    #[serde(default)]
    plugins: BTreeMap<String, PluginLockEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PluginLockEntry {
    version: String,
    source: String,
    pinned: bool,
    installed_at_epoch_ms: u64,
}

pub async fn run(cmd: PluginsCommand) -> Result<(), Box<dyn std::error::Error>> {
    let savfox_home = find_savfox_home()?;
    let plugins_dir = savfox_home.join("plugins");
    fs::create_dir_all(&plugins_dir)?;
    let lock_path = savfox_home.join(LOCK_FILE_NAME);
    let mut lock = load_lock_file(&lock_path)?;

    match cmd.action {
        PluginsAction::List { json } => {
            let installed = scan_installed_plugins(&plugins_dir, &lock);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({ "plugins": installed }))?
                );
            } else {
                print_plugin_table(&installed);
            }
        }
        PluginsAction::Install {
            plugin,
            registry,
            pin,
            force,
        } => {
            let plugin_path = PathBuf::from(&plugin);
            if plugin_path.exists() {
                let installed = install_from_local_source(&plugin_path, &plugins_dir, force)
                    .map_err(|err| {
                        format!(
                            "failed to install local plugin '{}': {err}",
                            plugin_path.display()
                        )
                    })?;
                upsert_lock(
                    &mut lock,
                    &installed.id,
                    &installed.version,
                    &format!("local:{}", plugin_path.display()),
                    pin,
                );
                save_lock_file(&lock_path, &lock)?;
                println!(
                    "Installed plugin '{}' ({}) from local source",
                    installed.id, installed.version
                );
            } else {
                let registry_doc = load_registry_doc(&savfox_home, registry.as_deref()).await?;
                let install_order = resolve_registry_install_order(&registry_doc, &plugin)?;
                install_registry_plugins(
                    &registry_doc,
                    &install_order,
                    &plugins_dir,
                    force,
                    &mut lock,
                    &plugin,
                    pin,
                )
                .await?;
                save_lock_file(&lock_path, &lock)?;
            }
        }
        PluginsAction::Update {
            plugin,
            registry,
            force,
        } => {
            let registry_doc = load_registry_doc(&savfox_home, registry.as_deref()).await?;
            let install_order = resolve_registry_install_order(&registry_doc, &plugin)?;
            let installed = scan_installed_plugins_map(&plugins_dir);

            let pinned = lock
                .plugins
                .get(&plugin)
                .map(|entry| entry.pinned)
                .unwrap_or(false);
            if pinned && !force {
                println!("Plugin '{plugin}' is pinned; use --force to update pinned versions");
                return Ok(());
            }

            for plugin_id in &install_order {
                let Some(entry) = find_registry_entry(&registry_doc, plugin_id) else {
                    continue;
                };
                let should_update = match installed.get(plugin_id) {
                    Some(local) => compare_versions(&local.version, &entry.version),
                    None => true,
                } || force;

                if should_update {
                    install_registry_entry(entry, &plugins_dir, true).await?;
                    let pin_state = lock
                        .plugins
                        .get(plugin_id)
                        .map(|e| e.pinned)
                        .unwrap_or(false);
                    upsert_lock(&mut lock, plugin_id, &entry.version, &entry.url, pin_state);
                    println!("Updated plugin '{}' to {}", plugin_id, entry.version);
                } else {
                    println!(
                        "Plugin '{}' already up-to-date ({})",
                        plugin_id, entry.version
                    );
                }
            }

            save_lock_file(&lock_path, &lock)?;
        }
        PluginsAction::Uninstall {
            plugin,
            dry_run,
            force,
        } => {
            let installed = scan_installed_plugins_map(&plugins_dir);
            if !installed.contains_key(&plugin) {
                if force {
                    println!("Plugin '{plugin}' is not installed");
                    return Ok(());
                }
                return Err(format!("plugin '{plugin}' is not installed").into());
            }

            let dependents = find_dependents(&installed, &plugin);
            if !dependents.is_empty() && !force {
                return Err(format!(
                    "cannot uninstall '{}': required by {} (use --force)",
                    plugin,
                    dependents.join(", ")
                )
                .into());
            }

            let target = plugins_dir.join(&plugin);
            if dry_run {
                println!("Dry run:");
                println!(" - remove {}", target.display());
                println!(" - remove lock entry '{plugin}'");
                return Ok(());
            }

            fs::remove_dir_all(&target)
                .map_err(|err| format!("failed to remove '{}': {err}", target.display()))?;
            lock.plugins.remove(&plugin);
            save_lock_file(&lock_path, &lock)?;
            println!("Uninstalled plugin '{plugin}'");
        }
        PluginsAction::Pin { plugin } => {
            let installed = scan_installed_plugins_map(&plugins_dir);
            let Some(local) = installed.get(&plugin) else {
                return Err(format!("plugin '{plugin}' is not installed").into());
            };
            let source = lock
                .plugins
                .get(&plugin)
                .map(|entry| entry.source.clone())
                .unwrap_or_else(|| "local:unknown".to_owned());
            upsert_lock(&mut lock, &plugin, &local.version, &source, true);
            save_lock_file(&lock_path, &lock)?;
            println!("Pinned plugin '{}' at {}", plugin, local.version);
        }
        PluginsAction::Unpin { plugin } => {
            let Some(entry) = lock.plugins.get_mut(&plugin) else {
                return Err(format!("plugin '{plugin}' has no lock entry").into());
            };
            entry.pinned = false;
            save_lock_file(&lock_path, &lock)?;
            println!("Unpinned plugin '{plugin}'");
        }
    }

    Ok(())
}

fn print_plugin_table(plugins: &[InstalledPlugin]) {
    println!(
        "{:<24} {:<14} {:<7} {:<20} Path",
        "Plugin", "Version", "Pinned", "Dependencies"
    );
    println!("{}", "-".repeat(120));
    for plugin in plugins {
        let deps = if plugin.dependencies.is_empty() {
            "-".to_owned()
        } else {
            plugin.dependencies.join(",")
        };
        println!(
            "{:<24} {:<14} {:<7} {:<20} {}",
            plugin.id,
            plugin.version,
            if plugin.pinned { "yes" } else { "no" },
            deps,
            plugin.path
        );
    }
}

fn load_lock_file(path: &Path) -> Result<PluginLockFile, String> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return Ok(PluginLockFile::default()),
    };
    serde_json::from_str::<PluginLockFile>(&content)
        .map_err(|err| format!("failed to parse lock file '{}': {err}", path.display()))
}

fn save_lock_file(path: &Path, lock: &PluginLockFile) -> Result<(), String> {
    let serialized = serde_json::to_string_pretty(lock)
        .map_err(|err| format!("failed to serialize lock file: {err}"))?;
    fs::write(path, serialized)
        .map_err(|err| format!("failed to write lock file '{}': {err}", path.display()))
}

fn upsert_lock(lock: &mut PluginLockFile, id: &str, version: &str, source: &str, pinned: bool) {
    lock.plugins.insert(
        id.to_owned(),
        PluginLockEntry {
            version: version.to_owned(),
            source: source.to_owned(),
            pinned,
            installed_at_epoch_ms: now_epoch_ms(),
        },
    );
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn scan_installed_plugins(plugins_dir: &Path, lock: &PluginLockFile) -> Vec<InstalledPlugin> {
    let mut plugins: Vec<InstalledPlugin> = scan_installed_plugins_map(plugins_dir)
        .into_values()
        .map(|mut plugin| {
            plugin.pinned = lock
                .plugins
                .get(&plugin.id)
                .map(|entry| entry.pinned)
                .unwrap_or(false);
            plugin
        })
        .collect();
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    plugins
}

fn scan_installed_plugins_map(plugins_dir: &Path) -> HashMap<String, InstalledPlugin> {
    let mut map = HashMap::new();
    let entries = match fs::read_dir(plugins_dir) {
        Ok(entries) => entries,
        Err(_) => return map,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(plugin_id) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(manifest) = load_local_manifest(&path) else {
            continue;
        };
        let dependencies = manifest
            .dependencies
            .iter()
            .map(|dep| dep.id().to_owned())
            .collect::<Vec<_>>();

        map.insert(
            plugin_id.to_owned(),
            InstalledPlugin {
                id: plugin_id.to_owned(),
                name: manifest.name,
                version: manifest.version,
                path: path.to_string_lossy().to_string(),
                dependencies,
                pinned: false,
            },
        );
    }

    map
}

fn find_dependents(
    installed: &HashMap<String, InstalledPlugin>,
    dependency_id: &str,
) -> Vec<String> {
    let mut dependents = installed
        .values()
        .filter(|plugin| plugin.dependencies.iter().any(|dep| dep == dependency_id))
        .map(|plugin| plugin.id.clone())
        .collect::<Vec<_>>();
    dependents.sort();
    dependents
}

fn resolve_manifest_path(plugin_dir: &Path) -> Option<PathBuf> {
    let primary = plugin_dir.join(PRIMARY_MANIFEST_FILE);
    if primary.exists() {
        return Some(primary);
    }
    None
}

fn load_local_manifest(plugin_dir: &Path) -> Result<LocalPluginManifest, String> {
    let Some(path) = resolve_manifest_path(plugin_dir) else {
        return Err(format!(
            "missing plugin manifest in '{}'",
            plugin_dir.display()
        ));
    };
    let content = fs::read_to_string(&path)
        .map_err(|err| format!("failed to read '{}': {err}", path.display()))?;
    toml::from_str::<LocalPluginManifest>(&content)
        .map_err(|err| format!("failed to parse '{}': {err}", path.display()))
}

fn install_from_local_source(
    source: &Path,
    plugins_dir: &Path,
    force: bool,
) -> Result<InstalledPlugin, String> {
    if source.is_dir() {
        return install_from_directory(source, plugins_dir, force);
    }
    if source.is_file()
        && source
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("zip"))
            .unwrap_or(false)
    {
        let archive_bytes = fs::read(source)
            .map_err(|err| format!("failed to read archive '{}': {err}", source.display()))?;
        return install_from_archive_bytes(&archive_bytes, plugins_dir, force);
    }
    Err(format!(
        "unsupported local plugin source '{}': expected directory or .zip file",
        source.display()
    ))
}

fn install_from_directory(
    source_dir: &Path,
    plugins_dir: &Path,
    force: bool,
) -> Result<InstalledPlugin, String> {
    let Some(plugin_id) = source_dir.file_name().and_then(|n| n.to_str()) else {
        return Err(format!("invalid plugin path '{}'", source_dir.display()));
    };
    let manifest = load_local_manifest(source_dir)?;
    let target_dir = plugins_dir.join(plugin_id);
    if target_dir.exists() {
        if !force {
            return Err(format!(
                "plugin '{}' already exists at '{}' (use --force to overwrite)",
                plugin_id,
                target_dir.display()
            ));
        }
        fs::remove_dir_all(&target_dir)
            .map_err(|err| format!("failed to clear '{}': {err}", target_dir.display()))?;
    }
    copy_dir_all(source_dir, &target_dir)?;

    Ok(InstalledPlugin {
        id: plugin_id.to_owned(),
        name: manifest.name,
        version: manifest.version,
        path: target_dir.to_string_lossy().to_string(),
        dependencies: manifest
            .dependencies
            .iter()
            .map(|dep| dep.id().to_owned())
            .collect(),
        pinned: false,
    })
}

fn install_from_archive_bytes(
    archive_bytes: &[u8],
    plugins_dir: &Path,
    force: bool,
) -> Result<InstalledPlugin, String> {
    let temp_dir = tempfile::tempdir()
        .map_err(|err| format!("failed to allocate temporary directory: {err}"))?;
    extract_zip_archive(archive_bytes, temp_dir.path())?;
    let root = find_plugin_root(temp_dir.path()).ok_or_else(|| {
        "archive does not contain a plugin manifest (savfox.plugin.toml or plugin.toml)".to_owned()
    })?;

    install_from_directory(&root, plugins_dir, force)
}

fn copy_dir_all(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target)
        .map_err(|err| format!("failed to create '{}': {err}", target.display()))?;
    for entry in fs::read_dir(source)
        .map_err(|err| format!("failed to read '{}': {err}", source.display()))?
    {
        let entry = entry.map_err(|err| format!("failed to read dir entry: {err}"))?;
        let src_path = entry.path();
        let dst_path = target.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else if src_path.is_file() {
            fs::copy(&src_path, &dst_path).map_err(|err| {
                format!(
                    "failed to copy '{}' -> '{}': {err}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn sanitize_archive_path(path: &Path) -> Option<PathBuf> {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if clean.as_os_str().is_empty() {
        None
    } else {
        Some(clean)
    }
}

fn extract_zip_archive(archive_bytes: &[u8], destination: &Path) -> Result<(), String> {
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes))
        .map_err(|err| format!("invalid zip archive: {err}"))?;
    for idx in 0..archive.len() {
        let mut file = archive
            .by_index(idx)
            .map_err(|err| format!("zip entry error: {err}"))?;
        let raw_name = Path::new(file.name());
        let Some(clean_relative) = sanitize_archive_path(raw_name) else {
            continue;
        };
        let out_path = destination.join(clean_relative);
        if file.is_dir() {
            fs::create_dir_all(&out_path)
                .map_err(|err| format!("failed to create '{}': {err}", out_path.display()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create '{}': {err}", parent.display()))?;
        }
        let mut out_file = fs::File::create(&out_path)
            .map_err(|err| format!("failed to create '{}': {err}", out_path.display()))?;
        std::io::copy(&mut file, &mut out_file)
            .map_err(|err| format!("failed to extract '{}': {err}", out_path.display()))?;
    }
    Ok(())
}

fn find_plugin_root(base: &Path) -> Option<PathBuf> {
    if resolve_manifest_path(base).is_some() {
        return Some(base.to_path_buf());
    }

    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if resolve_manifest_path(&path).is_some() {
                return Some(path);
            }
            stack.push(path);
        }
    }
    None
}

fn find_registry_entry<'a>(
    registry: &'a PluginRegistryDoc,
    plugin_id: &str,
) -> Option<&'a RegistryPluginEntry> {
    registry.plugins.iter().find(|entry| entry.id == plugin_id)
}

fn resolve_registry_install_order(
    registry: &PluginRegistryDoc,
    root_plugin_id: &str,
) -> Result<Vec<String>, String> {
    let mut install_order = Vec::new();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();

    fn visit(
        plugin_id: &str,
        registry: &PluginRegistryDoc,
        install_order: &mut Vec<String>,
        visiting: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) -> Result<(), String> {
        if visited.contains(plugin_id) {
            return Ok(());
        }
        if !visiting.insert(plugin_id.to_owned()) {
            return Err(format!(
                "cycle detected in plugin dependencies at '{plugin_id}'"
            ));
        }

        let entry = find_registry_entry(registry, plugin_id)
            .ok_or_else(|| format!("plugin '{plugin_id}' not found in registry"))?;

        for dep in &entry.dependencies {
            let dep_id = dep.id();
            let dep_entry = find_registry_entry(registry, dep_id).ok_or_else(|| {
                format!("dependency '{dep_id}' (required by '{plugin_id}') not found in registry")
            })?;

            if let Some(req_raw) = dep.version_req() {
                let req = VersionReq::parse(req_raw).map_err(|err| {
                    format!(
                        "invalid version requirement '{req_raw}' for dependency '{dep_id}': {err}"
                    )
                })?;
                let dep_ver = Version::parse(&dep_entry.version).map_err(|err| {
                    format!(
                        "invalid dependency version '{}' for '{}': {err}",
                        dep_entry.version, dep_id
                    )
                })?;
                if !req.matches(&dep_ver) {
                    return Err(format!(
                        "dependency '{}' version '{}' does not satisfy '{}'",
                        dep_id, dep_entry.version, req_raw
                    ));
                }
            }

            visit(dep_id, registry, install_order, visiting, visited)?;
        }

        visiting.remove(plugin_id);
        visited.insert(plugin_id.to_owned());
        install_order.push(plugin_id.to_owned());
        Ok(())
    }

    visit(
        root_plugin_id,
        registry,
        &mut install_order,
        &mut visiting,
        &mut visited,
    )?;

    Ok(install_order)
}

fn compare_versions(local: &str, remote: &str) -> bool {
    let Ok(local_ver) = Version::parse(local) else {
        return local != remote;
    };
    let Ok(remote_ver) = Version::parse(remote) else {
        return local != remote;
    };
    remote_ver > local_ver
}

async fn install_registry_plugins(
    registry_doc: &PluginRegistryDoc,
    install_order: &[String],
    plugins_dir: &Path,
    force: bool,
    lock: &mut PluginLockFile,
    root_plugin_id: &str,
    root_pin: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    for plugin_id in install_order {
        let entry = find_registry_entry(registry_doc, plugin_id).ok_or_else(|| {
            format!("plugin '{plugin_id}' resolved in dependency graph but missing from registry")
        })?;
        let installed = install_registry_entry(entry, plugins_dir, force).await?;
        let pin = if plugin_id == root_plugin_id {
            root_pin
        } else {
            lock.plugins
                .get(plugin_id)
                .map(|existing| existing.pinned)
                .unwrap_or(false)
        };
        upsert_lock(lock, plugin_id, &installed.version, &entry.url, pin);
        println!("Installed plugin '{}' ({})", plugin_id, installed.version);
    }
    Ok(())
}

async fn install_registry_entry(
    entry: &RegistryPluginEntry,
    plugins_dir: &Path,
    force: bool,
) -> Result<InstalledPlugin, Box<dyn std::error::Error>> {
    let archive_bytes = download_registry_archive(&entry.url).await?;
    if let Some(expected) = entry.sha256.as_deref() {
        verify_sha256(&archive_bytes, expected)?;
    }
    let mut installed = install_from_archive_bytes(&archive_bytes, plugins_dir, force)
        .map_err(|err| format!("failed to install plugin '{}': {err}", entry.id))?;

    if installed.id != entry.id {
        let expected_dir = plugins_dir.join(&entry.id);
        let actual_dir = plugins_dir.join(&installed.id);
        if actual_dir.exists() {
            if expected_dir.exists() {
                fs::remove_dir_all(&expected_dir).map_err(|err| {
                    format!(
                        "failed to replace existing plugin dir '{}': {err}",
                        expected_dir.display()
                    )
                })?;
            }
            fs::rename(&actual_dir, &expected_dir).map_err(|err| {
                format!(
                    "failed to normalize plugin id '{}' -> '{}': {err}",
                    installed.id, entry.id
                )
            })?;
        }
        installed.id = entry.id.clone();
        installed.path = expected_dir.to_string_lossy().to_string();
    }

    if !entry.version.is_empty() && installed.version != entry.version {
        return Err(format!(
            "installed plugin '{}' version '{}' does not match registry version '{}'",
            entry.id, installed.version, entry.version
        )
        .into());
    }

    Ok(installed)
}

async fn download_registry_archive(url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if url.starts_with("http://") || url.starts_with("https://") {
        let response = reqwest::get(url).await?;
        if !response.status().is_success() {
            return Err(format!("download failed (HTTP {}): {}", response.status(), url).into());
        }
        let bytes = response.bytes().await?;
        return Ok(bytes.to_vec());
    }
    let bytes = fs::read(url).map_err(|err| format!("failed to read '{url}': {err}"))?;
    Ok(bytes)
}

fn verify_sha256(data: &[u8], expected_hex: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let computed = hex::encode(hasher.finalize());
    let expected = expected_hex.trim().to_ascii_lowercase();
    if computed != expected {
        return Err(format!("sha256 mismatch: expected {expected}, got {computed}").into());
    }
    Ok(())
}

async fn load_registry_doc(
    savfox_home: &Path,
    explicit_registry: Option<&str>,
) -> Result<PluginRegistryDoc, Box<dyn std::error::Error>> {
    let source = explicit_registry
        .map(|s| s.to_owned())
        .or_else(|| std::env::var(REGISTRY_URL_ENV).ok())
        .or_else(|| {
            let default_file = savfox_home.join(DEFAULT_REGISTRY_FILE);
            if default_file.exists() {
                Some(default_file.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| {
            format!("no plugin registry configured; provide --registry or set {REGISTRY_URL_ENV}")
        })?;

    let content = if source.starts_with("http://") || source.starts_with("https://") {
        let response = reqwest::get(&source).await?;
        if !response.status().is_success() {
            return Err(format!(
                "failed to fetch registry (HTTP {}): {source}",
                response.status()
            )
            .into());
        }
        response.text().await?
    } else {
        fs::read_to_string(&source)
            .map_err(|err| format!("failed to read registry '{source}': {err}"))?
    };

    let doc = serde_json::from_str::<PluginRegistryDoc>(&content)
        .map_err(|err| format!("invalid registry JSON from '{source}': {err}"))?;
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_install_order_dependency_first() {
        let registry = PluginRegistryDoc {
            plugins: vec![
                RegistryPluginEntry {
                    id: "a".to_owned(),
                    version: "1.0.0".to_owned(),
                    url: "https://example.invalid/a.zip".to_owned(),
                    sha256: None,
                    dependencies: vec![PluginDependencySpec::Id("b".to_owned())],
                },
                RegistryPluginEntry {
                    id: "b".to_owned(),
                    version: "1.0.0".to_owned(),
                    url: "https://example.invalid/b.zip".to_owned(),
                    sha256: None,
                    dependencies: vec![],
                },
            ],
        };

        let order = resolve_registry_install_order(&registry, "a").expect("order");
        assert_eq!(order, vec!["b".to_owned(), "a".to_owned()]);
    }

    #[test]
    fn compare_versions_uses_semver_when_available() {
        assert!(compare_versions("1.2.3", "1.2.4"));
        assert!(!compare_versions("1.2.3", "1.2.3"));
    }

    #[test]
    fn sanitize_archive_path_rejects_parent_components() {
        assert!(sanitize_archive_path(Path::new("../evil")).is_none());
        assert_eq!(
            sanitize_archive_path(Path::new("good/file.txt")),
            Some(PathBuf::from("good").join("file.txt"))
        );
    }

    #[test]
    fn dependency_version_requirement_validation() {
        let registry = PluginRegistryDoc {
            plugins: vec![
                RegistryPluginEntry {
                    id: "a".to_owned(),
                    version: "1.0.0".to_owned(),
                    url: "https://example.invalid/a.zip".to_owned(),
                    sha256: None,
                    dependencies: vec![PluginDependencySpec::Detailed {
                        id: "b".to_owned(),
                        version: Some("^2.0".to_owned()),
                    }],
                },
                RegistryPluginEntry {
                    id: "b".to_owned(),
                    version: "2.1.0".to_owned(),
                    url: "https://example.invalid/b.zip".to_owned(),
                    sha256: None,
                    dependencies: vec![],
                },
            ],
        };

        let order = resolve_registry_install_order(&registry, "a").expect("valid dependencies");
        assert_eq!(order, vec!["b".to_owned(), "a".to_owned()]);
    }
}
