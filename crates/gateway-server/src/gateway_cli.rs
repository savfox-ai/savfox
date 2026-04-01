use std::collections::HashSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tracing::info;

use crate::config::{ApprovalsAction, DevicesAction, GatewaySubcommand};
use crate::daemon;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolve the PID file path, falling back to `{savfox_home}/gateway.pid`.
fn resolve_pid_file(explicit: Option<PathBuf>) -> std::io::Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let savfox_home = savfox_core::config::find_savfox_home()?;
    Ok(daemon::default_pid_file(&savfox_home))
}

/// Execute a gateway CLI subcommand by querying a running gateway.
pub async fn run_subcommand(subcommand: GatewaySubcommand) -> std::io::Result<()> {
    match subcommand {
        GatewaySubcommand::Status { url } => run_status(&url).await,
        GatewaySubcommand::Logs { url, lines, follow } => run_logs(&url, lines, follow).await,
        GatewaySubcommand::Models { url } => run_models(&url).await,
        GatewaySubcommand::Approvals { url, action } => run_approvals(&url, action).await,
        GatewaySubcommand::Devices { url, action } => run_devices(&url, action).await,
        GatewaySubcommand::Channels { url } => run_channels(&url).await,
        GatewaySubcommand::Nodes { url } => run_nodes(&url).await,
        GatewaySubcommand::Start {
            port,
            host,
            pid_file,
        } => run_start(host, port, pid_file).await,
        GatewaySubcommand::Stop { pid_file } => run_stop(pid_file).await,
        GatewaySubcommand::Restart {
            port,
            host,
            pid_file,
        } => run_restart(host, port, pid_file).await,
        GatewaySubcommand::Install { name, runtime } => run_install(&name, Some(runtime.as_str())),
        GatewaySubcommand::Uninstall { name, runtime } => {
            run_uninstall(&name, Some(runtime.as_str()))
        }
    }
}

fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_default()
}

async fn get_json(url: &str) -> Result<Value, String> {
    let client = build_client();
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    response
        .json::<Value>()
        .await
        .map_err(|e| format!("invalid JSON: {e}"))
}

async fn post_json(url: &str, body: &Value) -> Result<Value, String> {
    let client = build_client();
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {}", String::from_utf8_lossy(&body)));
    }

    response
        .json::<Value>()
        .await
        .map_err(|e| format!("invalid JSON: {e}"))
}

fn print_json(value: &Value) {
    if let Ok(pretty) = serde_json::to_string_pretty(value) {
        eprintln!("{pretty}");
    }
}

async fn run_status(url: &str) -> std::io::Result<()> {
    let status_url = format!("{url}/api/status");
    let health_url = format!("{url}/health");

    eprintln!("Querying gateway at {url}...\n");

    match get_json(&health_url).await {
        Ok(health) => {
            eprintln!("Health:");
            print_json(&health);
        }
        Err(err) => {
            eprintln!("Gateway not reachable: {err}");
            return Ok(());
        }
    }

    eprintln!();

    match get_json(&status_url).await {
        Ok(status) => {
            eprintln!("Status:");
            print_json(&status);
        }
        Err(err) => {
            eprintln!("Failed to get status: {err}");
        }
    }

    Ok(())
}

fn log_entry_key(entry: &Value) -> String {
    let ts = entry_timestamp_str(entry);
    let level = entry
        .get("level")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source = entry
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = entry
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!("{ts}|{level}|{source}|{message}")
}

