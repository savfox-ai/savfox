#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const APP_BUNDLE_NAME: &str = "Savfox.app";

pub async fn run_app_open_or_install(workspace: PathBuf, download_url: String) -> Result<()> {
    let app_path = match find_installed_app().await? {
        Some(path) => path,
        None => install_app_from_dmg(&download_url).await?,
    };

    open_app(&app_path, &workspace).await
}

async fn find_installed_app() -> Result<Option<PathBuf>> {
    for candidate in app_install_candidates() {
        if fs::try_exists(&candidate)
            .await
            .with_context(|| format!("failed to check {}", candidate.display()))?
        {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn app_install_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("/Applications").join(APP_BUNDLE_NAME)];
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("Applications").join(APP_BUNDLE_NAME));
    }
    candidates
}

async fn install_app_from_dmg(download_url: &str) -> Result<PathBuf> {
    let temp_dir = tempfile::tempdir().context("failed to create temporary directory")?;
    let dmg_path = temp_dir.path().join("Savfox.dmg");
    let mount_point = temp_dir.path().join("mnt");
    fs::create_dir_all(&mount_point)
        .await
        .with_context(|| format!("failed to create mount point {}", mount_point.display()))?;

    download_dmg(download_url, &dmg_path).await?;

    attach_dmg(&dmg_path, &mount_point).await?;

    let app_in_dmg = mount_point.join(APP_BUNDLE_NAME);
    let installed_app = choose_install_destination();

    let copy_result = copy_app_bundle(&app_in_dmg, &installed_app).await;
    let detach_result = detach_dmg(&mount_point).await;

    copy_result?;
    detach_result?;

    Ok(installed_app)
}

fn choose_install_destination() -> PathBuf {
    let system_app = PathBuf::from("/Applications").join(APP_BUNDLE_NAME);
    if std::fs::metadata("/Applications")
        .map(|meta| !meta.permissions().readonly())
        .unwrap_or(false)
    {
        return system_app;
    }

    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Applications")
        .join(APP_BUNDLE_NAME)
}

async fn download_dmg(download_url: &str, dmg_path: &Path) -> Result<()> {
    let response = reqwest::get(download_url)
        .await
        .with_context(|| format!("failed to download DMG from {download_url}"))?
        .error_for_status()
        .with_context(|| format!("download failed for {download_url}"))?;

    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("failed to read DMG response from {download_url}"))?;

    let mut file = fs::File::create(dmg_path)
        .await
        .with_context(|| format!("failed to create {}", dmg_path.display()))?;
    file.write_all(&bytes)
        .await
        .with_context(|| format!("failed to write {}", dmg_path.display()))?;
    Ok(())
}

async fn attach_dmg(dmg_path: &Path, mount_point: &Path) -> Result<()> {
    run_command(
        "hdiutil",
        &[
            "attach",
            "-nobrowse",
            "-readonly",
            "-mountpoint",
            mount_point
                .to_str()
                .ok_or_else(|| anyhow!("mount point path is not valid UTF-8"))?,
            dmg_path
                .to_str()
                .ok_or_else(|| anyhow!("dmg path is not valid UTF-8"))?,
        ],
    )
    .await
    .context("failed to attach downloaded DMG")
}

async fn detach_dmg(mount_point: &Path) -> Result<()> {
    run_command(
        "hdiutil",
        &[
            "detach",
            mount_point
                .to_str()
                .ok_or_else(|| anyhow!("mount point path is not valid UTF-8"))?,
        ],
    )
    .await
    .context("failed to detach DMG")
}

async fn copy_app_bundle(src_app: &Path, dst_app: &Path) -> Result<()> {
    if !fs::try_exists(src_app)
        .await
        .with_context(|| format!("failed to inspect {}", src_app.display()))?
    {
        bail!("downloaded DMG does not contain {}", src_app.display());
    }

    if let Some(parent) = dst_app.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    run_command(
        "ditto",
        &[
            src_app
                .to_str()
                .ok_or_else(|| anyhow!("source app path is not valid UTF-8"))?,
            dst_app
                .to_str()
                .ok_or_else(|| anyhow!("destination app path is not valid UTF-8"))?,
        ],
    )
    .await
    .with_context(|| format!("failed to install app to {}", dst_app.display()))
}

async fn open_app(app_path: &Path, workspace: &Path) -> Result<()> {
    run_command(
        "open",
        &[
            "-a",
            app_path
                .to_str()
                .ok_or_else(|| anyhow!("app path is not valid UTF-8"))?,
            workspace
                .to_str()
                .ok_or_else(|| anyhow!("workspace path is not valid UTF-8"))?,
        ],
    )
    .await
    .with_context(|| {
        format!(
            "failed to open {} with {}",
            workspace.display(),
            app_path.display()
        )
    })
}

async fn run_command(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("failed to execute {program}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };
    bail!("{program} failed: {detail}");
}
