#![allow(clippy::print_stderr)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use savfox_utils::home_dir::{GATEWAY_LOG_FILE, GATEWAY_PID_FILE};
use tracing::{error, info, warn};

/// Maximum time to wait for a process to exit after sending a termination signal.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Polling interval when waiting for a process to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// Service manager detection
// ---------------------------------------------------------------------------

/// Detected service manager on the host system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceManager {
    Systemd,
    Launchd,
    WindowsService,
    None,
}

fn detect_service_manager_auto() -> ServiceManager {
    if cfg!(target_os = "macos") {
        ServiceManager::Launchd
    } else if cfg!(target_os = "linux") && Path::new("/run/systemd/system").exists() {
        ServiceManager::Systemd
    } else if cfg!(windows) {
        ServiceManager::WindowsService
    } else {
        ServiceManager::None
    }
}

/// Detect which service manager is available on the current platform.
pub(crate) fn detect_service_manager() -> ServiceManager {
    detect_service_manager_with_override(None)
}

/// Detect service manager with optional runtime override.
///
/// Supported runtime values:
/// - `auto` (default): detect from host platform.
/// - `systemd`: force systemd flow (Linux only; falls back with warning elsewhere).
/// - `launchd`: force launchd flow (macOS only; falls back with warning elsewhere).
/// - `windows-task`: force Windows scheduled-task flow (Windows only; falls back with warning
///   elsewhere).
pub(crate) fn detect_service_manager_with_override(runtime: Option<&str>) -> ServiceManager {
    let auto = detect_service_manager_auto();
    let Some(runtime) = runtime.map(str::trim).filter(|v| !v.is_empty()) else {
        return auto;
    };

    match runtime.to_ascii_lowercase().as_str() {
        "auto" => auto,
        "systemd" => {
            if cfg!(target_os = "linux") {
                ServiceManager::Systemd
            } else {
                eprintln!(
                    "Warning: runtime=systemd is not native on this OS; falling back to auto."
                );
                auto
            }
        }
        "launchd" => {
            if cfg!(target_os = "macos") {
                ServiceManager::Launchd
            } else {
                eprintln!(
                    "Warning: runtime=launchd is not native on this OS; falling back to auto."
                );
                auto
            }
        }
        "windows-task" | "windows" => {
            if cfg!(windows) {
                ServiceManager::WindowsService
            } else {
                eprintln!(
                    "Warning: runtime=windows-task is not native on this OS; falling back to auto."
                );
                auto
            }
        }
        _ => {
            eprintln!("Warning: unknown daemon runtime '{runtime}'; falling back to auto.");
            auto
        }
    }
}

// ---------------------------------------------------------------------------
// PID file management
// ---------------------------------------------------------------------------

/// Return the default PID file path: `{savfox_home}/gateway.pid`.
pub(crate) fn default_pid_file(savfox_home: &Path) -> PathBuf {
    savfox_home.join(GATEWAY_PID_FILE)
}

/// Read a PID from the file at `path`. Returns `None` if the file doesn't
/// exist, is empty, or contains an invalid value.
pub(crate) fn read_pid_file(path: &Path) -> Option<u32> {
    let contents = fs::read_to_string(path).ok()?;
    let pid = contents.trim().parse::<u32>().ok()?;
    if pid == 0 {
        return None;
    }
    Some(pid)
}

/// Remove the PID file if it exists.
pub(crate) fn remove_pid_file(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => info!(path = %path.display(), "removed PID file"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(path = %path.display(), error = %e, "failed to remove PID file"),
    }
}

// ---------------------------------------------------------------------------
// Process management
// ---------------------------------------------------------------------------

