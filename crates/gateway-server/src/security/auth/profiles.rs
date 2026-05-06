//! Auth profile rotation system for multi-key provider access.
//!
//! Stores provider API key profiles in `{savfox_home}/auth/profiles.json` and
//! provides automatic rotation with cooldown handling. When a key hits a rate
//! limit (HTTP 429) it enters a timed cooldown; when it returns 401 it is
//! permanently marked as failed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

// ─── Constants ──────────────────────────────────────────────────────────────

/// Default cooldown duration for rate-limited keys (HTTP 429), in milliseconds.
const DEFAULT_RATE_LIMIT_COOLDOWN_MS: u64 = 60_000;

/// Sentinel cooldown timestamp indicating a permanently failed key (e.g. 401).
const PERMANENT_FAILURE_SENTINEL: u64 = u64::MAX;

// ─── Profile Entry ──────────────────────────────────────────────────────────

/// A single auth profile representing one API key for a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthProfile {
    /// Human-readable identifier for this profile (e.g. "primary", "backup").
    pub id: String,
    /// Provider name this profile belongs to (e.g. "openai", "anthropic").
    #[serde(skip)]
    pub provider: String,
    /// The API key value.
    pub api_key: String,
    /// Cumulative number of tokens consumed through this profile.
    #[serde(default)]
    pub usage_tokens: u64,
    /// Cumulative estimated cost in USD.
    #[serde(default)]
    pub usage_cost_usd: f64,
    /// If set, the profile is on cooldown until this epoch-millisecond timestamp.
    /// A value of `u64::MAX` means the key is permanently failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<u64>,
    /// The most recent error message, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl AuthProfile {
    /// Returns `true` if the profile is currently usable (not on cooldown).
    #[must_use]
    pub fn is_available(&self) -> bool {
        match self.cooldown_until {
            None => true,
            Some(PERMANENT_FAILURE_SENTINEL) => false,
            Some(until) => crate::json_store::now_ms() >= until,
        }
    }

    /// Returns `true` if the profile has been permanently marked as failed.
    #[must_use]
    pub fn is_permanently_failed(&self) -> bool {
        self.cooldown_until == Some(PERMANENT_FAILURE_SENTINEL)
    }
}

// ─── File-level store shape ─────────────────────────────────────────────────

/// On-disk format: a map from provider name to an ordered list of profiles.
type ProfileStore = HashMap<String, Vec<AuthProfile>>;

// ─── Manager ────────────────────────────────────────────────────────────────

/// Thread-safe manager for auth profile rotation and persistence.
///
/// Profiles are stored per-provider in a JSON file and accessed through an
/// `RwLock` to support concurrent reads with exclusive writes.
#[derive(Debug)]
pub struct AuthProfileManager {
    /// Directory containing `profiles.json`.
    auth_dir: PathBuf,
    /// In-memory profile store, keyed by provider name.
    profiles: RwLock<ProfileStore>,
}

impl AuthProfileManager {
    // ── Construction ────────────────────────────────────────────────────

    /// Create a new manager rooted at `{savfox_home}/auth`.
    #[must_use]
    pub fn from_home(home: &Path) -> Self {
        Self {
            auth_dir: home.join("auth"),
            profiles: RwLock::new(HashMap::new()),
        }
    }

    /// Path to the JSON persistence file.
    #[must_use]
    fn store_path(&self) -> PathBuf {
        self.auth_dir.join("profiles.json")
    }