fn entry_timestamp_str(entry: &Value) -> String {
    // New format: "timestamp" as ISO-8601 string
    if let Some(ts) = entry.get("timestamp").and_then(Value::as_str) {
        return ts.to_string();
    }
    // Legacy format: "tsMs" or "ts_ms" as u64 milliseconds
    let ts_ms = entry
        .get("tsMs")
        .or_else(|| entry.get("ts_ms"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let secs = ts_ms / 1_000;
    let millis = ts_ms % 1_000;
    if let Some(dt) = chrono::DateTime::from_timestamp(secs as i64, (millis as u32) * 1_000_000) {
        dt.to_rfc3339()
    } else {
        format!("{ts_ms}")
    }
}

fn print_log_entry(entry: &Value) {
    let ts = entry_timestamp_str(entry);
    let level = entry.get("level").and_then(Value::as_str).unwrap_or("info");
    let source = entry
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("gateway");
    let message = entry
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    eprintln!("[{ts}] {level} {source}: {message}");
}

async fn run_logs(url: &str, lines: usize, follow: bool) -> std::io::Result<()> {
    let logs_url = format!("{url}/api/logs?lines={lines}");

    eprintln!("Fetching gateway logs from {url} (last {lines} entries)...\n");

    if !follow {
        match get_json(&logs_url).await {
            Ok(data) => {
                print_json(&data);
            }
            Err(err) => {
                eprintln!("Failed to fetch logs: {err}");
            }
        }
        return Ok(());
    }

    eprintln!("Follow mode enabled. Press Ctrl+C to stop.\n");

    let mut seen: HashSet<String> = HashSet::new();
    loop {
        match get_json(&logs_url).await {
            Ok(data) => {
                if let Some(entries) = data.get("logs").and_then(Value::as_array) {
                    for entry in entries {
                        let key = log_entry_key(entry);
                        if seen.insert(key) {
                            print_log_entry(entry);
                        }
                    }
                    if seen.len() > 10_000 {
                        seen.clear();
                    }
                } else {
                    eprintln!("warning: unexpected log payload shape");
                }
            }
            Err(err) => {
                eprintln!("Failed to fetch logs: {err}");
            }
        }
        tokio::time::sleep(Duration::from_millis(900)).await;
    }
}

async fn run_models(url: &str) -> std::io::Result<()> {
    let config_url = format!("{url}/api/config");

    eprintln!("Listing models from gateway at {url}...\n");

    match get_json(&config_url).await {
        Ok(config) => {
            print_json(&config);
        }
        Err(err) => {
            eprintln!("Failed to list models: {err}");
        }
    }

    Ok(())
}

async fn run_approvals(url: &str, action: Option<ApprovalsAction>) -> std::io::Result<()> {
    match action {
        None | Some(ApprovalsAction::List) => {
            let approvals_url = format!("{url}/api/exec/approvals");
            eprintln!("Listing pending approvals from {url}...\n");

            match get_json(&approvals_url).await {
                Ok(data) => print_json(&data),
                Err(err) => eprintln!("Failed: {err}"),
            }
        }
        Some(ApprovalsAction::Approve { id }) => {
            let resolve_url = format!("{url}/api/exec/approval/resolve");
            let body = serde_json::json!({
                "id": id,
                "approved": true,
                "resolved_by": "cli:operator",
            });

            eprintln!("Approving request {id}...");
            match post_json(&resolve_url, &body).await {
                Ok(data) => print_json(&data),
                Err(err) => eprintln!("Failed: {err}"),
            }
        }
        Some(ApprovalsAction::Deny { id, reason }) => {
            let resolve_url = format!("{url}/api/exec/approval/resolve");
            let body = serde_json::json!({
                "id": id,
                "approved": false,
                "resolved_by": "cli:operator",
                "reason": reason,
            });

            eprintln!("Denying request {id}...");
            match post_json(&resolve_url, &body).await {
                Ok(data) => print_json(&data),
                Err(err) => eprintln!("Failed: {err}"),
            }
        }
    }

    Ok(())
}

async fn run_devices(url: &str, action: Option<DevicesAction>) -> std::io::Result<()> {
    match action {
        None | Some(DevicesAction::List) => {
            let devices_url = format!("{url}/api/devices");
            eprintln!("Listing paired devices from {url}...\n");

            match get_json(&devices_url).await {
                Ok(data) => print_json(&data),
                Err(err) => eprintln!("Failed: {err}"),
            }
        }
        Some(DevicesAction::Pair { name }) => {
            let pair_url = format!("{url}/api/devices/pair");
            let body = serde_json::json!({
                "name": name.unwrap_or_else(|| "unnamed-device".to_string()),
            });
            eprintln!("Creating pairing request...");
            match post_json(&pair_url, &body).await {
                Ok(data) => print_json(&data),
                Err(err) => eprintln!("Failed: {err}"),
            }
        }
        Some(DevicesAction::Revoke { id }) => {
            let revoke_url = format!("{url}/api/devices/{id}/revoke");
            eprintln!("Revoking device {id}...");
            match post_json(&revoke_url, &serde_json::json!({})).await {
                Ok(data) => print_json(&data),
                Err(err) => eprintln!("Failed: {err}"),
            }
        }
    }

    Ok(())
}

async fn run_channels(url: &str) -> std::io::Result<()> {
    let channels_url = format!("{url}/api/channels");
    eprintln!("Listing channels from {url}...\n");

    match get_json(&channels_url).await {
        Ok(config) => print_json(&config),
        Err(err) => eprintln!("Failed: {err}"),
    }

    Ok(())
}

async fn run_nodes(url: &str) -> std::io::Result<()> {
    let nodes_url = format!("{url}/api/nodes");
    eprintln!("Listing connected nodes from {url}...\n");

    match get_json(&nodes_url).await {
        Ok(data) => print_json(&data),
        Err(err) => eprintln!("Failed: {err}"),
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Daemon management subcommands
// ---------------------------------------------------------------------------

async fn run_start(host: IpAddr, port: u16, pid_file: Option<PathBuf>) -> std::io::Result<()> {
    let pid_path = resolve_pid_file(pid_file)?;

    // Check if a daemon is already running.
    if let Some(existing_pid) = daemon::read_pid_file(&pid_path) {
        if daemon::is_process_running(existing_pid) {
            eprintln!("Gateway daemon is already running (PID {existing_pid}).");
            return Ok(());
        }
        // Stale PID file -- clean it up.
        info!(existing_pid, "stale PID file found, removing");
        daemon::remove_pid_file(&pid_path);
    }

    let savfox_home = savfox_core::config::find_savfox_home()?;

    match daemon::spawn_daemon(&host, port, &savfox_home) {
        Ok(child_pid) => {
            // Write the child PID (not our own) so that `stop` can find it.
            if let Some(parent) = pid_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&pid_path, child_pid.to_string())?;
            info!(pid = child_pid, "wrote daemon PID file");

            eprintln!("Gateway daemon started (PID {child_pid}).");
            eprintln!("  Listening on {host}:{port}");
            eprintln!("  PID file: {}", pid_path.display());
            eprintln!("  Log file: {}", savfox_home.join("gateway.log").display());
        }
        Err(err) => {
            eprintln!("Failed to start gateway daemon: {err}");
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                err.to_string(),
            ));
        }
    }

    Ok(())
}

async fn run_stop(pid_file: Option<PathBuf>) -> std::io::Result<()> {
    let pid_path = resolve_pid_file(pid_file)?;

    let pid = match daemon::read_pid_file(&pid_path) {
        Some(pid) => pid,
        None => {
            eprintln!(
                "No PID file found at {}. Is the gateway running?",
                pid_path.display()
            );
            return Ok(());
        }
    };

    if !daemon::is_process_running(pid) {
        eprintln!("Process {pid} is not running (stale PID file). Cleaning up.");
        daemon::remove_pid_file(&pid_path);
        return Ok(());
    }

    eprintln!("Stopping gateway daemon (PID {pid})...");

    // `stop_process` sends a graceful signal, waits up to 10 seconds, then
    // escalates to a forceful kill if necessary. Run it on a blocking thread
    // to avoid tying up the tokio runtime during the polling loop.
    let stop_result = tokio::task::spawn_blocking(move || daemon::stop_process(pid))
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    match stop_result {
        Ok(()) => {
            daemon::remove_pid_file(&pid_path);
            eprintln!("Gateway daemon stopped.");
        }
        Err(err) => {
            eprintln!("Failed to stop gateway daemon: {err}");
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                err.to_string(),
            ));
        }
    }

    Ok(())
}