/// Check whether a process with the given PID is still running.
///
/// On Unix, uses `kill(pid, 0)` via `std::process::Command` calling `kill -0`.
/// On Windows, uses `tasklist` to check if the PID exists.
pub(crate) fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // `kill -0 <pid>` exits 0 if the process exists, non-zero otherwise.
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        // `tasklist /FI "PID eq <pid>"` outputs the process if it exists.
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .is_ok_and(|output| {
                let stdout = String::from_utf8_lossy(&output.stdout);
                stdout.contains(&pid.to_string())
            })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Attempt to gracefully stop a process with the given PID, waiting up to
/// [`STOP_TIMEOUT`] for it to exit. If the process does not exit within the
/// timeout, a forceful kill is sent.
///
/// On Unix this sends `SIGTERM` via `kill`, then `SIGKILL` if needed.
/// On Windows this first tries `taskkill` (graceful), then `taskkill /F`.
/// Returns `Ok(())` once the process is confirmed stopped (or was already gone).
pub(crate) fn stop_process(pid: u32) -> anyhow::Result<()> {
    if !is_process_running(pid) {
        info!(pid, "process already exited");
        return Ok(());
    }

    // Send the initial graceful termination signal.
    send_term_signal(pid)?;

    // Wait for the process to exit, polling periodically.
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        if !is_process_running(pid) {
            info!(pid, "process exited after graceful signal");
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    // Timed out -- escalate to forceful kill.
    warn!(
        pid,
        "process did not exit within timeout, sending forceful kill"
    );
    send_kill_signal(pid)?;

    // Give it a brief moment after the forceful kill.
    std::thread::sleep(Duration::from_millis(500));
    if is_process_running(pid) {
        anyhow::bail!("process {pid} could not be stopped");
    }

    info!(pid, "process forcefully terminated");
    Ok(())
}

/// Send a graceful termination signal (SIGTERM on Unix, `taskkill` on Windows).
fn send_term_signal(pid: u32) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let status = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;

        if status.success() {
            info!(pid, "sent SIGTERM");
        } else {
            info!(
                pid,
                "kill -TERM returned non-zero (process may have already exited)"
            );
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        // `taskkill` without `/F` sends WM_CLOSE for GUI apps, or a ctrl-close signal
        // for console apps. This is the closest to a graceful shutdown on Windows.
        let status = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;

        if status.success() {
            info!(pid, "sent graceful taskkill");
        } else {
            info!(
                pid,
                "taskkill returned non-zero (process may have already exited)"
            );
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        anyhow::bail!("stopping processes is not supported on this platform");
    }
}

/// Send a forceful kill signal (SIGKILL on Unix, `taskkill /F` on Windows).
fn send_kill_signal(pid: u32) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let status = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;

        if status.success() {
            info!(pid, "sent SIGKILL");
        } else {
            info!(
                pid,
                "kill -KILL returned non-zero (process may have already exited)"
            );
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let status = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;

        if status.success() {
            info!(pid, "forcefully terminated process via taskkill /F");
        } else {
            info!(
                pid,
                "taskkill /F returned non-zero (process may have already exited)"
            );
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        anyhow::bail!("stopping processes is not supported on this platform");
    }
}

// ---------------------------------------------------------------------------
// Daemon start helper
// ---------------------------------------------------------------------------

/// Spawn a new detached gateway process in the background and return its PID.
///
/// The child process runs `savfox gateway --host <host> --port <port>` with
/// stdout/stderr redirected to a log file under `savfox_home`.
///
/// On Windows, the child is created with `CREATE_NEW_PROCESS_GROUP` and
/// `DETACHED_PROCESS` flags so it survives the parent process exiting.
pub(crate) fn spawn_daemon(
    host: &std::net::IpAddr,
    port: u16,
    savfox_home: &Path,
) -> anyhow::Result<u32> {
    let exe = std::env::current_exe()?;

    // Ensure the savfox home directory exists.
    fs::create_dir_all(savfox_home)?;

    // Log file for the daemon's output.
    let log_path = savfox_home.join(GATEWAY_LOG_FILE);

    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stderr_file = log_file.try_clone()?;

    info!(
        exe = %exe.display(),
        log = %log_path.display(),
        "spawning gateway daemon"
    );

    let mut cmd = std::process::Command::new(&exe);
    cmd.args([
        "gateway",
        "--host",
        &host.to_string(),
        "--port",
        &port.to_string(),
    ])
    .stdout(log_file)
    .stderr(stderr_file)
    .stdin(std::process::Stdio::null());

    // On Windows, detach the child from the parent console so it survives
    // the CLI process exiting.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP (0x0200) | DETACHED_PROCESS (0x0008)
        const DETACH_FLAGS: u32 = 0x0200 | 0x0008;
        cmd.creation_flags(DETACH_FLAGS);
    }

    let child = cmd.spawn()?;
    let pid = child.id();
    info!(pid, "daemon process spawned");
    Ok(pid)
}

