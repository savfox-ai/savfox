//! Self-update module for the Savfox binary.
//!
//! Provides version checking against GitHub Releases, binary download and
//! replacement, and rollback to a previous version.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use semver::Version;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Release channel used when checking for updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    /// Only stable (non-prerelease) releases.
    Stable,
    /// Pre-releases whose tag contains "beta".
    Beta,
    /// Pre-releases whose tag contains "dev" or "alpha".
    Dev,
}

impl std::fmt::Display for UpdateChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stable => write!(f, "stable"),
            Self::Beta => write!(f, "beta"),
            Self::Dev => write!(f, "dev"),
        }
    }
}

/// Information about an available update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    /// Currently running version.
    pub current_version: String,
    /// Latest version available for the selected channel.
    pub latest_version: String,
    /// Channel that was checked.
    pub channel: UpdateChannel,
    /// Direct download URL for the platform-appropriate asset.
    pub download_url: String,
    /// Markdown release notes (body) from the GitHub release.
    pub release_notes: Option<String>,
    /// Timestamp when the release was published.
    pub published_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// GitHub API response types (private)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    prerelease: bool,
    body: Option<String>,
    published_at: Option<DateTime<Utc>>,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check whether a newer version is available on the given [`UpdateChannel`].
///
/// Returns `Ok(None)` when the local binary is already up to date (or newer).
/// Returns `Ok(Some(info))` when a newer release exists.
pub async fn check_for_updates(channel: UpdateChannel) -> Result<Option<UpdateInfo>> {
    let current_str = env!("CARGO_PKG_VERSION");
    let current = Version::parse(current_str)
        .with_context(|| format!("failed to parse current version: {current_str}"))?;

    let client = crate::default_client::build_reqwest_client();

    let releases: Vec<GitHubRelease> = client
        .get("https://api.github.com/repos/savfox-ai/savfox/releases")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("failed to fetch releases from GitHub")?
        .error_for_status()
        .context("GitHub API returned an error status")?
        .json()
        .await
        .context("failed to deserialize GitHub releases")?;

    let candidate = releases
        .into_iter()
        .filter(|r| matches_channel(r, channel))
        .filter_map(|r| {
            let version = parse_tag_version(&r.tag_name)?;
            Some((version, r))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b));

    let Some((latest_version, release)) = candidate else {
        tracing::debug!("no releases found for channel {channel}");
        return Ok(None);
    };

    if latest_version <= current {
        tracing::debug!(
            %current,
            %latest_version,
            "already up to date on {channel} channel"
        );
        return Ok(None);
    }

    let asset_name = platform_asset_name();
    let download_url = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .map(|a| a.browser_download_url.clone())
        .unwrap_or_else(|| {
            // Fallback: construct a plausible URL even when the asset list
            // does not contain an exact match.
            format!(
                "https://github.com/savfox-ai/savfox/releases/download/{tag}/{asset_name}",
                tag = release.tag_name,
            )
        });

    Ok(Some(UpdateInfo {
        current_version: current_str.to_owned(),
        latest_version: latest_version.to_string(),
        channel,
        download_url,
        release_notes: release.body,
        published_at: release.published_at,
    }))
}

