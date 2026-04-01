use std::path::{Path, PathBuf};

use savfox_utils::string::normalize_slug;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

const CHANNELS_SUBDIR: &str = "channels";
const DEFAULT_CHANNEL_KIND: &str = "channel";

fn default_router_agent_id() -> String {
    "default".to_string()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteMatchType {
    #[default]
    Peer,
    ParentPeer,
    Guild,
    GuildRoles,
    Team,
    Account,
    Channel,
    Default,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteRule {
    #[serde(default = "default_wildcard_channel")]
    pub channel: String,
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub sender: Option<String>,
    #[serde(alias = "agent")]
    pub agent_id: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub role_ids: Vec<String>,
    #[serde(default)]
    pub match_type: RouteMatchType,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteRules {
    #[serde(default = "default_router_agent_id", alias = "default_agent")]
    pub default_agent_id: String,
    #[serde(default)]
    pub rules: Vec<RouteRule>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Router {
    AgentId {
        agent_id: String,
    },
    RouteRules {
        #[serde(default = "default_router_agent_id", alias = "default_agent")]
        default_agent_id: String,
        #[serde(default)]
        rules: Vec<RouteRule>,
    },
}

impl Router {
    #[must_use]
    pub fn from_agent_id(agent_id: impl Into<String>) -> Option<Self> {
        let agent_id = agent_id.into();
        let agent_id = agent_id.trim();
        if agent_id.is_empty() {
            None
        } else {
            Some(Self::AgentId {
                agent_id: agent_id.to_string(),
            })
        }
    }

    #[must_use]
    pub fn route_rules(&self) -> Option<RouteRules> {
        match self {
            Self::AgentId { .. } => None,
            Self::RouteRules {
                default_agent_id,
                rules,
            } => Some(RouteRules {
                default_agent_id: default_agent_id.clone(),
                rules: rules.clone(),
            }),
        }
    }
}

fn default_wildcard_channel() -> String {
    "*".to_string()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessPolicyMode {
    #[default]
    Open,
    Allowlist,
    Denylist,
    Disabled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelAccessPolicy {
    #[serde(default)]
    pub mode: AccessPolicyMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denylist: Vec<String>,
}

impl ChannelAccessPolicy {
    /// Check whether a given identifier is permitted by this policy.
    #[must_use]
    pub fn is_allowed(&self, id: &str) -> bool {
        let id = id.trim().to_ascii_lowercase();
        match self.mode {
            AccessPolicyMode::Open => true,
            AccessPolicyMode::Disabled => false,
            AccessPolicyMode::Allowlist => self
                .allowlist
                .iter()
                .any(|entry| entry.trim().to_ascii_lowercase() == id),
            AccessPolicyMode::Denylist => !self
                .denylist
                .iter()
                .any(|entry| entry.trim().to_ascii_lowercase() == id),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub slug: String,
    pub name: String,
    pub enabled: bool,
    pub config: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<Router>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dm_policy: Option<ChannelAccessPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_policy: Option<ChannelAccessPolicy>,
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

fn parse_channel_config(content: &str) -> Result<ChannelConfig, serde_json::Error> {
    let mut raw = serde_json::from_str::<Value>(content)?;
    migrate_legacy_nested_enabled_field(&mut raw);
    migrate_legacy_router_fields(&mut raw);
    let mut config = serde_json::from_value::<ChannelConfig>(raw)?;
    normalize_config(&mut config);
    Ok(config)
}

fn migrate_legacy_nested_enabled_field(raw: &mut Value) {
    let Some(obj) = raw.as_object_mut() else {
        return;
    };
    let Some(config_obj) = obj.get_mut("config").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(enabled) = config_obj
        .remove("enabled")
        .and_then(|value| value.as_bool())
    else {
        return;
    };
    obj.insert("enabled".to_string(), Value::Bool(enabled));
}

fn migrate_legacy_router_fields(raw: &mut Value) {
    let Some(obj) = raw.as_object_mut() else {
        return;
    };

    if obj.get("router").is_some() {
        return;
    }

    let Some(agent_id) = obj
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };

    obj.insert(
        "router".to_string(),
        serde_json::to_value(Router::AgentId {
            agent_id: agent_id.to_string(),
        })
        .unwrap_or(Value::Null),
    );
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
        normalize_slug(prefix).or_else(|| normalize_slug(trimmed))
    } else {
        normalize_slug(trimmed)
    }
}

fn resolve_kind(raw_kind: &str, fallback: Option<&str>) -> String {
    normalize_slug(raw_kind)
        .or_else(|| fallback.and_then(kind_from_id_candidate))
        .unwrap_or_else(|| DEFAULT_CHANNEL_KIND.to_string())
}

fn resolve_name(raw_name: &str) -> String {
    normalize_channel_name(raw_name).unwrap_or_else(|| "Default".to_string())
}

fn resolve_slug(name: &str) -> String {
    normalize_slug(name).unwrap_or_else(|| "default".to_string())
}

fn resolve_id(slug: &str, kind: &str) -> String {
    format!("{kind}-{slug}")
}

fn normalize_config(config: &mut ChannelConfig) {
    let kind = resolve_kind(&config.kind, Some(&config.id));
    config.kind = kind.clone();
    config.name = resolve_name(&config.name);
    config.slug = resolve_slug(&config.name);
    config.id = resolve_id(&config.slug, &kind);
}

fn normalized_selector(selector: &str) -> String {
    normalize_slug(selector).unwrap_or_else(|| selector.trim().to_ascii_lowercase())
}

fn selector_matches(config: &ChannelConfig, selector: &str) -> bool {
    let selector = normalized_selector(selector);
    let id = normalized_selector(&config.id);
    let kind = normalized_selector(&config.kind);
    id == selector || kind == selector
}

fn id_matches(config: &ChannelConfig, channel_id: &str) -> bool {
    normalized_selector(&config.id) == normalized_selector(channel_id)
}

async fn find_channel_config_path(
    savfox_home: &PathBuf,
    predicate: impl Fn(&ChannelConfig) -> bool,
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
        let Ok(config) = parse_channel_config(&content) else {
            continue;
        };

        if predicate(&config) {
            return Ok(Some(path));
        }
    }

    Ok(None)
}

async fn find_channel_config_path_by_id(
    savfox_home: &PathBuf,
    channel_id: &str,
) -> std::io::Result<Option<PathBuf>> {
    find_channel_config_path(savfox_home, |c| id_matches(c, channel_id)).await
}

async fn find_channel_config_path_by_selector(
    savfox_home: &PathBuf,
    selector: &str,
) -> std::io::Result<Option<PathBuf>> {
    find_channel_config_path(savfox_home, |c| selector_matches(c, selector)).await
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
                Ok(content) => match parse_channel_config(&content) {
                    Ok(config) => {
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
    match parse_channel_config(&content) {
        Ok(config) => Ok(Some(config)),
        Err(e) => {
            warn!("Failed to parse channel config {}: {}", path.display(), e);
            Ok(None)
        }
    }
}

async fn get_channel_config_by_id(
    savfox_home: &PathBuf,
    channel_id: &str,
) -> std::io::Result<Option<ChannelConfig>> {
    let Some(path) = find_channel_config_path_by_id(savfox_home, channel_id).await? else {
        return Ok(None);
    };

    let content = tokio::fs::read_to_string(&path).await?;
    match parse_channel_config(&content) {
        Ok(config) => Ok(Some(config)),
        Err(e) => {
            warn!("Failed to parse channel config {}: {}", path.display(), e);
            Ok(None)
        }
    }
}

/// Re-save all existing channel configs so that the `slug` field is populated.
pub async fn migrate_channel_configs(savfox_home: &PathBuf) {
    let configs = match list_channel_configs(savfox_home).await {
        Ok(c) => c,
        Err(_) => return,
    };
    for config in configs {
        if !config.slug.is_empty() {
            continue;
        }
        if let Err(e) = save_channel_config(savfox_home, &config).await {
            warn!("Failed to migrate channel config {}: {}", config.id, e);
        }
    }
}

pub async fn save_channel_config(
    savfox_home: &PathBuf,
    config: &ChannelConfig,
) -> std::io::Result<()> {
    ensure_channels_dir(savfox_home).await?;

    let original_id = normalize_slug(&config.id);
    let mut normalized = config.clone();
    normalize_config(&mut normalized);

    let target_path = channels_dir(savfox_home).join(format!("{}.json", normalized.id.clone()));
    let mut stale_paths = std::collections::HashSet::<PathBuf>::new();
    if let Some(existing) = find_channel_config_path_by_id(savfox_home, &normalized.id).await?
        && existing != target_path
    {
        stale_paths.insert(existing);
    }
    if let Some(old_id) = original_id
        && normalized_selector(&old_id) != normalized_selector(&normalized.id)
        && let Some(previous) = find_channel_config_path_by_id(savfox_home, &old_id).await?
        && previous != target_path
    {
        stale_paths.insert(previous);
    }

    let content = serde_json::to_string_pretty(&normalized)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    tokio::fs::write(&target_path, content).await?;

    for existing in stale_paths {
        if let Err(err) = tokio::fs::remove_file(&existing).await {
            warn!(
                "Failed to remove old channel config {} after rename: {}",
                existing.display(),
                err
            );
        }
    }

    info!("Saved channel config: {}", target_path.display());
    Ok(())
}

pub async fn delete_channel_config(savfox_home: &PathBuf, selector: &str) -> std::io::Result<bool> {
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
    let lookup_id = patch
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(normalize_slug)
        .or_else(|| {
            patch.get("name").and_then(|v| v.as_str()).map(|name| {
                let slug = resolve_slug(&resolve_name(name));
                resolve_id(&slug, &kind)
            })
        })
        .unwrap_or_else(|| {
            let slug = resolve_slug(&resolve_name(channel_name));
            resolve_id(&slug, &kind)
        });
    let existing = get_channel_config_by_id(savfox_home, &lookup_id).await?;
    let now = chrono::Utc::now().timestamp();

    let initial_name = resolve_name(channel_name);
    let initial_slug = resolve_slug(&initial_name);
    let mut config = existing.unwrap_or(ChannelConfig {
        id: lookup_id,
        kind: kind.clone(),
        slug: initial_slug,
        name: initial_name,
        enabled: true,
        config: Value::Object(serde_json::Map::new()),
        router: None,
        dm_policy: None,
        group_policy: None,
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
                || key == "router"
                || key == "dm_policy"
                || key == "group_policy"
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
    if let Some(router_value) = patch.get("router") {
        if router_value.is_null() {
            config.router = None;
        } else {
            config.router = Some(
                serde_json::from_value::<Router>(router_value.clone()).map_err(|err| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("invalid channel router payload: {err}"),
                    )
                })?,
            );
        }
    } else if let Some(agent_id) = patch.get("agent_id").and_then(|v| v.as_str()) {
        config.router = Router::from_agent_id(agent_id);
    }
    if let Some(dm_policy_value) = patch.get("dm_policy") {
        if dm_policy_value.is_null() {
            config.dm_policy = None;
        } else {
            config.dm_policy = Some(
                serde_json::from_value::<ChannelAccessPolicy>(dm_policy_value.clone()).map_err(
                    |err| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            format!("invalid dm_policy payload: {err}"),
                        )
                    },
                )?,
            );
        }
    }
    if let Some(group_policy_value) = patch.get("group_policy") {
        if group_policy_value.is_null() {
            config.group_policy = None;
        } else {
            config.group_policy = Some(
                serde_json::from_value::<ChannelAccessPolicy>(group_policy_value.clone()).map_err(
                    |err| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            format!("invalid group_policy payload: {err}"),
                        )
                    },
                )?,
            );
        }
    }
    config.name = resolve_name(&config.name);
    config.slug = resolve_slug(&config.name);
    let resolved_id = resolve_id(&config.slug, &config.kind);
    if config.id.trim().is_empty() {
        config.id = resolved_id.clone();
    }
    if config.created_at.is_none() {
        config.created_at = Some(now);
    }
    config.updated_at = Some(now);

    save_channel_config(savfox_home, &config).await?;
    config.id = resolved_id;
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
    async fn channel_files_use_id_names_and_lookup_by_kind() {
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
            router: None,
            dm_policy: None,
            group_policy: None,
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
        assert_eq!(file_name, "matrix-matrix.json");

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
    async fn save_channel_renames_file_when_generated_id_changes() {
        let home =
            std::env::temp_dir().join(format!("savfox-channel-store-{}", uuid::Uuid::new_v4()));
        let channels = home.join(CHANNELS_SUBDIR);

        let first = ChannelConfig {
            id: String::new(),
            kind: "matrix".to_string(),
            name: "Primary Matrix".to_string(),
            enabled: true,
            config: json!({ "homeserver": "http://127.0.0.1:6006" }),
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: Some(1),
            updated_at: Some(1),
        };
        save_channel_config(&home, &first)
            .await
            .expect("save first");

        let mut files = list_json_files(&channels).await;
        assert_eq!(files.len(), 1);
        let first_name = files[0]
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        assert_eq!(first_name, "matrix-primary-matrix.json");

        let second = ChannelConfig {
            id: "matrix-primary-matrix".to_string(),
            kind: "matrix".to_string(),
            name: "Renamed Matrix".to_string(),
            enabled: true,
            config: json!({ "homeserver": "http://127.0.0.1:6006" }),
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: Some(1),
            updated_at: Some(2),
        };
        save_channel_config(&home, &second)
            .await
            .expect("save second");

        files = list_json_files(&channels).await;
        assert_eq!(files.len(), 1);
        let second_name = files[0]
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        assert_eq!(second_name, "matrix-renamed-matrix.json");

        let loaded = get_channel_config(&home, "matrix")
            .await
            .expect("lookup by kind")
            .expect("config should exist");
        assert_eq!(loaded.id, "matrix-renamed-matrix");
        assert_eq!(loaded.name, "Renamed Matrix");

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
    async fn merge_channel_allows_multiple_configs_for_same_kind() {
        let home =
            std::env::temp_dir().join(format!("savfox-channel-store-{}", uuid::Uuid::new_v4()));
        let channels = home.join(CHANNELS_SUBDIR);

        let first = merge_channel_config(
            &home,
            "Matrix",
            "Primary Matrix",
            &json!({ "homeserver": "http://127.0.0.1:6006", "groups": "!room-a:example.org" }),
        )
        .await
        .expect("merge first");
        let second = merge_channel_config(
            &home,
            "Matrix",
            "Backup Matrix",
            &json!({ "homeserver": "http://127.0.0.1:7007", "groups": "!room-b:example.org" }),
        )
        .await
        .expect("merge second");

        assert_ne!(first.id, second.id);

        let files = list_json_files(&channels).await;
        assert_eq!(files.len(), 2);

        let loaded_first = get_channel_config(&home, &first.id)
            .await
            .expect("get first")
            .expect("first exists");
        let loaded_second = get_channel_config(&home, &second.id)
            .await
            .expect("get second")
            .expect("second exists");
        assert_eq!(loaded_first.kind, "matrix");
        assert_eq!(loaded_second.kind, "matrix");

        let _ = tokio::fs::remove_dir_all(&home).await;
    }

    #[test]
    fn access_policy_open_allows_everything() {
        let policy = ChannelAccessPolicy {
            mode: AccessPolicyMode::Open,
            allowlist: vec![],
            denylist: vec![],
        };
        assert!(policy.is_allowed("anyone"));
        assert!(policy.is_allowed(""));
    }

    #[test]
    fn access_policy_disabled_blocks_everything() {
        let policy = ChannelAccessPolicy {
            mode: AccessPolicyMode::Disabled,
            allowlist: vec![],
            denylist: vec![],
        };
        assert!(!policy.is_allowed("anyone"));
    }

    #[test]
    fn access_policy_allowlist_permits_only_listed() {
        let policy = ChannelAccessPolicy {
            mode: AccessPolicyMode::Allowlist,
            allowlist: vec!["Alice".to_string(), "bob".to_string()],
            denylist: vec![],
        };
        assert!(policy.is_allowed("alice"));
        assert!(policy.is_allowed("Bob"));
        assert!(!policy.is_allowed("charlie"));
    }

    #[test]
    fn access_policy_denylist_blocks_listed() {
        let policy = ChannelAccessPolicy {
            mode: AccessPolicyMode::Denylist,
            allowlist: vec![],
            denylist: vec!["spammer".to_string()],
        };
        assert!(!policy.is_allowed("Spammer"));
        assert!(policy.is_allowed("normal_user"));
    }

    #[tokio::test]
    async fn merge_channel_config_persists_policies() {
        let home =
            std::env::temp_dir().join(format!("savfox-channel-store-{}", uuid::Uuid::new_v4()));
        let merged = merge_channel_config(
            &home,
            "telegram",
            "My Telegram",
            &json!({
                "bot_token": "123:ABC",
                "dm_policy": {
                    "mode": "allowlist",
                    "allowlist": ["user_a", "user_b"]
                },
                "group_policy": {
                    "mode": "disabled"
                }
            }),
        )
        .await
        .expect("merge");

        assert!(merged.dm_policy.is_some());
        let dm = merged.dm_policy.unwrap();
        assert_eq!(dm.mode, AccessPolicyMode::Allowlist);
        assert_eq!(dm.allowlist, vec!["user_a", "user_b"]);

        assert!(merged.group_policy.is_some());
        let grp = merged.group_policy.unwrap();
        assert_eq!(grp.mode, AccessPolicyMode::Disabled);

        let loaded = get_channel_config(&home, "telegram")
            .await
            .expect("lookup")
            .expect("exists");
        assert!(loaded.dm_policy.is_some());
        assert!(loaded.group_policy.is_some());

        let _ = tokio::fs::remove_dir_all(&home).await;
    }

    #[test]
    fn parse_channel_config_migrates_nested_enabled_flag() {
        let parsed = parse_channel_config(
            r#"{
                "id": "line-default",
                "kind": "line",
                "name": "Default",
                "enabled": true,
                "config": {
                    "enabled": false,
                    "channel_secret": "secret"
                }
            }"#,
        )
        .expect("parse");

        assert!(!parsed.enabled);
        assert_eq!(
            parsed.config["channel_secret"],
            Value::String("secret".to_string())
        );
        assert!(parsed.config.get("enabled").is_none());
    }
}
