use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::package::{SkillManifest, SkillPackage, SkillSource, SkillSourceType};

const DEFAULT_INDEX_URL: &str = "https://skills.savfox.ai/index.json";
const INDEX_CACHE_TTL_SECS: u64 = 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteIndex {
    pub version: String,
    pub updated_at: String,
    pub skills: Vec<IndexEntry>,
    #[serde(default)]
    pub categories: Vec<CategoryInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub short_description: Option<String>,
    pub author: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    pub download_url: String,
    pub checksum: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub min_host_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IndexCache {
    index: Option<RemoteIndex>,
    fetched_at: Option<std::time::Instant>,
}

impl Default for IndexCache {
    fn default() -> Self {
        Self {
            index: None,
            fetched_at: None,
        }
    }
}

#[derive(Debug)]
pub struct RemoteIndexClient {
    client: Client,
    index_url: String,
    cache: IndexCache,
}

impl RemoteIndexClient {
    pub fn new() -> Self {
        Self::with_url(DEFAULT_INDEX_URL.to_string())
    }

    pub fn with_url(index_url: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(format!(
                "savfox-skill-registry/{}",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .unwrap_or_default();

        Self {
            client,
            index_url,
            cache: IndexCache::default(),
        }
    }

    pub async fn fetch_index(&mut self, force_refresh: bool) -> anyhow::Result<RemoteIndex> {
        if !force_refresh {
            if let Some(ref index) = self.cache.index {
                if let Some(fetched_at) = self.cache.fetched_at {
                    if fetched_at.elapsed().as_secs() < INDEX_CACHE_TTL_SECS {
                        return Ok(index.clone());
                    }
                }
            }
        }

        info!(url = %self.index_url, "fetching skill index");
        let response = self.client.get(&self.index_url).send().await?;
        let index: RemoteIndex = response.json().await?;

        self.cache.index = Some(index.clone());
        self.cache.fetched_at = Some(std::time::Instant::now());

        info!(count = index.skills.len(), "fetched skill index");
        Ok(index)
    }

    pub fn cached_index(&self) -> Option<&RemoteIndex> {
        self.cache.index.as_ref()
    }

    pub fn search<'a>(&self, query: &str, index: &'a RemoteIndex) -> Vec<&'a IndexEntry> {
        let query_lower = query.to_lowercase();
        index
            .skills
            .iter()
            .filter(|skill| {
                skill.name.to_lowercase().contains(&query_lower)
                    || skill.description.to_lowercase().contains(&query_lower)
                    || skill
                        .keywords
                        .iter()
                        .any(|k| k.to_lowercase().contains(&query_lower))
            })
            .collect()
    }

    pub fn by_category<'a>(&self, category: &str, index: &'a RemoteIndex) -> Vec<&'a IndexEntry> {
        index
            .skills
            .iter()
            .filter(|skill| skill.categories.iter().any(|c| c == category))
            .collect()
    }

    pub fn entry_to_package(&self, entry: &IndexEntry) -> SkillPackage {
        let version = semver::Version::parse(&entry.version).ok();
        SkillPackage {
            manifest: SkillManifest {
                name: entry.name.clone(),
                version: version.clone().unwrap_or(semver::Version::new(0, 0, 0)),
                description: entry.description.clone(),
                short_description: entry.short_description.clone(),
                author: entry.author.clone(),
                keywords: entry.keywords.clone(),
                categories: entry.categories.clone(),
                min_host_version: entry
                    .min_host_version
                    .as_ref()
                    .and_then(|v| semver::Version::parse(v).ok()),
                ..Default::default()
            },
            source: SkillSource {
                source_type: SkillSourceType::Registry,
                url: Some(entry.download_url.clone()),
                checksum: Some(entry.checksum.clone()),
                registry: Some("default".to_string()),
                path: None,
            },
            installed: false,
            installed_version: None,
            install_path: None,
        }
    }
}

impl Default for RemoteIndexClient {
    fn default() -> Self {
        Self::new()
    }
}

pub fn get_builtin_skills() -> Vec<SkillPackage> {
    vec![
        create_builtin(
            "github",
            "1.0.0",
            "GitHub integration - create issues, pull requests, manage repositories",
            Some("Work with GitHub"),
            vec!["github", "git", "code-review"],
            vec!["developer-tools"],
        ),
        create_builtin(
            "weather",
            "1.0.0",
            "Get weather information for any location",
            Some("Check weather"),
            vec!["weather", "forecast"],
            vec!["information"],
        ),
        create_builtin(
            "spotify",
            "1.0.0",
            "Control Spotify playback and manage playlists",
            Some("Control Spotify"),
            vec!["spotify", "music", "playback"],
            vec!["entertainment"],
        ),
        create_builtin(
            "notion",
            "1.0.0",
            "Interact with Notion workspaces - pages, databases, and more",
            Some("Work with Notion"),
            vec!["notion", "notes", "productivity"],
            vec!["productivity"],
        ),
        create_builtin(
            "calendar",
            "1.0.0",
            "Manage calendar events and schedules",
            Some("Manage calendars"),
            vec!["calendar", "events", "schedule"],
            vec!["productivity"],
        ),
        create_builtin(
            "web-search",
            "1.0.0",
            "Search the web using various search engines",
            Some("Search the web"),
            vec!["search", "web", "google"],
            vec!["information"],
        ),
        create_builtin(
            "memory",
            "1.0.0",
            "Persistent memory storage for conversations and context",
            Some("Remember information"),
            vec!["memory", "context", "storage"],
            vec!["core"],
        ),
        create_builtin(
            "shell-commands",
            "1.0.0",
            "Execute shell commands with safety checks",
            Some("Run shell commands"),
            vec!["shell", "terminal", "commands"],
            vec!["developer-tools"],
        ),
    ]
}

fn create_builtin(
    name: &str,
    version: &str,
    description: &str,
    short_description: Option<&str>,
    keywords: Vec<&str>,
    categories: Vec<&str>,
) -> SkillPackage {
    SkillPackage {
        manifest: SkillManifest {
            name: name.to_string(),
            version: semver::Version::parse(version).unwrap(),
            description: description.to_string(),
            short_description: short_description.map(|s| s.to_string()),
            keywords: keywords.into_iter().map(|s| s.to_string()).collect(),
            categories: categories.into_iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
        source: SkillSource {
            source_type: SkillSourceType::Embedded,
            url: None,
            path: None,
            registry: None,
            checksum: None,
        },
        installed: false,
        installed_version: None,
        install_path: None,
    }
}