/// Download the binary from `info.download_url` and replace the running
/// executable.
///
/// A backup of the current binary is created at `<path>.bak` so that
/// [`rollback`] can restore it if needed.
pub async fn download_and_install(info: &UpdateInfo) -> Result<()> {
    let current_exe = std::env::current_exe().context("failed to determine current executable")?;

    tracing::info!(
        url = %info.download_url,
        dest = %current_exe.display(),
        "downloading update"
    );

    let client = crate::default_client::build_reqwest_client();
    let bytes = client
        .get(&info.download_url)
        .send()
        .await
        .context("failed to download update")?
        .error_for_status()
        .context("download returned an error status")?
        .bytes()
        .await
        .context("failed to read update bytes")?;

    if bytes.is_empty() {
        bail!("downloaded update is empty");
    }

    // Create a backup of the current binary.
    let backup_path = backup_path_for(&current_exe);
    // On Windows the running exe cannot be overwritten directly, but it *can*
    // be renamed. We rename it to the backup path first, then write the new
    // binary to the original path.
    #[cfg(target_os = "windows")]
    {
        if backup_path.exists() {
            tokio::fs::remove_file(&backup_path)
                .await
                .with_context(|| {
                    format!("failed to remove old backup at {}", backup_path.display())
                })?;
        }
        tokio::fs::rename(&current_exe, &backup_path)
            .await
            .with_context(|| {
                format!(
                    "failed to rename running binary to backup at {}",
                    backup_path.display()
                )
            })?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        tokio::fs::copy(&current_exe, &backup_path)
            .await
            .with_context(|| format!("failed to create backup at {}", backup_path.display()))?;
    }

    tracing::info!(backup = %backup_path.display(), "created backup of current binary");

    // Write the new binary.  On Unix we write to a temporary file in the same
    // directory and atomically rename it over the target so we never leave a
    // half-written executable on disk.
    #[cfg(not(target_os = "windows"))]
    {
        let dir = current_exe
            .parent()
            .context("current executable has no parent directory")?;
        let tmp = dir.join(".savfox-update.tmp");
        tokio::fs::write(&tmp, &bytes)
            .await
            .context("failed to write temporary update file")?;

        // Preserve the executable permission bits from the original binary.
        use std::os::unix::fs::PermissionsExt;
        let original_meta = tokio::fs::metadata(&backup_path).await?;
        let perms = original_meta.permissions();
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(perms.mode()))
            .await
            .context("failed to set permissions on temporary update file")?;

        tokio::fs::rename(&tmp, &current_exe)
            .await
            .context("failed to replace current executable with update")?;
    }

    #[cfg(target_os = "windows")]
    {
        tokio::fs::write(&current_exe, &bytes)
            .await
            .context("failed to write updated binary")?;
    }

    tracing::info!(
        version = %info.latest_version,
        "update installed successfully"
    );

    Ok(())
}