// ---------------------------------------------------------------------------
// Service install / uninstall
// ---------------------------------------------------------------------------

fn passthrough_env_vars() -> Vec<(String, String)> {
    const KEYS: [&str; 13] = [
        "SAVFOX_HOME",
        "SAVFOX_GATEWAY_URL",
        "SAVFOX_GATEWAY_TOKEN",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GROQ_API_KEY",
        "DISCORD_BOT_TOKEN",
        "TELEGRAM_BOT_TOKEN",
        "SLACK_BOT_TOKEN",
        "SLACK_SIGNING_SECRET",
        "WEBHOOK_SECRET",
        "WHATSAPP_ACCESS_TOKEN",
        "WHATSAPP_PHONE_NUMBER_ID",
    ];
    KEYS.iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| ((*key).to_owned(), value))
        })
        .collect()
}

fn systemd_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Install a systemd user service unit.
pub(crate) fn install_systemd_service(name: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let exe_str = exe.display();
    let env_lines = passthrough_env_vars()
        .into_iter()
        .map(|(key, value)| {
            format!(
                "Environment=\"{}={}\"\n",
                systemd_escape(&key),
                systemd_escape(&value)
            )
        })
        .collect::<String>();

    let unit = format!(
        "\
[Unit]
Description=Savfox Gateway Server
After=network.target

[Service]
Type=simple
ExecStart={exe_str} gateway
Restart=on-failure
RestartSec=5
{env_lines}

[Install]
WantedBy=default.target
"
    );

    let user_systemd_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("could not determine config directory"))?
        .join("systemd")
        .join("user");
    fs::create_dir_all(&user_systemd_dir)?;

    let unit_path = user_systemd_dir.join(format!("{name}.service"));
    fs::write(&unit_path, unit)?;
    info!(path = %unit_path.display(), "wrote systemd unit file");
    eprintln!("Installed systemd user service: {}", unit_path.display());
    eprintln!();
    eprintln!("To enable and start:");
    eprintln!("  systemctl --user daemon-reload");
    eprintln!("  systemctl --user enable --now {name}");
    eprintln!();
    eprintln!("To check status:");
    eprintln!("  systemctl --user status {name}");

    Ok(())
}

/// Install a launchd user agent plist.
pub(crate) fn install_launchd_service(name: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let exe_str = exe.display();
    let label = name.replace(' ', "-");
    let passthrough_vars = passthrough_env_vars();
    let env_dict = if passthrough_vars.is_empty() {
        String::new()
    } else {
        let mut out = String::from("  <key>EnvironmentVariables</key>\n  <dict>\n");
        for (key, value) in passthrough_vars {
            out.push_str(&format!(
                "    <key>{}</key>\n    <string>{}</string>\n",
                xml_escape(&key),
                xml_escape(&value)
            ));
        }
        out.push_str("  </dict>\n");
        out
    };

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe_str}</string>
    <string>gateway</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/tmp/{label}.out.log</string>
  <key>StandardErrorPath</key>
  <string>/tmp/{label}.err.log</string>
{env_dict}</dict>
</plist>
"#
    );

    let launch_agents = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?
        .join("Library")
        .join("LaunchAgents");
    fs::create_dir_all(&launch_agents)?;

    let plist_path = launch_agents.join(format!("{label}.plist"));
    fs::write(&plist_path, plist)?;
    info!(path = %plist_path.display(), "wrote launchd plist");
    eprintln!("Installed launchd agent: {}", plist_path.display());
    eprintln!();
    eprintln!("To load and start:");
    eprintln!("  launchctl load {}", plist_path.display());
    eprintln!();
    eprintln!("To unload:");
    eprintln!("  launchctl unload {}", plist_path.display());

    Ok(())
}

