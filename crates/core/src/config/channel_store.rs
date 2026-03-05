use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

const CHANNELS_SUBDIR: &str = "channels";
const DEFAULT_CHANNEL_KIND: &str = "channel";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    pub config: Value,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<i64>,
}

fn channels_dir(savfox_home: &PathBuf) -> PathBuf {
    savfox_home.join(CHANNELS_SUBDIR)
}

fn is_json_file(path: &Path) -> bool {
    path.extension().map(|e| e == "json").unwrap_or(false)
}

fn normalize_channel_slug(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
            continue;
        }

        if ch.is_ascii_whitespace() || matches!(ch, '-' | '_' | ':' | '/' | '\\' | '.') {
            if !out.is_empty() && !prev_dash {
                out.push('-');
                prev_dash = true;
            }
            continue;
        }
    }

    let normalized = out.trim_matches('-');
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_string())
    }
}

fn normalize_channel_name(raw: &str) -> Option<String> {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let compact = compact.trim();
    if compact.is_empty() {
        None
    } else {
        Some(compact.to_string())
    }
}

fn kind_from_id_candidate(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if let Some((prefix, _)) = trimmed.split_once('-') {
        normalize_channel_slug(prefix).or_else(|| normalize_channel_slug(trimmed))
    } else {
        normalize_channel_slug(trimmed)
    }
}

fn resolve_kind(raw_kind: &str, fallback: Option<&str>) -> String {
    normalize_channel_slug(raw_kind)
        .or_else(|| fallback.and_then(kind_from_id_candidate))
        .unwrap_or_else(|| DEFAULT_CHANNEL_KIND.to_string())
}

fn resolve_name(raw_name: &str, fallback_kind: &str) -> String {
    normalize_channel_name(raw_name).unwrap_or_else(|| fallback_kind.to_string())
}

fn resolve_id(name: &str, kind: &str) -> String {
    let name_slug = normalize_channel_slug(name).unwrap_or_else(|| kind.to_string());
    format!("{kind}-{name_slug}")
}

fn normalize_config(config: &mut ChannelConfig) {
    let kind = resolve_kind(&config.kind, Some(&config.id));
    config.kind = kind.clone();
    config.name = resolve_name(&config.name, &kind);
    config.id = resolve_id(&config.name, &kind);
}

fn normalized_selector(selector: &str) -> String {
    normalize_channel_slug(selector).unwrap_or_else(|| selector.trim().to_ascii_lowercase())
}

fn selector_matches(config: &ChannelConfig, selector: &str) -> bool {
    let selector = normalized_selector(selector);
    let id = normalized_selector(&config.id);
    let kind = normalized_selector(&config.kind);
    id == selector || kind == selector
}

async fn find_channel_config_path_by_selector(
    savfox_home: &PathBuf,
    selector: &str,
) -> std::io::Result<Option<PathBuf>> {
    let dir = channels_dir(savfox_home);
    if !dir.exists() {
        return Ok(None);
    }

    let mut entries = tokio::fs::read_dir(&dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !is_json_file(&path) {
            continue;
        }

        let Ok(content) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        let Ok(mut config) = serde_json::from_str::<ChannelConfig>(&content) else {
            continue;
        };
        normalize_config(&mut config);

        if selector_matches(&config, selector) {
            return Ok(Some(path));
        }
    }

    Ok(None)
}

pub async fn ensure_channels_dir(savfox_home: &PathBuf) -> std::io::Result<()> {
    let dir = channels_dir(savfox_home);
    if !dir.exists() {
        tokio::fs::create_dir_all(&dir).await?;
        info!("Created channels directory: {}", dir.display());
    }
    Ok(())
}

pub async fn list_channel_configs(savfox_home: &PathBuf) -> std::io::Result<Vec<ChannelConfig>> {
    let dir = channels_dir(savfox_home);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut configs = Vec::new();
    let mut entries = tokio::fs::read_dir(&dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if is_json_file(&path) {
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => match serde_json::from_str::<ChannelConfig>(&content) {
                    Ok(mut config) => {
                        normalize_config(&mut config);
                        configs.push(config);
                    }
                    Err(e) => {
                        warn!("Failed to parse channel config {}: {}", path.display(), e);
                    }
                },
                Err(e) => {
                    warn!("Failed to read channel config {}: {}", path.display(), e);
                }
            }
        }
    }

    configs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(configs)
}

