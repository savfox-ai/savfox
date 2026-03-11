use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A broadcast group allows sending messages to multiple channels at once.
/// Used for announcements, scheduled broadcasts, or coordinated auto-replies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BroadcastGroup {
    /// Unique group identifier.
    pub(crate) id: String,
    /// Human-readable name.
    pub(crate) name: String,
    /// Target channels to broadcast to.
    pub(crate) channels: Vec<BroadcastTarget>,
    /// Whether to send in sequence (with delays) or all at once.
    #[serde(default)]
    pub(crate) sequencing: SequencingMode,
    /// Whether this group is active.
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
}

fn default_true() -> bool {
    true
}

/// A single broadcast target channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BroadcastTarget {
    /// Channel identifier (e.g., "discord:general", "telegram:12345").
    pub(crate) channel_id: String,
    /// Optional per-channel message override template.
    pub(crate) template_override: Option<String>,
    /// Delay in seconds before sending to this target (for sequenced mode).
    #[serde(default)]
    pub(crate) delay_secs: u64,
}

/// How messages are sequenced across broadcast targets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SequencingMode {
    /// Send to all channels simultaneously.
    #[default]
    Parallel,
    /// Send to channels in order, respecting per-target delays.
    Sequential,
    /// Send with a fixed delay between each channel.
    Staggered {
        /// Delay in seconds between each channel.
        interval_secs: u64,
    },
}

/// Result of a broadcast operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BroadcastResult {
    /// Group that was broadcast to.
    pub(crate) group_id: String,
    /// Per-channel results.
    pub(crate) results: Vec<ChannelResult>,
    /// Number of successful deliveries.
    pub(crate) success_count: usize,
    /// Number of failed deliveries.
    pub(crate) failure_count: usize,
}

/// Result of sending to a single channel in a broadcast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChannelResult {
    pub(crate) channel_id: String,
    pub(crate) success: bool,
    pub(crate) error: Option<String>,
}

/// Manages broadcast groups and executes broadcasts.
#[derive(Debug)]
pub(crate) struct BroadcastManager {
    groups: HashMap<String, BroadcastGroup>,
}

impl BroadcastManager {
    pub(crate) fn new() -> Self {
        Self {
            groups: HashMap::new(),
        }
    }

    pub(crate) fn from_groups(groups: Vec<BroadcastGroup>) -> Self {
        let groups = groups.into_iter().map(|g| (g.id.clone(), g)).collect();
        Self { groups }
    }

    /// Add or update a broadcast group.
    pub(crate) fn upsert_group(&mut self, group: BroadcastGroup) {
        self.groups.insert(group.id.clone(), group);
    }

    /// Remove a broadcast group.
    pub(crate) fn remove_group(&mut self, id: &str) -> Option<BroadcastGroup> {
        self.groups.remove(id)
    }

    /// Get a group by ID.
    pub(crate) fn get_group(&self, id: &str) -> Option<&BroadcastGroup> {
        self.groups.get(id)
    }

    /// List all groups.
    pub(crate) fn list_groups(&self) -> Vec<&BroadcastGroup> {
        self.groups.values().collect()
    }

    /// Prepare a broadcast: resolve the group and return the target list
    /// with any template overrides applied.
    pub(crate) fn resolve_targets(
        &self,
        group_id: &str,
        default_message: &str,
    ) -> Option<Vec<ResolvedTarget>> {
        let group = self.groups.get(group_id)?;
        if !group.enabled {
            return None;
        }

        let targets = group
            .channels
            .iter()
            .map(|t| ResolvedTarget {
                channel_id: t.channel_id.clone(),
                message: t
                    .template_override
                    .as_deref()
                    .unwrap_or(default_message)
                    .to_string(),
                delay_secs: t.delay_secs,
            })
            .collect();

        Some(targets)
    }
}

/// A resolved broadcast target with the final message to send.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedTarget {
    pub(crate) channel_id: String,
    pub(crate) message: String,
    pub(crate) delay_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_group() -> BroadcastGroup {
        BroadcastGroup {
            id: "announcements".to_string(),
            name: "Announcements".to_string(),
            channels: vec![
                BroadcastTarget {
                    channel_id: "discord:general".to_string(),
                    template_override: None,
                    delay_secs: 0,
                },
                BroadcastTarget {
                    channel_id: "telegram:12345".to_string(),
                    template_override: Some("📢 {{message}}".to_string()),
                    delay_secs: 5,
                },
                BroadcastTarget {
                    channel_id: "slack:C123".to_string(),
                    template_override: None,
                    delay_secs: 0,
                },
            ],
            sequencing: SequencingMode::Parallel,
            enabled: true,
        }
    }

    #[test]
    fn test_broadcast_manager_crud() {
        let mut mgr = BroadcastManager::new();
        assert!(mgr.list_groups().is_empty());

        let group = test_group();
        mgr.upsert_group(group.clone());
        assert_eq!(mgr.list_groups().len(), 1);

        let fetched = mgr.get_group("announcements").unwrap();
        assert_eq!(fetched.name, "Announcements");
        assert_eq!(fetched.channels.len(), 3);

        let removed = mgr.remove_group("announcements");
        assert!(removed.is_some());
        assert!(mgr.list_groups().is_empty());
    }

    #[test]
    fn test_resolve_targets() {
        let mut mgr = BroadcastManager::new();
        mgr.upsert_group(test_group());

        let targets = mgr
            .resolve_targets("announcements", "Hello everyone!")
            .unwrap();
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].message, "Hello everyone!");
        assert_eq!(targets[1].message, "📢 {{message}}");
        assert_eq!(targets[2].message, "Hello everyone!");
    }

    #[test]
    fn test_disabled_group() {
        let mut mgr = BroadcastManager::new();
        let mut group = test_group();
        group.enabled = false;
        mgr.upsert_group(group);

        assert!(mgr.resolve_targets("announcements", "test").is_none());
    }

    #[test]
    fn test_nonexistent_group() {
        let mgr = BroadcastManager::new();
        assert!(mgr.resolve_targets("missing", "test").is_none());
    }
}
