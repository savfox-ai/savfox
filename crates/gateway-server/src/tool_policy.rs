//! Tool policy management for controlling tool access per agent.
//!
//! Provides allowlist/denylist management, tool categories, and policy profiles.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Tool categories for policy grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Files,
    Runtime,
    Shell,
    Browsing,
    Media,
    AI,
    Models,
    System,
    Memory,
    Communication,
    External,
}

impl ToolCategory {
    pub fn all() -> &'static [ToolCategory] {
        &[
            ToolCategory::Files,
            ToolCategory::Runtime,
            ToolCategory::Shell,
            ToolCategory::Browsing,
            ToolCategory::Media,
            ToolCategory::AI,
            ToolCategory::Models,
            ToolCategory::System,
            ToolCategory::Memory,
            ToolCategory::Communication,
            ToolCategory::External,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            ToolCategory::Files => "Files",
            ToolCategory::Runtime => "Runtime",
            ToolCategory::Shell => "Shell",
            ToolCategory::Browsing => "Browsing",
            ToolCategory::Media => "Media",
            ToolCategory::AI => "AI",
            ToolCategory::Models => "Models",
            ToolCategory::System => "System",
            ToolCategory::Memory => "Memory",
            ToolCategory::Communication => "Communication",
            ToolCategory::External => "External",
        }
    }
}

/// Predefined tool policy profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicyProfile {
    /// Default policy: most tools allowed, dangerous ones require approval.
    Default,
    /// Restricted policy: only safe tools allowed.
    Restricted,
    /// Full access: all tools allowed without approval.
    Full,
}

impl ToolPolicyProfile {
    pub fn all() -> &'static [ToolPolicyProfile] {
        &[
            ToolPolicyProfile::Default,
            ToolPolicyProfile::Restricted,
            ToolPolicyProfile::Full,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            ToolPolicyProfile::Default => "Default",
            ToolPolicyProfile::Restricted => "Restricted",
            ToolPolicyProfile::Full => "Full Access",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            ToolPolicyProfile::Default => "Most tools allowed, dangerous ones require approval",
            ToolPolicyProfile::Restricted => "Only safe tools allowed (read-only, no shell)",
            ToolPolicyProfile::Full => "All tools allowed without approval",
        }
    }
}

/// Tool policy for an agent.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolPolicy {
    /// Policy profile to use as base.
    pub profile: Option<ToolPolicyProfile>,
    /// Tools that are always allowed (override denylist).
    #[serde(default)]
    pub allowlist: HashSet<String>,
    /// Tools that are always denied (override allowlist).
    #[serde(default)]
    pub denylist: HashSet<String>,
    /// Category-level overrides.
    #[serde(default)]
    pub category_overrides: HashMap<String, bool>,
    /// Whether approval is required for tools not in allowlist.
    #[serde(default)]
    pub require_approval: bool,
    /// Custom tool descriptions/overrides.
    #[serde(default)]
    pub tool_overrides: HashMap<String, ToolOverride>,
}

/// Per-tool override settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOverride {
    pub enabled: Option<bool>,
    pub require_approval: Option<bool>,
    pub description: Option<String>,
    pub category: Option<ToolCategory>,
}

impl ToolPolicy {
    pub fn new(profile: ToolPolicyProfile) -> Self {
        let require_approval = profile != ToolPolicyProfile::Full;
        Self {
            profile: Some(profile),
            require_approval,
            ..Default::default()
        }
    }

