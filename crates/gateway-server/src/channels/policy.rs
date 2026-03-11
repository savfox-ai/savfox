use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::DmScope;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct DmScopePolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default: Option<DmScope>,
    #[serde(default)]
    pub(crate) agents: HashMap<String, DmScope>,
    #[serde(default)]
    pub(crate) channels: HashMap<String, DmScope>,
}

impl DmScopePolicyConfig {
    pub(crate) fn normalize(&mut self) {
        self.agents = self
            .agents
            .drain()
            .map(|(key, value)| (key.trim().to_ascii_lowercase(), value))
            .collect();
        self.channels = self
            .channels
            .drain()
            .map(|(key, value)| (key.trim().to_ascii_lowercase(), value))
            .collect();
    }

    pub(crate) fn resolve(&self, agent_id: &str, channel: &str) -> Option<DmScope> {
        let channel = channel.trim().to_ascii_lowercase();
        if let Some(scope) = self.channels.get(&channel) {
            return Some(*scope);
        }
        if let Some((platform, _)) = channel.split_once(':')
            && let Some(scope) = self.channels.get(platform)
        {
            return Some(*scope);
        }
        let agent_id = agent_id.trim().to_ascii_lowercase();
        if let Some(scope) = self.agents.get(&agent_id) {
            return Some(*scope);
        }
        if let Some(scope) = self.channels.get("*") {
            return Some(*scope);
        }
        self.default
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct AgentTonePolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default: Option<String>,
    #[serde(default)]
    pub(crate) channels: HashMap<String, String>,
}

impl AgentTonePolicyConfig {
    pub(crate) fn normalize(&mut self) {
        self.default = normalize_tone_value(self.default.as_deref());
        self.channels = self
            .channels
            .drain()
            .filter_map(|(key, value)| {
                let key = key.trim().to_ascii_lowercase();
                normalize_tone_value(Some(value.as_str())).map(|tone| (key, tone))
            })
            .collect();
    }

    pub(crate) fn resolve(&self, channel: &str) -> Option<String> {
        if let Some(tone) = self.channels.get(channel) {
            return Some(tone.clone());
        }
        if let Some((platform, _)) = channel.split_once(':')
            && let Some(tone) = self.channels.get(platform)
        {
            return Some(tone.clone());
        }
        if let Some(tone) = self.channels.get("*") {
            return Some(tone.clone());
        }
        self.default.clone()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ChannelTonePolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default: Option<String>,
    #[serde(default)]
    pub(crate) channels: HashMap<String, String>,
    #[serde(default)]
    pub(crate) agents: HashMap<String, AgentTonePolicyConfig>,
}

impl ChannelTonePolicyConfig {
    pub(crate) fn normalize(&mut self) {
        self.default = normalize_tone_value(self.default.as_deref());
        self.channels = self
            .channels
            .drain()
            .filter_map(|(key, value)| {
                let key = key.trim().to_ascii_lowercase();
                normalize_tone_value(Some(value.as_str())).map(|tone| (key, tone))
            })
            .collect();
        self.agents = self
            .agents
            .drain()
            .filter_map(|(key, mut value)| {
                value.normalize();
                let key = key.trim().to_ascii_lowercase();
                if key.is_empty() {
                    None
                } else {
                    Some((key, value))
                }
            })
            .collect();
    }

    pub(crate) fn resolve(&self, agent_id: &str, channel: &str) -> Option<String> {
        let channel = channel.trim().to_ascii_lowercase();
        let agent = agent_id.trim().to_ascii_lowercase();

        if let Some(agent_cfg) = self.agents.get(&agent)
            && let Some(tone) = agent_cfg.resolve(&channel)
        {
            return Some(tone);
        }
        if let Some(tone) = self.channels.get(&channel) {
            return Some(tone.clone());
        }
        if let Some((platform, _)) = channel.split_once(':')
            && let Some(tone) = self.channels.get(platform)
        {
            return Some(tone.clone());
        }
        if let Some(tone) = self.channels.get("*") {
            return Some(tone.clone());
        }
        self.default.clone()
    }
}

pub(crate) fn dm_scope_policy_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("dm-scope.json")
}

pub(crate) fn parse_dm_scope(value: Option<&str>) -> Option<DmScope> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "main" => Some(DmScope::Main),
        "per_peer" => Some(DmScope::PerPeer),
        "per_channel_peer" => Some(DmScope::PerChannelPeer),
        "per_account_channel_peer" => Some(DmScope::PerAccountChannelPeer),
        _ => None,
    }
}

pub(crate) async fn load_dm_scope_policy(savfox_home: &Path) -> DmScopePolicyConfig {
    let path = dm_scope_policy_path(savfox_home);
    let mut cfg: DmScopePolicyConfig = crate::json_store::load_json(&path, "DM scope policy")
        .await
        .unwrap_or_default();
    cfg.normalize();
    cfg
}

