//! Agent routing -- maps incoming messages from channels/senders to specific agents.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// A routing rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    /// Channel type to match (e.g., "discord", "telegram", "*" for any)
    #[serde(default = "default_wildcard")]
    pub channel: String,
    /// Channel-specific filter (server ID, group name, etc.)
    #[serde(default)]
    pub filter: Option<String>,
    /// Sender ID pattern (exact or prefix match)
    #[serde(default)]
    pub sender: Option<String>,
    /// Target agent name
    pub agent: String,
    /// Priority (higher = checked first)
    #[serde(default)]
    pub priority: i32,
    /// Discord guild/server ID for guild-specific routing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild_id: Option<String>,
    /// Team ID (Slack workspace, MS Teams tenant, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// Account ID for account-scoped routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Role names/IDs required to match this rule (preferred field).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    /// Discord role IDs required to match this rule
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub role_ids: Vec<String>,
    /// Match type: how the rule selects its target
    #[serde(default)]
    pub match_type: RoutingMatchType,
}

fn default_wildcard() -> String {
    "*".to_string()
}

/// How the routing rule matches messages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMatchType {
    /// Match by peer/sender ID (default)
    #[default]
    Peer,
    /// Match by parent thread peer/sender ID.
    ParentPeer,
    /// Match by guild/server
    Guild,
    /// Match by guild + required roles
    GuildRoles,
    /// Match by team/workspace
    Team,
    /// Match by account
    Account,
    /// Match by channel
    Channel,
    /// Default fallback
    Default,
}

/// Routing context for matching
#[derive(Debug, Clone)]
pub struct RoutingContext {
    pub channel: String,
    pub channel_id: Option<String>,
    pub sender_id: Option<String>,
    pub group_id: Option<String>,
    /// Guild/server ID (Discord).
    pub guild_id: Option<String>,
    /// Team/workspace ID (Slack, MS Teams).
    pub team_id: Option<String>,
    /// Account ID (multi-account bridge contexts).
    pub account_id: Option<String>,
    /// Role IDs the sender has (Discord).
    pub role_ids: Vec<String>,
    /// Parent channel for thread-based routing.
    pub parent_channel_id: Option<String>,
    /// Parent sender for thread reply inheritance routing.
    pub parent_sender_id: Option<String>,
    #[allow(dead_code)]
    pub is_dm: bool,
}

/// Agent router
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRouter {
    rules: Vec<RoutingRule>,
    #[serde(default = "default_agent")]
    default_agent: String,
}

fn default_agent() -> String {
    "default".to_string()
}

impl Default for AgentRouter {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            default_agent: "default".to_string(),
        }
    }
}

impl AgentRouter {
    pub fn new(rules: Vec<RoutingRule>, default_agent: String) -> Self {
        let mut router = Self {
            rules,
            default_agent,
        };
        router.rules.sort_by(|a, b| b.priority.cmp(&a.priority)); // highest first
        router
    }

    /// Resolve which agent should handle a message
    pub fn resolve(&self, ctx: &RoutingContext) -> String {
        let chain = [
            RoutingMatchType::Peer,
            RoutingMatchType::ParentPeer,
            RoutingMatchType::GuildRoles,
            RoutingMatchType::Guild,
            RoutingMatchType::Team,
            RoutingMatchType::Account,
            RoutingMatchType::Channel,
            RoutingMatchType::Default,
        ];

        for match_type in chain {
            for rule in &self.rules {
                if rule.match_type == match_type && self.matches_rule(rule, ctx) {
                    return rule.agent.clone();
                }
            }
        }

        // Fallback for legacy rules that did not specify a match type.
        for rule in &self.rules {
            if self.matches_rule(rule, ctx) {
                return rule.agent.clone();
            }
        }

        self.default_agent.clone()
    }

    fn matches_rule(&self, rule: &RoutingRule, ctx: &RoutingContext) -> bool {
        // Channel match
        if rule.channel != "*" && rule.channel != ctx.channel {
            return false;
        }

        // Filter match (channel-specific ID)
        if let Some(filter) = &rule.filter {
            let matches = ctx.channel_id.as_deref() == Some(filter.as_str())
                || ctx.group_id.as_deref() == Some(filter.as_str());
            if !matches {
                return false;
            }
        }

        self.matches_by_match_type(rule, ctx)
    }

