//! Auth profiles for managing multiple API key configurations.
//!
//! Allows rotating between different API keys per provider,
//! useful for load balancing, rate limit distribution, or
//! using different accounts for different tasks.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A collection of auth profiles for API key management.
#[derive(Debug)]
pub struct AuthProfiles {
    profiles: HashMap<String, ProviderProfile>,
}

/// Auth configuration for a specific provider.
#[derive(Debug)]
pub struct ProviderProfile {
    /// Provider identifier (e.g., "openai", "anthropic").
    pub provider: String,
    /// Available API keys for this provider.
    keys: Vec<ApiKey>,
    /// Rotation strategy.
    strategy: RotationStrategy,
    /// Current index for round-robin.
    current_index: AtomicUsize,
}

/// A single API key entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    /// Key identifier (for logging, not the actual key).
    pub id: String,
    /// The actual API key value.
    pub key: String,
    /// Optional organization ID.
    pub org_id: Option<String>,
    /// Whether this key is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Rate limit remaining (updated at runtime).
    #[serde(skip)]
    pub rate_limit_remaining: Option<u32>,
}

fn default_true() -> bool {
    true
}

/// Strategy for rotating between API keys.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RotationStrategy {
    /// Use keys in round-robin order.
    #[default]
    RoundRobin,
    /// Always use the first available key.
    Primary,
    /// Use the key with the most remaining rate limit.
    LeastUsed,
    /// Random selection.
    Random,
}

/// Auth profiles configuration (for deserialization).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProfilesConfig {
    #[serde(default)]
    pub providers: HashMap<String, ProviderProfileConfig>,
}

/// Per-provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderProfileConfig {
    pub keys: Vec<ApiKey>,
    #[serde(default)]
    pub strategy: RotationStrategy,
}

impl AuthProfiles {
    /// Create from configuration.
    pub fn from_config(config: AuthProfilesConfig) -> Self {
        let profiles = config
            .providers
            .into_iter()
            .map(|(provider, cfg)| {
                let profile = ProviderProfile {
                    provider: provider.clone(),
                    keys: cfg.keys,
                    strategy: cfg.strategy,
                    current_index: AtomicUsize::new(0),
                };
                (provider, profile)
            })
            .collect();

        Self { profiles }
    }

    /// Create empty profiles.
    pub fn empty() -> Self {
        Self {
            profiles: HashMap::new(),
        }
    }

    /// Get the next API key for a provider.
    pub fn get_key(&self, provider: &str) -> Option<&ApiKey> {
        let profile = self.profiles.get(provider)?;
        let enabled_keys: Vec<&ApiKey> = profile.keys.iter().filter(|k| k.enabled).collect();

        if enabled_keys.is_empty() {
            return None;
        }

        match profile.strategy {
            RotationStrategy::RoundRobin => {
                let idx = profile.current_index.fetch_add(1, Ordering::Relaxed);
                Some(enabled_keys[idx % enabled_keys.len()])
            }
            RotationStrategy::Primary => enabled_keys.first().copied(),
            RotationStrategy::LeastUsed => {
                // Pick the key with highest remaining rate limit
                enabled_keys
                    .iter()
                    .max_by_key(|k| k.rate_limit_remaining.unwrap_or(u32::MAX))
                    .copied()
            }
            RotationStrategy::Random => {
                use std::time::{SystemTime, UNIX_EPOCH};
                let seed = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos() as usize;
                Some(enabled_keys[seed % enabled_keys.len()])
            }
        }
    }

    /// Update rate limit info for a key after an API response.
    pub fn update_rate_limit(&mut self, provider: &str, key_id: &str, remaining: u32) {
        if let Some(profile) = self.profiles.get_mut(provider) {
            if let Some(key) = profile.keys.iter_mut().find(|k| k.id == key_id) {
                key.rate_limit_remaining = Some(remaining);
            }
        }
    }

    /// Disable a key (e.g., if it's been revoked or quota exceeded).
    pub fn disable_key(&mut self, provider: &str, key_id: &str) {
        if let Some(profile) = self.profiles.get_mut(provider) {
            if let Some(key) = profile.keys.iter_mut().find(|k| k.id == key_id) {
                key.enabled = false;
            }
        }
    }

    /// List all providers with key counts.
    pub fn list_providers(&self) -> Vec<(&str, usize, usize)> {
        self.profiles
            .iter()
            .map(|(name, profile)| {
                let total = profile.keys.len();
                let enabled = profile.keys.iter().filter(|k| k.enabled).count();
                (name.as_str(), total, enabled)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AuthProfilesConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderProfileConfig {
                keys: vec![
                    ApiKey {
                        id: "key-1".to_string(),
                        key: "sk-test1".to_string(),
                        org_id: None,
                        enabled: true,
                        rate_limit_remaining: None,
                    },
                    ApiKey {
                        id: "key-2".to_string(),
                        key: "sk-test2".to_string(),
                        org_id: None,
                        enabled: true,
                        rate_limit_remaining: None,
                    },
                ],
                strategy: RotationStrategy::RoundRobin,
            },
        );
        AuthProfilesConfig { providers }
    }

    #[test]
    fn test_round_robin() {
        let profiles = AuthProfiles::from_config(test_config());

        let key1 = profiles.get_key("openai").unwrap();
        let key2 = profiles.get_key("openai").unwrap();

        // Should alternate
        assert_ne!(key1.id, key2.id);
    }

    #[test]
    fn test_missing_provider() {
        let profiles = AuthProfiles::from_config(test_config());
        assert!(profiles.get_key("nonexistent").is_none());
    }

    #[test]
    fn test_disable_key() {
        let mut profiles = AuthProfiles::from_config(test_config());
        profiles.disable_key("openai", "key-1");

        let key = profiles.get_key("openai").unwrap();
        assert_eq!(key.id, "key-2");
    }

    #[test]
    fn test_list_providers() {
        let profiles = AuthProfiles::from_config(test_config());
        let list = profiles.list_providers();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].1, 2); // total keys
        assert_eq!(list[0].2, 2); // enabled keys
    }
}
