//! `savfox update` — Self-updating CLI from GitHub releases.

use std::path::Path;
use std::process::Command;
use std::{env, fs};

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;

const GITHUB_API_URL: &str = "https://api.github.com/repos/anomalyco/savfox/releases";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Manage CLI updates from GitHub releases.
#[derive(Debug, Parser)]
pub struct UpdateCommand {
    #[clap(subcommand)]
    pub action: UpdateAction,
}

#[derive(Debug, clap::Subcommand)]
pub enum UpdateAction {
    /// Check for available updates.
    Check {
        /// Update channel to check
        #[clap(long, default_value = "stable")]
        channel: String,
        /// Output format
        #[clap(long, default_value = "table")]
        format: String,
    },
    /// Install the latest update.
    Install {
        /// Update channel to install from
        #[clap(long, default_value = "stable")]
        channel: String,
        /// Force reinstall even if already up to date
        #[clap(long)]
        force: bool,
        /// Dry run (download but don't install)
        #[clap(long)]
        dry_run: bool,
        /// Skip daemon restart even if a gateway PID file is detected.
        #[clap(long)]
        skip_restart: bool,
        /// Skip running `savfox doctor` after install.
        #[clap(long)]
        skip_doctor: bool,
    },
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    prerelease: bool,
    draft: bool,
    assets: Vec<GitHubAsset>,
    published_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

pub async fn run(cmd: UpdateCommand) -> Result<()> {
    match cmd.action {
        UpdateAction::Check { channel, format } => run_check(&channel, &format).await,
        UpdateAction::Install {
            channel,
            force,
            dry_run,
            skip_restart,
            skip_doctor,
        } => run_install(&channel, force, dry_run, skip_restart, skip_doctor).await,
    }
}

async fn run_check(channel: &str, format: &str) -> Result<()> {
    eprintln!("Checking for updates ({channel} channel)...");

    let releases = fetch_releases().await?;
    let latest = find_latest_release(&releases, channel)?;

    let current =
        semver::Version::parse(CURRENT_VERSION).context("Failed to parse current version")?;
    let latest_ver = extract_version(&latest.tag_name)?;

    let update_available = latest_ver > current;

    if format == "json" {
        let json = serde_json::json!({
            "current_version": CURRENT_VERSION,
            "latest_version": latest_ver.to_string(),
            "update_available": update_available,
            "channel": channel,
            "release_url": format!("https://github.com/anomalyco/savfox/releases/tag/{}", latest.tag_name),
            "published_at": latest.published_at,
            "changelog": latest.body,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        println!();
        println!("Current version: v{}", CURRENT_VERSION);
        println!("Latest version:  v{} ({})", latest_ver, channel);

        if update_available {
            println!();
            println!(
                "Update available! Run: savfox update install --channel {}",
                channel
            );
            println!(
                "Release notes: https://github.com/anomalyco/savfox/releases/tag/{}",
                latest.tag_name
            );

            if let Some(notes) = &latest.body {
                println!();
                println!("Changelog:");
                for line in notes.lines().take(20) {
                    println!("  {}", line);
                }
                let line_count = notes.lines().count();
                if line_count > 20 {
                    println!("  ... (truncated, see release page for full notes)");
                }
            }
        } else {
            println!();
            println!("Already up to date.");
        }
    }

    Ok(())
}

async fn run_install(
    channel: &str,
    force: bool,
    dry_run: bool,
    skip_restart: bool,
    skip_doctor: bool,
) -> Result<()> {
    eprintln!("Installing update ({channel} channel)...");

    let releases = fetch_releases().await?;
    let latest = find_latest_release(&releases, channel)?;

    let current =
        semver::Version::parse(CURRENT_VERSION).context("Failed to parse current version")?;
    let latest_ver = extract_version(&latest.tag_name)?;

    if !force && latest_ver <= current {
        eprintln!(
            "Already up to date (v{}). Use --force to reinstall.",
            latest_ver
        );
        return Ok(());
    }

    let target_triple = get_target_triple()?;
    let asset_name = format!("savfox-{}", target_triple);

    let asset = latest
        .assets
        .iter()
        .find(|a| a.name.starts_with(&asset_name) || a.name == asset_name)
        .or_else(|| {
            latest
                .assets
                .iter()
                .find(|a| a.name.contains(&target_triple))
        })
        .context(format!("No asset found for target {}", target_triple))?;

    eprintln!("Downloading {} ({} bytes)...", asset.name, asset.size);

    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_file = temp_dir.path().join(&asset.name);

    download_file(&asset.browser_download_url, &temp_file).await?;

    eprintln!("Downloaded to {}", temp_file.display());

    if dry_run {
        eprintln!("Dry run - not installing.");
        return Ok(());
    }

    let current_exe = env::current_exe().context("Failed to get current executable path")?;

    install_binary(&temp_file, &current_exe)?;

    eprintln!("Update installed successfully to v{}!", latest_ver);
    eprintln!("Restart savfox to use the new version.");

    if !skip_restart && gateway_daemon_likely_running() {
        eprintln!("Gateway daemon PID file detected. Running post-update restart...");
        if run_self_command(&current_exe, &["gateway", "restart"])? {
            eprintln!("Gateway daemon restart completed.");
        } else {
            eprintln!("Gateway daemon restart failed. Run `savfox gateway restart` manually.");
        }
    }

    if !skip_doctor {
        eprintln!("Running post-update health check (`savfox doctor`)...");
        if run_self_command(&current_exe, &["doctor"])? {
            eprintln!("Post-update doctor completed.");
        } else {
            eprintln!("Doctor reported issues. Run `savfox doctor` for details.");
        }
    }

    Ok(())
}

fn run_self_command(current_exe: &Path, args: &[&str]) -> Result<bool> {
    let status = Command::new(current_exe)
        .args(args)
        .status()
        .with_context(|| format!("failed to execute post-update command: {}", args.join(" ")))?;
    Ok(status.success())
}

fn gateway_daemon_likely_running() -> bool {
    let Ok(savfox_home) = savfox_core::config::find_savfox_home() else {
        return false;
    };
    let pid_file = savfox_home.join("gateway.pid");
    if !pid_file.exists() {
        return false;
    }
    fs::read_to_string(pid_file)
        .ok()
        .and_then(|content| content.trim().parse::<u32>().ok())
        .is_some()
}

fn install_binary(temp_file: &Path, current_exe: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let backup = current_exe.with_extension("exe.bak");

        if backup.exists() {
            fs::remove_file(&backup).context("Failed to remove old backup")?;
        }

        fs::rename(current_exe, &backup).context("Failed to backup current executable")?;

        fs::copy(temp_file, current_exe).context("Failed to copy new executable")?;

        eprintln!("Created backup at {}", backup.display());
    }

    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(temp_file, fs::Permissions::from_mode(0o755))
            .context("Failed to set executable permissions")?;

        let backup = current_exe.with_extension("bak");

        if backup.exists() {
            fs::remove_file(&backup).context("Failed to remove old backup")?;
        }

        fs::rename(current_exe, &backup).context("Failed to backup current executable")?;

        fs::copy(temp_file, current_exe).context("Failed to copy new executable")?;

        eprintln!("Created backup at {}", backup.display());
    }

    Ok(())
}

async fn fetch_releases() -> Result<Vec<GitHubRelease>> {
    let client = reqwest::Client::builder()
        .user_agent(format!("savfox/{}", CURRENT_VERSION))
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(GITHUB_API_URL)
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .context("Failed to fetch releases from GitHub")?;

    if !response.status().is_success() {
        anyhow::bail!("GitHub API returned status {}", response.status());
    }

    let releases: Vec<GitHubRelease> = response
        .json()
        .await
        .context("Failed to parse GitHub releases")?;

    Ok(releases)
}

fn find_latest_release<'a>(
    releases: &'a [GitHubRelease],
    channel: &str,
) -> Result<&'a GitHubRelease> {
    let release = match channel {
        "stable" => releases
            .iter()
            .find(|r| !r.prerelease && !r.draft)
            .context("No stable releases found")?,
        "beta" | "rc" => releases
            .iter()
            .filter(|r| !r.draft)
            .find(|r| r.prerelease || r.tag_name.contains("-rc") || r.tag_name.contains("-beta"))
            .or_else(|| releases.iter().find(|r| !r.prerelease && !r.draft))
            .context("No releases found")?,
        "dev" | "nightly" => releases
            .iter()
            .filter(|r| !r.draft)
            .next()
            .context("No releases found")?,
        _ => anyhow::bail!("Unknown channel: {}. Use stable, beta, or dev.", channel),
    };

    Ok(release)
}

fn extract_version(tag: &str) -> Result<semver::Version> {
    let version_str = tag.trim_start_matches('v');
    semver::Version::parse(version_str).context(format!("Failed to parse version from tag {}", tag))
}

fn get_target_triple() -> Result<String> {
    let target = env::var("TARGET")
        .or_else(|_| env::var("HOST"))
        .unwrap_or_else(|_| {
            let arch = if cfg!(target_arch = "x86_64") {
                "x86_64"
            } else if cfg!(target_arch = "aarch64") {
                "aarch64"
            } else {
                "unknown"
            };
            let os = if cfg!(target_os = "windows") {
                "pc-windows-msvc"
            } else if cfg!(target_os = "macos") {
                "apple-darwin"
            } else if cfg!(target_os = "linux") {
                "unknown-linux-musl"
            } else {
                "unknown"
            };
            format!("{}-{}", arch, os)
        });
    Ok(target)
}

async fn download_file(url: &str, path: &Path) -> Result<()> {
    let response = reqwest::get(url).await.context("Failed to download file")?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed with status {}", response.status());
    }

    let bytes = response
        .bytes()
        .await
        .context("Failed to read response body")?;

    fs::write(path, &bytes).context("Failed to write downloaded file")?;

    Ok(())
}