    fn matches_by_match_type(&self, rule: &RoutingRule, ctx: &RoutingContext) -> bool {
        match rule.match_type {
            RoutingMatchType::Peer => {
                if let Some(sender) = &rule.sender {
                    ctx.sender_id.as_deref() == Some(sender.as_str())
                        || ctx
                            .sender_id
                            .as_deref()
                            .is_some_and(|id| id.starts_with(sender))
                } else {
                    true
                }
            }
            RoutingMatchType::ParentPeer => {
                if let Some(sender) = &rule.sender {
                    ctx.parent_sender_id.as_deref() == Some(sender.as_str())
                        || ctx
                            .parent_sender_id
                            .as_deref()
                            .is_some_and(|id| id.starts_with(sender))
                } else {
                    ctx.parent_sender_id.is_some()
                }
            }
            RoutingMatchType::Guild => {
                if let Some(guild) = &rule.guild_id {
                    ctx.guild_id.as_deref() == Some(guild.as_str())
                } else {
                    false
                }
            }
            RoutingMatchType::GuildRoles => {
                let Some(guild) = &rule.guild_id else {
                    return false;
                };
                if ctx.guild_id.as_deref() != Some(guild.as_str()) {
                    return false;
                }
                let required_roles = Self::effective_roles(rule);
                if required_roles.is_empty() {
                    return false;
                }
                let actual_roles = ctx
                    .role_ids
                    .iter()
                    .map(|r| r.to_ascii_lowercase())
                    .collect::<HashSet<_>>();
                required_roles
                    .iter()
                    .any(|r| actual_roles.contains(&r.to_ascii_lowercase()))
            }
            RoutingMatchType::Team => {
                if let Some(team) = &rule.team_id {
                    ctx.team_id.as_deref() == Some(team.as_str())
                } else {
                    false
                }
            }
            RoutingMatchType::Account => {
                if let Some(account) = &rule.account_id {
                    ctx.account_id.as_deref() == Some(account.as_str())
                } else {
                    false
                }
            }
            RoutingMatchType::Channel => {
                let by_id = rule
                    .filter
                    .as_deref()
                    .and_then(|f| ctx.channel_id.as_deref().map(|id| id == f))
                    .unwrap_or(false);
                by_id || rule.channel == "*" || rule.channel == ctx.channel
            }
            RoutingMatchType::Default => true,
        }
    }

    fn effective_roles(rule: &RoutingRule) -> Vec<String> {
        if !rule.roles.is_empty() {
            rule.roles.clone()
        } else {
            rule.role_ids.clone()
        }
    }