    pub fn from_profile(profile: ToolPolicyProfile) -> Self {
        let mut policy = Self::new(profile);
        match profile {
            ToolPolicyProfile::Default => {
                // Default: allow safe tools, restrict dangerous ones
                policy.denylist = [
                    "shell.execute",
                    "shell.run",
                    "system.reboot",
                    "system.shutdown",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect();
            }
            ToolPolicyProfile::Restricted => {
                // Restricted: only allow read-only tools
                policy.allowlist = [
                    "files.read",
                    "files.list",
                    "web.fetch",
                    "web.search",
                    "memory.get",
                    "memory.list",
                    "models.list",
                    "models.resolve",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect();
                policy.require_approval = true;
            }
            ToolPolicyProfile::Full => {
                // Full: allow everything
                policy.require_approval = false;
            }
        }
        policy
    }

    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        // Denylist takes precedence
        if self.denylist.contains(tool_name) {
            return false;
        }

        // Check allowlist
        if self.allowlist.contains(tool_name) {
            return true;
        }

        // Check category override
        let category = categorize_tool(tool_name);
        if let Some(allowed) = self.category_overrides.get(&category) {
            return *allowed;
        }

        // Default based on profile
        match self.profile {
            Some(ToolPolicyProfile::Full) => true,
            Some(ToolPolicyProfile::Restricted) => false,
            _ => true, // Default profile allows most tools
        }
    }

    pub fn requires_approval(&self, tool_name: &str) -> bool {
        if !self.require_approval {
            return false;
        }

        // Check tool override
        if let Some(override_) = self.tool_overrides.get(tool_name) {
            if let Some(req) = override_.require_approval {
                return req;
            }
        }

        // Check if in allowlist (no approval needed)
        if self.allowlist.contains(tool_name) {
            return false;
        }

        true
    }

    pub fn allow_tool(&mut self, tool_name: &str) {
        self.allowlist.insert(tool_name.to_string());
        self.denylist.remove(tool_name);
    }

    pub fn deny_tool(&mut self, tool_name: &str) {
        self.denylist.insert(tool_name.to_string());
        self.allowlist.remove(tool_name);
    }

    pub fn reset_tool(&mut self, tool_name: &str) {
        self.allowlist.remove(tool_name);
        self.denylist.remove(tool_name);
        self.tool_overrides.remove(tool_name);
    }
}

/// Categorize a tool based on its name.
pub fn categorize_tool(tool_name: &str) -> String {
    let category = if tool_name.starts_with("files.") || tool_name.starts_with("fs.") {
        ToolCategory::Files
    } else if tool_name.starts_with("shell.") || tool_name.starts_with("exec.") {
        ToolCategory::Shell
    } else if tool_name.starts_with("browser.")
        || tool_name.starts_with("web.")
        || tool_name.starts_with("selenium.")
    {
        ToolCategory::Browsing
    } else if tool_name.starts_with("image.")
        || tool_name.starts_with("audio.")
        || tool_name.starts_with("video.")
        || tool_name.starts_with("media.")
    {
        ToolCategory::Media
    } else if tool_name.starts_with("ai.")
        || tool_name.starts_with("llm.")
        || tool_name.starts_with("embedding.")
    {
        ToolCategory::AI
    } else if tool_name.starts_with("models.") || tool_name.starts_with("provider.") {
        ToolCategory::Models
    } else if tool_name.starts_with("system.") || tool_name.starts_with("config.") {
        ToolCategory::System
    } else if tool_name.starts_with("memory.") || tool_name.starts_with("remember.") {
        ToolCategory::Memory
    } else if tool_name.starts_with("send.")
        || tool_name.starts_with("notify.")
        || tool_name.starts_with("channel.")
    {
        ToolCategory::Communication
    } else if tool_name.starts_with("http.")
        || tool_name.starts_with("fetch.")
        || tool_name.starts_with("api.")
    {
        ToolCategory::External
    } else {
        ToolCategory::Runtime
    };

    serde_json::to_string(&category)
        .unwrap_or_else(|_| "runtime".to_string())
        .trim_matches('"')
        .to_string()
}

/// Store for tool policies.
pub struct ToolPolicyStore {
    home: std::path::PathBuf,
    cache: RwLock<Option<HashMap<String, ToolPolicy>>>,
}

impl ToolPolicyStore {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            cache: RwLock::new(None),
        }
    }

    fn policy_path(&self) -> std::path::PathBuf {
        self.home.join("tool-policies.json")
    }

    pub async fn load(&self) -> HashMap<String, ToolPolicy> {
        let cached = self.cache.read().await.clone();
        if let Some(policies) = cached {
            return policies;
        }

        let path = self.policy_path();
        let policies: HashMap<String, ToolPolicy> = match tokio::fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };

        *self.cache.write().await = Some(policies.clone());
        policies
    }

    pub async fn save(&self, policies: &HashMap<String, ToolPolicy>) -> Result<()> {
        let path = self.policy_path();
        let content = serde_json::to_string_pretty(policies)?;
        tokio::fs::write(&path, content).await?;
        *self.cache.write().await = Some(policies.clone());
        Ok(())
    }

    pub async fn get(&self, agent_id: &str) -> ToolPolicy {
        let policies = self.load().await;
        policies
            .get(agent_id)
            .cloned()
            .unwrap_or_else(|| ToolPolicy::from_profile(ToolPolicyProfile::Default))
    }

    pub async fn set(&self, agent_id: &str, policy: ToolPolicy) -> Result<()> {
        let mut policies = self.load().await;
        policies.insert(agent_id.to_string(), policy);
        self.save(&policies).await
    }

    pub async fn reset(&self, agent_id: &str) -> Result<()> {
        let mut policies = self.load().await;
        policies.remove(agent_id);
        self.save(&policies).await
    }

    pub async fn allow_tool(&self, agent_id: &str, tool_name: &str) -> Result<()> {
        let mut policy = self.get(agent_id).await;
        policy.allow_tool(tool_name);
        self.set(agent_id, policy).await
    }

    pub async fn deny_tool(&self, agent_id: &str, tool_name: &str) -> Result<()> {
        let mut policy = self.get(agent_id).await;
        policy.deny_tool(tool_name);
        self.set(agent_id, policy).await
    }

    pub async fn list_tools(&self, agent_id: &str) -> Result<Vec<ToolInfo>> {
        let policy = self.get(agent_id).await;

        // Built-in tools
        let built_in = vec![
            ("files.read", "Read file contents", ToolCategory::Files),
            ("files.write", "Write to file", ToolCategory::Files),
            ("files.list", "List directory contents", ToolCategory::Files),
            ("files.delete", "Delete file", ToolCategory::Files),
            (
                "shell.execute",
                "Execute shell command",
                ToolCategory::Shell,
            ),
            ("web.fetch", "Fetch web page", ToolCategory::Browsing),
            ("web.search", "Search the web", ToolCategory::Browsing),
            (
                "browser.navigate",
                "Navigate browser",
                ToolCategory::Browsing,
            ),
            ("browser.click", "Click element", ToolCategory::Browsing),
            (
                "browser.screenshot",
                "Take screenshot",
                ToolCategory::Browsing,
            ),
            ("memory.get", "Get memory entry", ToolCategory::Memory),
            ("memory.list", "List memory entries", ToolCategory::Memory),
            ("memory.set", "Set memory entry", ToolCategory::Memory),
            ("models.list", "List available models", ToolCategory::Models),
            (
                "models.resolve",
                "Resolve model alias",
                ToolCategory::Models,
            ),
            ("system.status", "Get system status", ToolCategory::System),
            ("config.get", "Get configuration", ToolCategory::System),
            (
                "send.message",
                "Send message to channel",
                ToolCategory::Communication,
            ),
        ];

        let mut tools: Vec<ToolInfo> = built_in
            .into_iter()
            .map(|(name, desc, cat)| {
                let allowed = policy.is_tool_allowed(name);
                let requires_approval = policy.requires_approval(name);
                ToolInfo {
                    name: name.to_string(),
                    description: desc.to_string(),
                    category: cat,
                    allowed,
                    requires_approval,
                }
            })
            .collect();

        // Sort by name
        tools.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(tools)
    }
}

