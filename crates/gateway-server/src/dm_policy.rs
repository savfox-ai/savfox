//! DM pairing policies -- control who can DM the bot on each channel.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

/// DM policy mode
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DmPolicyMode {
    /// Unknown senders get a pairing code, must be approved
    Pairing,
    /// Anyone can DM
    Open,
    /// Only allowlisted users
    Closed,
}

impl Default for DmPolicyMode {
    fn default() -> Self {
        Self::Pairing
    }
}

/// Per-channel DM policy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelDmPolicy {
    pub mode: DmPolicyMode,
    #[serde(default)]
    pub allowlist: HashSet<String>,
    #[serde(default)]
    pub blocklist: HashSet<String>,
}

impl Default for ChannelDmPolicy {
    fn default() -> Self {
        Self {
            mode: DmPolicyMode::Pairing,
            allowlist: HashSet::new(),
            blocklist: HashSet::new(),
        }
    }
}

/// DM policy decision
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmDecision {
    Allow,
    RequirePairing,
    Block,
}

/// DM policy store
#[derive(Debug)]
pub struct DmPolicyStore {
    path: PathBuf,
    policies: RwLock<HashMap<String, ChannelDmPolicy>>,
    default_policy: DmPolicyMode,
}

impl DmPolicyStore {
    pub fn new(savfox_home: &Path) -> Self {
        let path = savfox_home.join("gateway").join("dm-policies.json");
        Self {
            path,
            policies: RwLock::new(HashMap::new()),
            default_policy: DmPolicyMode::Pairing,
        }
    }

    /// Load policies from disk
    pub async fn load(&self) {
        if let Ok(content) = tokio::fs::read_to_string(&self.path).await {
            if let Ok(policies) = serde_json::from_str::<HashMap<String, ChannelDmPolicy>>(&content)
            {
                *self.policies.write().await = policies;
            }
        }
    }

    /// Save policies to disk
    pub async fn save(&self) -> Result<(), String> {
        let policies = self.policies.read().await;
        let json = serde_json::to_string_pretty(&*policies)
            .map_err(|e| format!("serialize error: {e}"))?;
        if let Some(parent) = self.path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        tokio::fs::write(&self.path, json)
            .await
            .map_err(|e| format!("write error: {e}"))
    }

    /// Check if a sender is allowed to DM on a channel
    pub async fn check(&self, channel: &str, sender_id: &str) -> DmDecision {
        let policies = self.policies.read().await;
        let policy = policies.get(channel).cloned().unwrap_or(ChannelDmPolicy {
            mode: self.default_policy.clone(),
            ..Default::default()
        });

        // Always block blocklisted senders
        if policy.blocklist.contains(sender_id) {
            return DmDecision::Block;
        }

        // Always allow allowlisted senders
        if policy.allowlist.contains(sender_id) {
            return DmDecision::Allow;
        }

        match policy.mode {
            DmPolicyMode::Open => DmDecision::Allow,
            DmPolicyMode::Closed => DmDecision::Block,
            DmPolicyMode::Pairing => DmDecision::RequirePairing,
        }
    }

    /// Set policy for a channel
    pub async fn set_policy(&self, channel: String, policy: ChannelDmPolicy) {
        self.policies.write().await.insert(channel.clone(), policy);
        if let Err(e) = self.save().await {
            warn!(error = %e, "failed to save DM policies");
        }
        info!(channel = %channel, "DM policy updated");
    }

    /// Add sender to allowlist
    pub async fn allow_sender(&self, channel: &str, sender_id: String) {
        let mut policies = self.policies.write().await;
        let policy = policies.entry(channel.to_string()).or_default();
        policy.allowlist.insert(sender_id.clone());
        policy.blocklist.remove(&sender_id);
        drop(policies);
        let _ = self.save().await;
    }

    /// Add sender to blocklist
    pub async fn block_sender(&self, channel: &str, sender_id: String) {
        let mut policies = self.policies.write().await;
        let policy = policies.entry(channel.to_string()).or_default();
        policy.blocklist.insert(sender_id.clone());
        policy.allowlist.remove(&sender_id);
        drop(policies);
        let _ = self.save().await;
    }

    /// Get policy for a channel
    pub async fn get_policy(&self, channel: &str) -> ChannelDmPolicy {
        self.policies
            .read()
            .await
            .get(channel)
            .cloned()
            .unwrap_or_default()
    }

    /// List all policies
    pub async fn list_policies(&self) -> HashMap<String, ChannelDmPolicy> {
        self.policies.read().await.clone()
    }
}