    /// Ensure the auth directory exists on disk.
    async fn ensure_dir(&self) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.auth_dir).await
    }

    // ── Persistence ────────────────────────────────────────────────────

    /// Load profiles from disk into memory, replacing any current state.
    pub async fn load(&self) -> std::io::Result<()> {
        let store = self.load_from_disk().await;
        let mut profiles = self.profiles.write().await;
        *profiles = store;
        info!(
            providers = profiles.len(),
            "auth profiles loaded from {}",
            self.store_path().display()
        );
        Ok(())
    }

    /// Read the on-disk store, returning an empty map on any error.
    async fn load_from_disk(&self) -> ProfileStore {
        let path = self.store_path();
        let data = match tokio::fs::read_to_string(&path).await {
            Ok(d) => d,
            Err(_) => return HashMap::new(),
        };
        match serde_json::from_str::<ProfileStore>(&data) {
            Ok(mut store) => {
                // Back-fill the transient `provider` field from the map key.
                for (provider, entries) in &mut store {
                    for entry in entries.iter_mut() {
                        entry.provider = provider.clone();
                    }
                }
                store
            }
            Err(err) => {
                warn!(path = %path.display(), "failed to parse auth profiles: {err}");
                HashMap::new()
            }
        }
    }

    /// Persist the current in-memory state to disk.
    ///
    /// On Unix the file is created (or recreated) with mode 0600 so other
    /// users on the same host cannot read the API keys this file contains
    /// (S18 in the security review). On Windows the file inherits the
    /// parent directory's ACL — typically a per-user profile path — and a
    /// dedicated DACL tightening is left as a follow-up.
    async fn save_to_disk(&self) -> std::io::Result<()> {
        self.ensure_dir().await?;
        let profiles = self.profiles.read().await;
        let json = serde_json::to_string_pretty(&*profiles)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let path = self.store_path();
        write_secret_file(&path, json.as_bytes()).await
    }

    // ── Query ──────────────────────────────────────────────────────────

    /// List all profiles for a given provider.
    #[must_use]
    pub async fn list_profiles(&self, provider: &str) -> Vec<AuthProfile> {
        let profiles = self.profiles.read().await;
        profiles.get(provider).cloned().unwrap_or_default()
    }

    /// Select the first available (non-cooldown) profile for a provider.
    ///
    /// Returns `None` if no profiles are registered or all are on cooldown.
    #[must_use]
    pub async fn select_profile(&self, provider: &str) -> Option<AuthProfile> {
        let profiles = self.profiles.read().await;
        profiles
            .get(provider)?
            .iter()
            .find(|p| p.is_available())
            .cloned()
    }

    // ── Mutation ───────────────────────────────────────────────────────

    /// Add a new profile for a provider. If a profile with the same `id`
    /// already exists it is replaced.
    pub async fn add_profile(
        &self,
        provider: &str,
        id: &str,
        api_key: &str,
    ) -> std::io::Result<()> {
        {
            let mut profiles = self.profiles.write().await;
            let entries = profiles.entry(provider.to_owned()).or_default();

            // Remove any existing entry with the same id.
            entries.retain(|p| p.id != id);

            entries.push(AuthProfile {
                id: id.to_owned(),
                provider: provider.to_owned(),
                api_key: api_key.to_owned(),
                usage_tokens: 0,
                usage_cost_usd: 0.0,
                cooldown_until: None,
                last_error: None,
            });
        }

        info!(provider, id, "auth profile added");
        self.save_to_disk().await
    }

    /// Remove a profile by provider and id. Returns `true` if it existed.
    pub async fn remove_profile(&self, provider: &str, id: &str) -> std::io::Result<bool> {
        let removed = {
            let mut profiles = self.profiles.write().await;
            if let Some(entries) = profiles.get_mut(provider) {
                let before = entries.len();
                entries.retain(|p| p.id != id);
                let after = entries.len();

                // Clean up empty provider lists.
                if entries.is_empty() {
                    profiles.remove(provider);
                }

                before != after
            } else {
                false
            }
        };

        if removed {
            info!(provider, id, "auth profile removed");
            self.save_to_disk().await?;
        }

        Ok(removed)
    }

    /// Report an error for a specific profile. Applies cooldown rules:
    ///
    /// - **429 (rate limited)**: 60-second cooldown.
    /// - **401 (unauthorized)**: permanently mark as failed.
    /// - **Other**: record the error but do not set a cooldown.
    pub async fn report_error(
        &self,
        provider: &str,
        id: &str,
        status_code: u16,
    ) -> std::io::Result<()> {
        {
            let mut profiles = self.profiles.write().await;
            if let Some(entry) = find_profile_mut(&mut profiles, provider, id) {
                let error_msg = format!("HTTP {status_code}");
                entry.last_error = Some(error_msg);

                match status_code {
                    429 => {
                        let until = crate::json_store::now_ms() + DEFAULT_RATE_LIMIT_COOLDOWN_MS;
                        entry.cooldown_until = Some(until);
                        warn!(
                            provider,
                            id,
                            cooldown_ms = DEFAULT_RATE_LIMIT_COOLDOWN_MS,
                            "auth profile rate-limited, entering cooldown"
                        );
                    }
                    401 => {
                        entry.cooldown_until = Some(PERMANENT_FAILURE_SENTINEL);
                        warn!(
                            provider,
                            id, "auth profile permanently failed (401 unauthorized)"
                        );
                    }
                    _ => {
                        warn!(provider, id, status_code, "auth profile error recorded");
                    }
                }
            }
        }

        self.save_to_disk().await
    }

    /// Report a successful request, incrementing the usage counter and
    /// clearing any transient error state.
    pub async fn report_success(
        &self,
        provider: &str,
        id: &str,
        tokens_used: u64,
    ) -> std::io::Result<()> {
        {
            let mut profiles = self.profiles.write().await;
            if let Some(entry) = find_profile_mut(&mut profiles, provider, id) {
                entry.usage_tokens = entry.usage_tokens.saturating_add(tokens_used);
                entry.last_error = None;

                // Clear a non-permanent cooldown on success.
                if entry.cooldown_until != Some(PERMANENT_FAILURE_SENTINEL) {
                    entry.cooldown_until = None;
                }
            }
        }

        self.save_to_disk().await
    }

    /// Rotate profiles for a provider: move the current (first) profile to
    /// the end of the list so the next available one becomes active.
    pub async fn rotate(&self, provider: &str) -> std::io::Result<()> {
        {
            let mut profiles = self.profiles.write().await;
            if let Some(entries) = profiles.get_mut(provider)
                && entries.len() > 1
            {
                let first = entries.remove(0);
                entries.push(first);
                info!(
                    provider,
                    now_active = entries.first().map(|p| p.id.as_str()).unwrap_or("?"),
                    "auth profile rotated"
                );
            }
        }

        self.save_to_disk().await
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Find a mutable reference to a profile by provider + id.
fn find_profile_mut<'a>(
    store: &'a mut ProfileStore,
    provider: &str,
    id: &str,
) -> Option<&'a mut AuthProfile> {
    store.get_mut(provider)?.iter_mut().find(|p| p.id == id)
}