/// Information about a tool for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub category: ToolCategory,
    pub allowed: bool,
    pub requires_approval: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = ToolPolicy::from_profile(ToolPolicyProfile::Default);

        assert!(policy.is_tool_allowed("files.read"));
        assert!(!policy.is_tool_allowed("shell.execute"));
    }

    #[test]
    fn test_restricted_policy() {
        let policy = ToolPolicy::from_profile(ToolPolicyProfile::Restricted);

        assert!(policy.is_tool_allowed("files.read"));
        assert!(!policy.is_tool_allowed("files.write"));
        assert!(!policy.is_tool_allowed("shell.execute"));
    }

    #[test]
    fn test_full_policy() {
        let policy = ToolPolicy::from_profile(ToolPolicyProfile::Full);

        assert!(policy.is_tool_allowed("files.read"));
        assert!(policy.is_tool_allowed("shell.execute"));
        assert!(!policy.requires_approval("shell.execute"));
    }

    #[test]
    fn test_allowlist_override() {
        let mut policy = ToolPolicy::from_profile(ToolPolicyProfile::Default);
        policy.allow_tool("shell.execute");

        assert!(policy.is_tool_allowed("shell.execute"));
    }

    #[test]
    fn test_denylist_override() {
        let mut policy = ToolPolicy::from_profile(ToolPolicyProfile::Full);
        policy.deny_tool("shell.execute");

        assert!(!policy.is_tool_allowed("shell.execute"));
    }

    #[test]
    fn test_categorize_tool() {
        assert_eq!(categorize_tool("files.read"), "files");
        assert_eq!(categorize_tool("shell.execute"), "shell");
        assert_eq!(categorize_tool("web.fetch"), "browsing");
        assert_eq!(categorize_tool("memory.get"), "memory");
    }
}