/// Install a Windows scheduled task that starts gateway at logon.
pub(crate) fn install_windows_scheduled_task(name: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()?;
        let command = format!("\"{}\" gateway", exe.display());
        let status = std::process::Command::new("schtasks")
            .args([
                "/Create", "/TN", name, "/TR", &command, "/SC", "ONLOGON", "/RL", "LIMITED", "/F",
            ])
            .status()?;

        if !status.success() {
            anyhow::bail!("failed to create scheduled task '{name}'");
        }

        eprintln!("Installed Windows scheduled task: {name}");
        eprintln!("Task runs at user logon and launches `savfox gateway`.");
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        anyhow::bail!("windows scheduled tasks are only supported on Windows");
    }
}

/// Uninstall a Windows scheduled task created for gateway daemon mode.
pub(crate) fn uninstall_windows_scheduled_task(name: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let status = std::process::Command::new("schtasks")
            .args(["/Delete", "/TN", name, "/F"])
            .status()?;

        if !status.success() {
            anyhow::bail!("failed to remove scheduled task '{name}'");
        }

        eprintln!("Removed Windows scheduled task: {name}");
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        anyhow::bail!("windows scheduled tasks are only supported on Windows");
    }
}

pub(crate) fn uninstall_service_with_manager(
    name: &str,
    manager: ServiceManager,
) -> anyhow::Result<()> {
    match manager {
        ServiceManager::Systemd => {
            let user_systemd_dir = dirs::config_dir()
                .ok_or_else(|| anyhow::anyhow!("could not determine config directory"))?
                .join("systemd")
                .join("user");
            let unit_path = user_systemd_dir.join(format!("{name}.service"));

            if unit_path.exists() {
                fs::remove_file(&unit_path)?;
                info!(path = %unit_path.display(), "removed systemd unit file");
                eprintln!("Removed systemd unit: {}", unit_path.display());
                eprintln!();
                eprintln!("Run to clean up:");
                eprintln!("  systemctl --user daemon-reload");
            } else {
                eprintln!("No systemd unit found at {}", unit_path.display());
            }
        }
        ServiceManager::Launchd => {
            let label = name.replace(' ', "-");
            let launch_agents = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?
                .join("Library")
                .join("LaunchAgents");
            let plist_path = launch_agents.join(format!("{label}.plist"));

            if plist_path.exists() {
                fs::remove_file(&plist_path)?;
                info!(path = %plist_path.display(), "removed launchd plist");
                eprintln!("Removed launchd agent: {}", plist_path.display());
                eprintln!();
                eprintln!("If the service was loaded, also run:");
                eprintln!("  launchctl unload {}", plist_path.display());
            } else {
                eprintln!("No launchd plist found at {}", plist_path.display());
            }
        }
        ServiceManager::WindowsService => {
            uninstall_windows_scheduled_task(name)?;
        }
        ServiceManager::None => {
            error!("no supported service manager detected on this platform");
            anyhow::bail!("no supported service manager detected");
        }
    }

    Ok(())
}

/// Uninstall a previously-installed system service.
pub(crate) fn uninstall_service(name: &str) -> anyhow::Result<()> {
    uninstall_service_with_manager(name, detect_service_manager())
}