async fn run_restart(host: IpAddr, port: u16, pid_file: Option<PathBuf>) -> std::io::Result<()> {
    let pid_path = resolve_pid_file(pid_file)?;

    // Stop if running.
    if let Some(pid) = daemon::read_pid_file(&pid_path) {
        if daemon::is_process_running(pid) {
            eprintln!("Stopping existing gateway daemon (PID {pid})...");
            let stop_result = tokio::task::spawn_blocking(move || daemon::stop_process(pid))
                .await
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
            if let Err(err) = stop_result {
                eprintln!("Warning: failed to stop PID {pid}: {err}");
            }
        }
        daemon::remove_pid_file(&pid_path);
    }

    // Start fresh.
    run_start(host, port, Some(pid_path)).await
}

fn run_install(name: &str, runtime: Option<&str>) -> std::io::Result<()> {
    let mgr = daemon::detect_service_manager_with_override(runtime);
    let result = match mgr {
        daemon::ServiceManager::Systemd => daemon::install_systemd_service(name),
        daemon::ServiceManager::Launchd => daemon::install_launchd_service(name),
        daemon::ServiceManager::WindowsService => daemon::install_windows_scheduled_task(name),
        daemon::ServiceManager::None => {
            eprintln!("No supported service manager detected on this platform.");
            return Ok(());
        }
    };

    result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
}

fn run_uninstall(name: &str, runtime: Option<&str>) -> std::io::Result<()> {
    let mgr = daemon::detect_service_manager_with_override(runtime);
    let result = match mgr {
        daemon::ServiceManager::Systemd
        | daemon::ServiceManager::Launchd
        | daemon::ServiceManager::WindowsService => {
            daemon::uninstall_service_with_manager(name, mgr)
        }
        daemon::ServiceManager::None => {
            eprintln!("No supported service manager detected on this platform.");
            return Ok(());
        }
    };
    result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
}