pub async fn get_channel_config(
    savfox_home: &PathBuf,
    selector: &str,
) -> std::io::Result<Option<ChannelConfig>> {
    let Some(path) = find_channel_config_path_by_selector(savfox_home, selector).await? else {
        return Ok(None);
    };

    let content = tokio::fs::read_to_string(&path).await?;
    match serde_json::from_str::<ChannelConfig>(&content) {
        Ok(mut config) => {
            normalize_config(&mut config);
            Ok(Some(config))
        }
        Err(e) => {
            warn!("Failed to parse channel config {}: {}", path.display(), e);
            Ok(None)
        }
    }
}

pub async fn save_channel_config(
    savfox_home: &PathBuf,
    config: &ChannelConfig,
) -> std::io::Result<()> {
    ensure_channels_dir(savfox_home).await?;

    let mut normalized = config.clone();
    normalize_config(&mut normalized);

    let path = if let Some(existing) =
        find_channel_config_path_by_selector(savfox_home, &normalized.id).await?
    {
        existing
    } else if normalized.kind != normalized.id {
        if let Some(existing) =
            find_channel_config_path_by_selector(savfox_home, &normalized.kind).await?
        {
            existing
        } else {
            channels_dir(savfox_home).join(format!("{}.json", uuid::Uuid::new_v4()))
        }
    } else {
        channels_dir(savfox_home).join(format!("{}.json", uuid::Uuid::new_v4()))
    };

    let content = serde_json::to_string_pretty(&normalized)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    tokio::fs::write(&path, content).await?;
    info!("Saved channel config: {}", path.display());
    Ok(())
}

pub async fn delete_channel_config(
    savfox_home: &PathBuf,
    selector: &str,
) -> std::io::Result<bool> {
    let Some(path) = find_channel_config_path_by_selector(savfox_home, selector).await? else {
        return Ok(false);
    };

    tokio::fs::remove_file(&path).await?;
    info!("Deleted channel config: {}", path.display());
    Ok(true)
}

pub async fn merge_channel_config(
    savfox_home: &PathBuf,
    channel_kind: &str,
    channel_name: &str,
    patch: &Value,
) -> std::io::Result<ChannelConfig> {
    let kind = resolve_kind(channel_kind, None);
    let existing = get_channel_config(savfox_home, &kind).await?;
    let now = chrono::Utc::now().timestamp();

    let mut config = existing.unwrap_or(ChannelConfig {
        id: String::new(),
        kind: kind.clone(),
        name: channel_name.to_string(),
        enabled: true,
        config: Value::Object(serde_json::Map::new()),
        agent_id: None,
        created_at: Some(now),
        updated_at: Some(now),
    });

    config.kind = kind;

    if !config.config.is_object() {
        config.config = Value::Object(serde_json::Map::new());
    }

    if let Some(obj) = patch.as_object()
        && let Some(config_obj) = config.config.as_object_mut()
    {
        for (key, value) in obj {
            if key == "agent_id"
                || key == "name"
                || key == "enabled"
                || key == "id"
                || key == "kind"
            {
                continue;
            }
            if value.is_null() {
                config_obj.remove(key);
            } else {
                config_obj.insert(key.clone(), value.clone());
            }
        }
    }

    if let Some(name) = patch.get("name").and_then(|v| v.as_str()) {
        config.name = name.to_string();
    } else if config.name.trim().is_empty() {
        config.name = channel_name.to_string();
    }
    if let Some(enabled) = patch.get("enabled").and_then(|v| v.as_bool()) {
        config.enabled = enabled;
    }
    if let Some(agent_id) = patch.get("agent_id").and_then(|v| v.as_str()) {
        config.agent_id = if agent_id.is_empty() {
            None
        } else {
            Some(agent_id.to_string())
        };
    }

    config.name = resolve_name(&config.name, &config.kind);
    config.id = resolve_id(&config.name, &config.kind);
    if config.created_at.is_none() {
        config.created_at = Some(now);
    }
    config.updated_at = Some(now);

    save_channel_config(savfox_home, &config).await?;
    Ok(config)
}