    /// Extract Discord role IDs from raw metadata values.
    pub fn extract_discord_roles(raw_roles: &[String]) -> Vec<String> {
        raw_roles
            .iter()
            .flat_map(|s| s.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    /// Extract Slack user groups from raw metadata values.
    pub fn extract_slack_groups(raw_groups: &[String]) -> Vec<String> {
        raw_groups
            .iter()
            .flat_map(|s| s.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    /// Add a rule
    pub fn add_rule(&mut self, rule: RoutingRule) {
        self.rules.push(rule);
        self.rules.sort_by(|a, b| b.priority.cmp(&a.priority));
    }

    /// Remove rules matching an agent
    pub fn remove_rules_for_agent(&mut self, agent: &str) {
        self.rules.retain(|r| r.agent != agent);
    }

    /// List all rules
    pub fn rules(&self) -> &[RoutingRule] {
        &self.rules
    }

    /// Get default agent
    pub fn default_agent(&self) -> &str {
        &self.default_agent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_routing() {
        let router = AgentRouter::default();
        let ctx = RoutingContext {
            channel: "discord".to_string(),
            channel_id: None,
            sender_id: None,
            group_id: None,
            guild_id: None,
            team_id: None,
            account_id: None,
            role_ids: Vec::new(),
            parent_channel_id: None,
            parent_sender_id: None,
            is_dm: true,
        };
        assert_eq!(router.resolve(&ctx), "default");
    }

    #[test]
    fn test_channel_routing() {
        let router = AgentRouter::new(
            vec![RoutingRule {
                channel: "discord".to_string(),
                filter: None,
                sender: None,
                agent: "discord-agent".to_string(),
                priority: 0,
                guild_id: None,
                team_id: None,
                account_id: None,
                roles: Vec::new(),
                role_ids: Vec::new(),
                match_type: RoutingMatchType::default(),
            }],
            "default".to_string(),
        );
        let ctx = RoutingContext {
            channel: "discord".to_string(),
            channel_id: None,
            sender_id: None,
            group_id: None,
            guild_id: None,
            team_id: None,
            account_id: None,
            role_ids: Vec::new(),
            parent_channel_id: None,
            parent_sender_id: None,
            is_dm: true,
        };
        assert_eq!(router.resolve(&ctx), "discord-agent");
    }

    #[test]
    fn test_priority_routing() {
        let router = AgentRouter::new(
            vec![
                RoutingRule {
                    channel: "*".to_string(),
                    filter: None,
                    sender: None,
                    agent: "fallback".to_string(),
                    priority: 0,
                    guild_id: None,
                    team_id: None,
                    account_id: None,
                    roles: Vec::new(),
                    role_ids: Vec::new(),
                    match_type: RoutingMatchType::default(),
                },
                RoutingRule {
                    channel: "telegram".to_string(),
                    filter: None,
                    sender: None,
                    agent: "telegram-agent".to_string(),
                    priority: 10,
                    guild_id: None,
                    team_id: None,
                    account_id: None,
                    roles: Vec::new(),
                    role_ids: Vec::new(),
                    match_type: RoutingMatchType::default(),
                },
            ],
            "default".to_string(),
        );
        let ctx = RoutingContext {
            channel: "telegram".to_string(),
            channel_id: None,
            sender_id: None,
            group_id: None,
            guild_id: None,
            team_id: None,
            account_id: None,
            role_ids: Vec::new(),
            parent_channel_id: None,
            parent_sender_id: None,
            is_dm: false,
        };
        assert_eq!(router.resolve(&ctx), "telegram-agent");
    }

    #[test]
    fn test_routing_fallback_chain_with_roles() {
        let router = AgentRouter::new(
            vec![
                RoutingRule {
                    channel: "discord".to_string(),
                    filter: None,
                    sender: Some("u-peer".to_string()),
                    agent: "peer-agent".to_string(),
                    priority: 100,
                    guild_id: None,
                    team_id: None,
                    account_id: None,
                    roles: Vec::new(),
                    role_ids: Vec::new(),
                    match_type: RoutingMatchType::Peer,
                },
                RoutingRule {
                    channel: "discord".to_string(),
                    filter: None,
                    sender: Some("u-parent".to_string()),
                    agent: "parent-agent".to_string(),
                    priority: 90,
                    guild_id: None,
                    team_id: None,
                    account_id: None,
                    roles: Vec::new(),
                    role_ids: Vec::new(),
                    match_type: RoutingMatchType::ParentPeer,
                },
                RoutingRule {
                    channel: "discord".to_string(),
                    filter: None,
                    sender: None,
                    agent: "role-agent".to_string(),
                    priority: 80,
                    guild_id: Some("g1".to_string()),
                    team_id: None,
                    account_id: None,
                    roles: vec!["admin".to_string()],
                    role_ids: Vec::new(),
                    match_type: RoutingMatchType::GuildRoles,
                },
                RoutingRule {
                    channel: "discord".to_string(),
                    filter: None,
                    sender: None,
                    agent: "guild-agent".to_string(),
                    priority: 70,
                    guild_id: Some("g1".to_string()),
                    team_id: None,
                    account_id: None,
                    roles: Vec::new(),
                    role_ids: Vec::new(),
                    match_type: RoutingMatchType::Guild,
                },
                RoutingRule {
                    channel: "slack".to_string(),
                    filter: None,
                    sender: None,
                    agent: "team-agent".to_string(),
                    priority: 60,
                    guild_id: None,
                    team_id: Some("t1".to_string()),
                    account_id: None,
                    roles: Vec::new(),
                    role_ids: Vec::new(),
                    match_type: RoutingMatchType::Team,
                },
                RoutingRule {
                    channel: "*".to_string(),
                    filter: None,
                    sender: None,
                    agent: "account-agent".to_string(),
                    priority: 50,
                    guild_id: None,
                    team_id: None,
                    account_id: Some("acc1".to_string()),
                    roles: Vec::new(),
                    role_ids: Vec::new(),
                    match_type: RoutingMatchType::Account,
                },
                RoutingRule {
                    channel: "*".to_string(),
                    filter: None,
                    sender: None,
                    agent: "fallback-agent".to_string(),
                    priority: 1,
                    guild_id: None,
                    team_id: None,
                    account_id: None,
                    roles: Vec::new(),
                    role_ids: Vec::new(),
                    match_type: RoutingMatchType::Default,
                },
            ],
            "default".to_string(),
        );

        let mut ctx = RoutingContext {
            channel: "discord".to_string(),
            channel_id: Some("c1".to_string()),
            sender_id: Some("u-peer".to_string()),
            group_id: None,
            guild_id: Some("g1".to_string()),
            team_id: None,
            account_id: Some("acc1".to_string()),
            role_ids: vec!["admin".to_string()],
            parent_channel_id: None,
            parent_sender_id: Some("u-parent".to_string()),
            is_dm: false,
        };
        assert_eq!(router.resolve(&ctx), "peer-agent");

        ctx.sender_id = Some("u-other".to_string());
        assert_eq!(router.resolve(&ctx), "parent-agent");

        ctx.parent_sender_id = None;
        assert_eq!(router.resolve(&ctx), "role-agent");

        ctx.role_ids = vec!["viewer".to_string()];
        assert_eq!(router.resolve(&ctx), "guild-agent");

        ctx.channel = "slack".to_string();
        ctx.guild_id = None;
        ctx.team_id = Some("t1".to_string());
        assert_eq!(router.resolve(&ctx), "team-agent");

        ctx.team_id = None;
        assert_eq!(router.resolve(&ctx), "account-agent");

        ctx.account_id = None;
        assert_eq!(router.resolve(&ctx), "fallback-agent");
    }

    #[test]
    fn test_role_extractors() {
        let roles =
            AgentRouter::extract_discord_roles(&["admin,mod".to_string(), " ops ".to_string()]);
        assert_eq!(roles, vec!["admin", "mod", "ops"]);

        let groups =
            AgentRouter::extract_slack_groups(&["eng,qa".to_string(), "oncall".to_string()]);
        assert_eq!(groups, vec!["eng", "qa", "oncall"]);
    }
}