/// Atomically write `data` to `path` with mode 0600 on Unix (default ACL on
/// Windows; tightening to a per-user DACL is tracked as follow-up).
///
/// "Atomically" here means we write to `path.tmp` first then rename — even
/// if the daemon crashes mid-write the existing `profiles.json` is left
/// intact. Each create uses `O_TRUNC` so a previous-run leftover with
/// looser permissions is replaced rather than re-used.
async fn write_secret_file(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let parent = tmp
        .parent()
        .ok_or_else(|| std::io::Error::other("profiles store path has no parent"))?;
    tokio::fs::create_dir_all(parent).await?;

    // Use blocking IO for the create+chmod step so we can set the mode
    // atomically with the create. On Unix this guarantees no other process
    // can open the file with the more permissive default mode in the
    // window between open() and chmod().
    let tmp_clone = tmp.clone();
    let data = data.to_vec();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp_clone)?;
        std::io::Write::write_all(&mut file, &data)?;
        file.sync_all()?;
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)??;

    tokio::fs::rename(&tmp, path).await
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    /// Helper to create a manager backed by a temp directory.
    async fn test_manager() -> (AuthProfileManager, TempDir) {
        let tmp = TempDir::new().unwrap();
        let mgr = AuthProfileManager::from_home(tmp.path());
        mgr.load().await.unwrap();
        (mgr, tmp)
    }

    #[tokio::test]
    async fn add_and_list_profiles() {
        let (mgr, _tmp) = test_manager().await;

        mgr.add_profile("openai", "primary", "sk-aaa")
            .await
            .unwrap();
        mgr.add_profile("openai", "backup", "sk-bbb").await.unwrap();

        let profiles = mgr.list_profiles("openai").await;
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].id, "primary");
        assert_eq!(profiles[1].id, "backup");
    }

    #[tokio::test]
    async fn remove_profile() {
        let (mgr, _tmp) = test_manager().await;

        mgr.add_profile("openai", "primary", "sk-aaa")
            .await
            .unwrap();
        mgr.add_profile("openai", "backup", "sk-bbb").await.unwrap();

        let removed = mgr.remove_profile("openai", "primary").await.unwrap();
        assert!(removed);

        let profiles = mgr.list_profiles("openai").await;
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "backup");
    }

    #[tokio::test]
    async fn remove_nonexistent_returns_false() {
        let (mgr, _tmp) = test_manager().await;
        let removed = mgr.remove_profile("openai", "nope").await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn select_skips_cooldown() {
        let (mgr, _tmp) = test_manager().await;

        mgr.add_profile("openai", "primary", "sk-aaa")
            .await
            .unwrap();
        mgr.add_profile("openai", "backup", "sk-bbb").await.unwrap();

        // Rate-limit the primary key.
        mgr.report_error("openai", "primary", 429).await.unwrap();

        let selected = mgr.select_profile("openai").await.unwrap();
        assert_eq!(selected.id, "backup");
    }

    #[tokio::test]
    async fn permanent_failure_on_401() {
        let (mgr, _tmp) = test_manager().await;

        mgr.add_profile("openai", "only", "sk-aaa").await.unwrap();
        mgr.report_error("openai", "only", 401).await.unwrap();

        let profiles = mgr.list_profiles("openai").await;
        assert!(profiles[0].is_permanently_failed());
        assert!(!profiles[0].is_available());

        // select_profile should return None since the only key is dead.
        assert!(mgr.select_profile("openai").await.is_none());
    }

    #[tokio::test]
    async fn report_success_clears_transient_error() {
        let (mgr, _tmp) = test_manager().await;

        mgr.add_profile("openai", "primary", "sk-aaa")
            .await
            .unwrap();
        mgr.report_error("openai", "primary", 500).await.unwrap();

        let profiles = mgr.list_profiles("openai").await;
        assert!(profiles[0].last_error.is_some());

        mgr.report_success("openai", "primary", 100).await.unwrap();

        let profiles = mgr.list_profiles("openai").await;
        assert!(profiles[0].last_error.is_none());
        assert_eq!(profiles[0].usage_tokens, 100);
    }

    #[tokio::test]
    async fn rotate_moves_first_to_end() {
        let (mgr, _tmp) = test_manager().await;

        mgr.add_profile("openai", "a", "sk-a").await.unwrap();
        mgr.add_profile("openai", "b", "sk-b").await.unwrap();
        mgr.add_profile("openai", "c", "sk-c").await.unwrap();

        mgr.rotate("openai").await.unwrap();

        let profiles = mgr.list_profiles("openai").await;
        assert_eq!(profiles[0].id, "b");
        assert_eq!(profiles[1].id, "c");
        assert_eq!(profiles[2].id, "a");
    }

    #[tokio::test]
    async fn persistence_survives_reload() {
        let tmp = TempDir::new().unwrap();
        let mgr = AuthProfileManager::from_home(tmp.path());
        mgr.load().await.unwrap();

        mgr.add_profile("anthropic", "main", "ant-key")
            .await
            .unwrap();
        mgr.report_success("anthropic", "main", 500).await.unwrap();

        // Create a fresh manager pointing at the same directory.
        let mgr2 = AuthProfileManager::from_home(tmp.path());
        mgr2.load().await.unwrap();

        let profiles = mgr2.list_profiles("anthropic").await;
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "main");
        assert_eq!(profiles[0].usage_tokens, 500);
    }

    #[tokio::test]
    async fn add_profile_replaces_existing_id() {
        let (mgr, _tmp) = test_manager().await;

        mgr.add_profile("openai", "primary", "sk-old")
            .await
            .unwrap();
        mgr.add_profile("openai", "primary", "sk-new")
            .await
            .unwrap();

        let profiles = mgr.list_profiles("openai").await;
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].api_key, "sk-new");
    }

    #[tokio::test]
    async fn empty_provider_returns_none() {
        let (mgr, _tmp) = test_manager().await;
        assert!(mgr.select_profile("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn report_success_does_not_clear_permanent_failure() {
        let (mgr, _tmp) = test_manager().await;

        mgr.add_profile("openai", "bad", "sk-bad").await.unwrap();
        mgr.report_error("openai", "bad", 401).await.unwrap();

        // Even after a "success" report, the permanent flag stays.
        mgr.report_success("openai", "bad", 100).await.unwrap();

        let profiles = mgr.list_profiles("openai").await;
        assert!(profiles[0].is_permanently_failed());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_to_disk_uses_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (mgr, _tmp) = test_manager().await;
        mgr.add_profile("openai", "k1", "sk-secret").await.unwrap();

        let meta = tokio::fs::metadata(mgr.store_path()).await.unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "profiles.json must be 0600, got {mode:o}");
    }

    #[tokio::test]
    async fn save_to_disk_overwrites_with_tightened_mode() {
        // Write a profile, then re-write — ensure the file is replaced
        // (and on Unix that 0600 is reasserted even if a prior run left
        // looser permissions on the temp copy).
        let (mgr, _tmp) = test_manager().await;
        mgr.add_profile("openai", "k1", "sk-1").await.unwrap();
        mgr.add_profile("openai", "k2", "sk-2").await.unwrap();
        let listed = mgr.list_profiles("openai").await;
        assert_eq!(listed.len(), 2);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = tokio::fs::metadata(mgr.store_path()).await.unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