pub fn channel_config_to_json(config: &ChannelConfig) -> Value {
    serde_json::to_value(config).unwrap_or(Value::Null)
}

pub fn channel_configs_to_json(configs: &[ChannelConfig]) -> Value {
    serde_json::to_value(configs).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    async fn list_json_files(dir: &PathBuf) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            return out;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if is_json_file(&path) {
                out.push(path);
            }
        }
        out.sort();
        out
    }

    #[tokio::test]
    async fn channel_files_use_uuid_names_and_lookup_by_kind() {
        let home =
            std::env::temp_dir().join(format!("savfox-channel-store-{}", uuid::Uuid::new_v4()));
        let config = ChannelConfig {
            id: "matrix".to_string(),
            kind: "matrix".to_string(),
            name: "Matrix".to_string(),
            enabled: true,
            config: json!({
                "homeserver": "http://127.0.0.1:6006",
                "userId": "@bot:127.0.0.1:6006"
            }),
            agent_id: None,
            created_at: Some(1),
            updated_at: Some(1),
        };

        save_channel_config(&home, &config).await.expect("save");
        let channels = home.join(CHANNELS_SUBDIR);
        let files = list_json_files(&channels).await;
        assert_eq!(files.len(), 1);
        let file_name = files[0]
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        assert_ne!(file_name, "matrix.json");

        let loaded = get_channel_config(&home, "matrix")
            .await
            .expect("lookup")
            .expect("exists");
        assert_eq!(loaded.id, "matrix-matrix");
        assert_eq!(loaded.kind, "matrix");
        assert_eq!(
            loaded
                .config
                .get("homeserver")
                .and_then(|v| v.as_str())
                .unwrap_or_default(),
            "http://127.0.0.1:6006"
        );

        let deleted = delete_channel_config(&home, "matrix")
            .await
            .expect("delete");
        assert!(deleted);
        let files_after = list_json_files(&channels).await;
        assert!(files_after.is_empty());

        let _ = tokio::fs::remove_dir_all(&home).await;
    }

    #[tokio::test]
    async fn merge_channel_generates_id_from_name_and_keeps_kind_lookup() {
        let home =
            std::env::temp_dir().join(format!("savfox-channel-store-{}", uuid::Uuid::new_v4()));
        let merged = merge_channel_config(
            &home,
            "Matrix",
            "My Matrix Home",
            &json!({ "homeserver": "http://127.0.0.1:6006" }),
        )
        .await
        .expect("merge");

        assert_eq!(merged.kind, "matrix");
        assert_eq!(merged.id, "matrix-my-matrix-home");
        assert_eq!(merged.name, "My Matrix Home");

        let by_kind = get_channel_config(&home, "matrix")
            .await
            .expect("lookup by kind")
            .expect("config should exist");
        assert_eq!(by_kind.id, "matrix-my-matrix-home");
        assert_eq!(by_kind.kind, "matrix");

        let by_id = get_channel_config(&home, "matrix-my-matrix-home")
            .await
            .expect("lookup by id")
            .expect("config should exist");
        assert_eq!(by_id.kind, "matrix");

        let _ = tokio::fs::remove_dir_all(&home).await;
    }

    #[tokio::test]
    async fn get_channel_backfills_missing_kind_for_legacy_files() {
        let home =
            std::env::temp_dir().join(format!("savfox-channel-store-{}", uuid::Uuid::new_v4()));
        let channels = home.join(CHANNELS_SUBDIR);
        tokio::fs::create_dir_all(&channels).await.expect("mkdir channels");
        tokio::fs::write(
            channels.join("legacy.json"),
            r#"{
  "id": "matrix",
  "name": "Legacy Matrix",
  "enabled": true,
  "config": {"homeserver":"http://127.0.0.1:6006"}
}"#,
        )
        .await
        .expect("write");

        let loaded = get_channel_config(&home, "matrix")
            .await
            .expect("lookup")
            .expect("exists");
        assert_eq!(loaded.kind, "matrix");
        assert_eq!(loaded.id, "matrix-legacy-matrix");
        assert_eq!(loaded.name, "Legacy Matrix");

        let _ = tokio::fs::remove_dir_all(&home).await;
    }
}
