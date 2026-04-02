//! Config migration -- detects old config format and auto-migrates to current version.

use serde_json::Value;
use tracing::info;

/// Current config version
pub const CURRENT_VERSION: u32 = 2;

/// Detect config version
#[must_use]
pub fn detect_version(config: &Value) -> u32 {
    config
        .get("config_version")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(1) // No version field = v1
}

/// Migrate config to current version
pub fn migrate(mut config: Value) -> Result<(Value, Vec<String>), String> {
    let version = detect_version(&config);
    let mut log = Vec::new();

    if version >= CURRENT_VERSION {
        return Ok((config, log));
    }

    // v1 -> v2 migration
    if version < 2 {
        config = migrate_v1_to_v2(config, &mut log)?;
    }

    // Set current version
    if let Value::Object(ref mut map) = config {
        map.insert("config_version".to_owned(), Value::from(CURRENT_VERSION));
    }

    info!(from = version, to = CURRENT_VERSION, "config migrated");
    Ok((config, log))
}

/// Migrate from v1 to v2:
/// - Move flat `model` + `provider` fields on agents to `models.primary` format
/// - Rename `fallback_models` to `models.fallbacks`
fn migrate_v1_to_v2(mut config: Value, log: &mut Vec<String>) -> Result<Value, String> {
    if let Value::Object(ref mut root) = config
        && let Some(Value::Object(agents)) = root.get_mut("agents")
    {
        for (name, agent) in agents.iter_mut() {
            if let Value::Object(agent_map) = agent {
                let mut needs_migration = false;

                // Check for flat model field
                let model = agent_map
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_owned());
                let provider = agent_map
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_owned());
                let fallbacks = agent_map.get("fallback_models").cloned();

                if model.is_some() || fallbacks.is_some() {
                    needs_migration = true;
                }

                if needs_migration {
                    let mut models_obj = serde_json::Map::new();

                    if let Some(m) = &model {
                        let primary = if let Some(p) = &provider {
                            if m.contains('/') {
                                m.clone()
                            } else {
                                format!("{p}/{m}")
                            }
                        } else {
                            m.clone()
                        };
                        models_obj.insert("primary".to_owned(), Value::String(primary));
                    }

                    if let Some(Value::Array(fb)) = &fallbacks {
                        models_obj.insert("fallbacks".to_owned(), Value::Array(fb.clone()));
                    }

                    agent_map.insert("models".to_owned(), Value::Object(models_obj));

                    // Remove old fields
                    agent_map.remove("model");
                    agent_map.remove("provider");
                    agent_map.remove("fallback_models");

                    log.push(format!(
                        "agent '{name}': migrated model/provider to models.primary format"
                    ));
                }
            }
        }
    }

    Ok(config)
}

/// Create a backup of the config file before migration
pub async fn backup_config(config_path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let backup_path = config_path.with_extension("json.bak");
    tokio::fs::copy(config_path, &backup_path)
        .await
        .map_err(|e| format!("Failed to create config backup: {e}"))?;
    info!(backup = %backup_path.display(), "config backup created");
    Ok(backup_path)
}
