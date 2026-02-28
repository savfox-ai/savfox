use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

const CHANNELS_SUBDIR: &str = "channels";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub id: String,
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

fn channel_config_path(savfox_home: &PathBuf, channel_id: &str) -> PathBuf {
    channels_dir(savfox_home).join(format!("{}.json", channel_id))
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
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => match serde_json::from_str::<ChannelConfig>(&content) {
                    Ok(config) => configs.push(config),
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
    channel_id: &str,
) -> std::io::Result<Option<ChannelConfig>> {
    let path = channel_config_path(savfox_home, channel_id);
    if !path.exists() {
        return Ok(None);
    }

    let content = tokio::fs::read_to_string(&path).await?;
    match serde_json::from_str::<ChannelConfig>(&content) {
        Ok(config) => Ok(Some(config)),
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

    let path = channel_config_path(savfox_home, &config.id);
    let content = serde_json::to_string_pretty(config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    tokio::fs::write(&path, content).await?;
    info!("Saved channel config: {}", path.display());
    Ok(())
}

pub async fn delete_channel_config(
    savfox_home: &PathBuf,
    channel_id: &str,
) -> std::io::Result<bool> {
    let path = channel_config_path(savfox_home, channel_id);
    if !path.exists() {
        return Ok(false);
    }

    tokio::fs::remove_file(&path).await?;
    info!("Deleted channel config: {}", path.display());
    Ok(true)
}

pub async fn merge_channel_config(
    savfox_home: &PathBuf,
    channel_id: &str,
    channel_name: &str,
    patch: &Value,
) -> std::io::Result<ChannelConfig> {
    let existing = get_channel_config(savfox_home, channel_id).await?;
    let now = chrono::Utc::now().timestamp();

    let mut config = existing.unwrap_or(ChannelConfig {
        id: channel_id.to_string(),
        name: channel_name.to_string(),
        enabled: true,
        config: Value::Object(serde_json::Map::new()),
        agent_id: None,
        created_at: Some(now),
        updated_at: Some(now),
    });

    if let Some(obj) = patch.as_object() {
        if let Some(config_obj) = config.config.as_object_mut() {
            for (key, value) in obj {
                if key == "agent_id" || key == "name" || key == "enabled" {
                    continue;
                }
                if value.is_null() {
                    config_obj.remove(key);
                } else {
                    config_obj.insert(key.clone(), value.clone());
                }
            }
        }
    }

    if let Some(name) = patch.get("name").and_then(|v| v.as_str()) {
        config.name = name.to_string();
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