/// Restore the previous binary from the `.bak` backup created by
/// [`download_and_install`].
pub async fn rollback() -> Result<()> {
    let current_exe = std::env::current_exe().context("failed to determine current executable")?;
    let backup_path = backup_path_for(&current_exe);

    if !backup_path.exists() {
        bail!(
            "no backup found at {}; nothing to roll back to",
            backup_path.display()
        );
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows, rename the current (possibly running) binary out of the
        // way, then move the backup into place.
        let tmp = current_exe.with_extension("rollback-tmp.exe");
        if tmp.exists() {
            tokio::fs::remove_file(&tmp).await.ok();
        }
        tokio::fs::rename(&current_exe, &tmp)
            .await
            .context("failed to move current binary for rollback")?;
        tokio::fs::rename(&backup_path, &current_exe)
            .await
            .context("failed to restore backup")?;
        // Best-effort cleanup of the temp file.
        tokio::fs::remove_file(&tmp).await.ok();
    }

    #[cfg(not(target_os = "windows"))]
    {
        tokio::fs::rename(&backup_path, &current_exe)
            .await
            .context("failed to restore backup")?;
    }

    tracing::info!(
        restored_from = %backup_path.display(),
        "rollback complete"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Determine whether a GitHub release matches the requested channel.
fn matches_channel(release: &GitHubRelease, channel: UpdateChannel) -> bool {
    match channel {
        UpdateChannel::Stable => !release.prerelease,
        UpdateChannel::Beta => {
            let tag = release.tag_name.to_lowercase();
            release.prerelease && tag.contains("beta")
        }
        UpdateChannel::Dev => {
            let tag = release.tag_name.to_lowercase();
            release.prerelease && (tag.contains("dev") || tag.contains("alpha"))
        }
    }
}

/// Extract a [`semver::Version`] from a GitHub release tag.
///
/// Supports formats like `v1.2.3`, `rust-v1.2.3`, `1.2.3`,
/// `v1.2.3-beta.1`, etc.
fn parse_tag_version(tag: &str) -> Option<Version> {
    let stripped = tag
        .strip_prefix("rust-v")
        .or_else(|| tag.strip_prefix('v'))
        .unwrap_or(tag);
    Version::parse(stripped).ok()
}

/// Compute the expected asset name for the current platform / architecture.
fn platform_asset_name() -> String {
    let (os, ext) = if cfg!(target_os = "windows") {
        ("windows", ".exe")
    } else if cfg!(target_os = "macos") {
        ("macos", "")
    } else {
        ("linux", "")
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        std::env::consts::ARCH
    };

    format!("savfox-{os}-{arch}{ext}")
}

/// Return the path used for backup binaries.
fn backup_path_for(exe: &std::path::Path) -> PathBuf {
    let mut backup = exe.as_os_str().to_owned();
    backup.push(".bak");
    PathBuf::from(backup)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tag_version_handles_common_formats() {
        assert_eq!(parse_tag_version("v1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(
            parse_tag_version("rust-v0.10.5"),
            Some(Version::new(0, 10, 5))
        );
        assert_eq!(parse_tag_version("2.0.0"), Some(Version::new(2, 0, 0)));
        assert_eq!(
            parse_tag_version("v1.0.0-beta.1"),
            Some(Version::parse("1.0.0-beta.1").unwrap())
        );
    }

    #[test]
    fn parse_tag_version_returns_none_for_garbage() {
        assert_eq!(parse_tag_version("not-a-version"), None);
        assert_eq!(parse_tag_version(""), None);
    }

    #[test]
    fn matches_channel_stable_excludes_prereleases() {
        let stable = GitHubRelease {
            tag_name: "v1.0.0".into(),
            prerelease: false,
            body: None,
            published_at: None,
            assets: vec![],
        };
        let pre = GitHubRelease {
            tag_name: "v1.0.0-beta.1".into(),
            prerelease: true,
            body: None,
            published_at: None,
            assets: vec![],
        };
        assert!(matches_channel(&stable, UpdateChannel::Stable));
        assert!(!matches_channel(&pre, UpdateChannel::Stable));
    }

    #[test]
    fn matches_channel_beta_requires_beta_tag() {
        let beta = GitHubRelease {
            tag_name: "v1.0.0-beta.2".into(),
            prerelease: true,
            body: None,
            published_at: None,
            assets: vec![],
        };
        let alpha = GitHubRelease {
            tag_name: "v1.0.0-alpha.1".into(),
            prerelease: true,
            body: None,
            published_at: None,
            assets: vec![],
        };
        assert!(matches_channel(&beta, UpdateChannel::Beta));
        assert!(!matches_channel(&alpha, UpdateChannel::Beta));
    }

    #[test]
    fn matches_channel_dev_accepts_dev_and_alpha() {
        let dev = GitHubRelease {
            tag_name: "v2.0.0-dev.1".into(),
            prerelease: true,
            body: None,
            published_at: None,
            assets: vec![],
        };
        let alpha = GitHubRelease {
            tag_name: "v2.0.0-alpha.3".into(),
            prerelease: true,
            body: None,
            published_at: None,
            assets: vec![],
        };
        let beta = GitHubRelease {
            tag_name: "v2.0.0-beta.1".into(),
            prerelease: true,
            body: None,
            published_at: None,
            assets: vec![],
        };
        assert!(matches_channel(&dev, UpdateChannel::Dev));
        assert!(matches_channel(&alpha, UpdateChannel::Dev));
        assert!(!matches_channel(&beta, UpdateChannel::Dev));
    }

    #[test]
    fn backup_path_appends_bak_extension() {
        let exe = PathBuf::from("/usr/local/bin/savfox");
        assert_eq!(
            backup_path_for(&exe),
            PathBuf::from("/usr/local/bin/savfox.bak")
        );
    }

    #[test]
    fn backup_path_windows_style() {
        let exe = PathBuf::from(r"C:\Program Files\savfox\savfox.exe");
        assert_eq!(
            backup_path_for(&exe),
            PathBuf::from(r"C:\Program Files\savfox\savfox.exe.bak")
        );
    }

    #[test]
    fn platform_asset_name_is_not_empty() {
        let name = platform_asset_name();
        assert!(!name.is_empty());
        assert!(name.starts_with("savfox-"));
    }

    #[test]
    fn update_channel_display() {
        assert_eq!(UpdateChannel::Stable.to_string(), "stable");
        assert_eq!(UpdateChannel::Beta.to_string(), "beta");
        assert_eq!(UpdateChannel::Dev.to_string(), "dev");
    }

    #[test]
    fn update_channel_serialization_roundtrip() {
        let json = serde_json::to_string(&UpdateChannel::Beta).unwrap();
        assert_eq!(json, r#""beta""#);
        let deserialized: UpdateChannel = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, UpdateChannel::Beta);
    }

    #[test]
    fn update_info_serialization_roundtrip() {
        let info = UpdateInfo {
            current_version: "0.1.0".into(),
            latest_version: "0.2.0".into(),
            channel: UpdateChannel::Stable,
            download_url: "https://example.com/savfox".into(),
            release_notes: Some("Bug fixes".into()),
            published_at: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        let deserialized: UpdateInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.current_version, "0.1.0");
        assert_eq!(deserialized.latest_version, "0.2.0");
        assert_eq!(deserialized.download_url, "https://example.com/savfox");
    }
}
