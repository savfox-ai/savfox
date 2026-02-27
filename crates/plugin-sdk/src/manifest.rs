use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::types::{PluginConfigUiHint, PluginIsolationMode, PluginKind, PluginMetadata};

pub const PLUGIN_MANIFEST_FILENAME: &str = "savfox.plugin.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default)]
    pub kind: PluginKind,
    #[serde(default)]
    pub isolation_mode: PluginIsolationMode,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub ui_hints: std::collections::HashMap<String, PluginConfigUiHint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    #[serde(default)]
    pub provides_tools: Vec<String>,
    #[serde(default)]
    pub provides_commands: Vec<String>,
    #[serde(default)]
    pub provides_hooks: Vec<String>,
}

impl PluginManifest {
    pub fn to_metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            author: self.author.clone(),
            homepage: self.homepage.clone(),
            license: self.license.clone(),
            kind: self.kind,
            tags: self.tags.clone(),
            isolation_mode: self.isolation_mode,
        }
    }
}

#[derive(Debug)]
pub enum ManifestLoadResult {
    Ok {
        manifest: PluginManifest,
        manifest_path: std::path::PathBuf,
    },
    Err {
        error: String,
        manifest_path: std::path::PathBuf,
    },
}

pub fn resolve_plugin_manifest_path(root_dir: &Path) -> std::path::PathBuf {
    let candidate = root_dir.join(PLUGIN_MANIFEST_FILENAME);
    if candidate.exists() {
        return candidate;
    }
    candidate
}

pub fn load_plugin_manifest(root_dir: &Path) -> ManifestLoadResult {
    let manifest_path = resolve_plugin_manifest_path(root_dir);

    if !manifest_path.exists() {
        return ManifestLoadResult::Err {
            error: format!("Plugin manifest not found: {}", manifest_path.display()),
            manifest_path,
        };
    }

    let content = match std::fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(e) => {
            return ManifestLoadResult::Err {
                error: format!("Failed to read manifest: {}", e),
                manifest_path,
            };
        }
    };

    let manifest: PluginManifest = match serde_json::from_str(&content) {
        Ok(m) => m,
        Err(e) => {
            return ManifestLoadResult::Err {
                error: format!("Failed to parse manifest: {}", e),
                manifest_path,
            };
        }
    };

    if manifest.id.trim().is_empty() {
        return ManifestLoadResult::Err {
            error: "Plugin manifest requires non-empty 'id' field".to_string(),
            manifest_path,
        };
    }

    if manifest.name.trim().is_empty() {
        return ManifestLoadResult::Err {
            error: "Plugin manifest requires non-empty 'name' field".to_string(),
            manifest_path,
        };
    }

    ManifestLoadResult::Ok {
        manifest,
        manifest_path,
    }
}