pub(crate) async fn write_dm_scope_policy(
    savfox_home: &Path,
    policy: &DmScopePolicyConfig,
) -> anyhow::Result<()> {
    let path = dm_scope_policy_path(savfox_home);
    crate::json_store::save_json(&path, policy, "DM scope policy")
        .await
        .map_err(|e| anyhow::anyhow!(e))
}

pub(crate) async fn configured_dm_scope(
    savfox_home: &Path,
    agent_id: &str,
    channel: &str,
) -> DmScope {
    let policy = load_dm_scope_policy(savfox_home).await;
    if let Some(scope) = policy.resolve(agent_id, channel) {
        return scope;
    }
    let env_scope = std::env::var("SAVFOX_DM_SCOPE").ok();
    DmScope::from_str(env_scope.as_deref())
}

pub(crate) async fn load_channel_tone_policy(savfox_home: &Path) -> ChannelTonePolicyConfig {
    let path = savfox_home.join("channel-tone.json");
    let mut cfg: ChannelTonePolicyConfig =
        crate::json_store::load_json(&path, "channel tone policy")
            .await
            .unwrap_or_default();
    cfg.normalize();
    cfg
}

pub(crate) async fn configured_channel_tone_suffix(
    savfox_home: &Path,
    agent_id: &str,
    channel: &str,
) -> Option<String> {
    let policy = load_channel_tone_policy(savfox_home).await;
    policy.resolve(agent_id, channel)
}

pub(crate) fn append_channel_tone_suffix(prompt: &str, tone_suffix: Option<&str>) -> String {
    let Some(tone_suffix) = normalize_tone_value(tone_suffix) else {
        return prompt.to_string();
    };
    let trimmed_prompt = prompt.trim_end();
    if trimmed_prompt.is_empty() {
        format!("[system tone]\n{tone_suffix}")
    } else {
        format!(
            "{trimmed_prompt}\n\n[system tone]\nApply the following channel style guidance in your reply:\n{tone_suffix}"
        )
    }
}

fn normalize_tone_value(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        AgentTonePolicyConfig, ChannelTonePolicyConfig, DmScopePolicyConfig,
        append_channel_tone_suffix,
    };
    use crate::session::DmScope;

    #[test]
    fn dm_scope_policy_resolve_prefers_channel_then_agent_then_default() {
        let mut cfg = DmScopePolicyConfig {
            default: Some(DmScope::Main),
            agents: HashMap::from([("support".to_string(), DmScope::PerPeer)]),
            channels: HashMap::from([
                ("slack".to_string(), DmScope::PerChannelPeer),
                ("slack:c01".to_string(), DmScope::PerAccountChannelPeer),
            ]),
        };
        cfg.normalize();

        assert_eq!(
            cfg.resolve("support", "slack:C01"),
            Some(DmScope::PerAccountChannelPeer)
        );
        assert_eq!(
            cfg.resolve("support", "slack:C02"),
            Some(DmScope::PerChannelPeer)
        );
        assert_eq!(cfg.resolve("support", "telegram:1"), Some(DmScope::PerPeer));
        assert_eq!(cfg.resolve("other", "discord:1"), Some(DmScope::Main));
    }

    #[test]
    fn channel_tone_resolve_prefers_agent_channel_matrix() {
        let mut cfg = ChannelTonePolicyConfig {
            default: Some("default style".to_string()),
            channels: HashMap::from([("slack".to_string(), "formal".to_string())]),
            agents: HashMap::from([(
                "support".to_string(),
                AgentTonePolicyConfig {
                    default: Some("friendly".to_string()),
                    channels: HashMap::from([
                        ("discord".to_string(), "emoji".to_string()),
                        ("discord:123".to_string(), "very formal".to_string()),
                    ]),
                },
            )]),
        };
        cfg.normalize();

        assert_eq!(
            cfg.resolve("support", "discord:123"),
            Some("very formal".to_string())
        );
        assert_eq!(
            cfg.resolve("support", "discord:999"),
            Some("emoji".to_string())
        );
        assert_eq!(
            cfg.resolve("support", "telegram:1"),
            Some("friendly".to_string())
        );
        assert_eq!(
            cfg.resolve("other", "slack:C01"),
            Some("formal".to_string())
        );
        assert_eq!(
            cfg.resolve("other", "telegram:777"),
            Some("default style".to_string())
        );
    }

    #[test]
    fn append_channel_tone_suffix_is_noop_when_missing() {
        let prompt = "hello";
        assert_eq!(append_channel_tone_suffix(prompt, None), prompt);
    }

    #[test]
    fn append_channel_tone_suffix_injects_instruction() {
        let prompt = "Respond to the user.";
        let out = append_channel_tone_suffix(prompt, Some("Be more formal on Slack."));
        assert!(out.contains("Respond to the user."));
        assert!(out.contains("Be more formal on Slack."));
        assert!(out.contains("[system tone]"));
    }
}
