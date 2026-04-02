//! Tailscale integration for gateway exposure and authentication.
//!
//! Provides:
//! - Funnel exposure (make gateway accessible via Tailscale Funnel)
//! - Identity-based authentication (Tailscale WhoIs API)
//! - Status monitoring

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Tailscale integration configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TailscaleConfig {
    /// Enable Tailscale integration.
    #[serde(default)]
    pub(crate) enabled: bool,
    /// Path to the Tailscale socket (Unix) or API URL.
    pub(crate) socket_path: Option<String>,
    /// Enable Funnel (expose gateway publicly via Tailscale).
    #[serde(default)]
    pub(crate) funnel: bool,
    /// Hostname for Funnel (e.g., "savfox"  -> savfox.tailnet-name.ts.net).
    pub(crate) funnel_hostname: Option<String>,
    /// Port to expose via Funnel.
    pub(crate) funnel_port: Option<u16>,
    /// Enable Tailscale identity-based auth.
    #[serde(default)]
    pub(crate) auth_enabled: bool,
    /// Map Tailscale users to gateway scopes.
    #[serde(default)]
    pub(crate) user_scopes: Vec<TailscaleUserScope>,
}

/// Map a Tailscale user to a gateway scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TailscaleUserScope {
    /// Tailscale login name (email).
    pub(crate) login: String,
    /// Gateway scope to grant.
    pub(crate) scope: String,
}

/// Tailscale status information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TailscaleStatus {
    /// Whether Tailscale is running.
    pub(crate) running: bool,
    /// Current Tailscale IP.
    pub(crate) tailscale_ip: Option<String>,
    /// Tailnet name.
    pub(crate) tailnet: Option<String>,
    /// Hostname.
    pub(crate) hostname: Option<String>,
    /// Whether Funnel is active.
    pub(crate) funnel_active: bool,
    /// Public Funnel URL if active.
    pub(crate) funnel_url: Option<String>,
}

/// Identity from Tailscale WhoIs API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TailscaleIdentity {
    /// Tailscale login name (email).
    pub(crate) login_name: String,
    /// Display name.
    pub(crate) name: String,
    /// Tailscale node name.
    pub(crate) node_name: String,
    /// Tailscale IP of the connecting node.
    pub(crate) node_ip: String,
}

/// Check Tailscale status by running `tailscale status --json`.
pub(crate) async fn get_status() -> anyhow::Result<TailscaleStatus> {
    let output = tokio::process::Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .await?;

    if !output.status.success() {
        return Ok(TailscaleStatus {
            running: false,
            tailscale_ip: None,
            tailnet: None,
            hostname: None,
            funnel_active: false,
            funnel_url: None,
        });
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;

    let self_node = &json["Self"];
    let tailscale_ip = self_node["TailscaleIPs"]
        .as_array()
        .and_then(|ips| ips.first())
        .and_then(|ip| ip.as_str())
        .map(String::from);

    let hostname = self_node["HostName"].as_str().map(String::from);

    let tailnet = json["MagicDNSSuffix"].as_str().map(String::from);

    Ok(TailscaleStatus {
        running: true,
        tailscale_ip,
        tailnet,
        hostname,
        funnel_active: false, // Updated by funnel check
        funnel_url: None,
    })
}

/// Start Tailscale Funnel to expose the gateway publicly.
pub(crate) async fn start_funnel(port: u16) -> anyhow::Result<String> {
    info!("Starting Tailscale Funnel on port {port}");

    let output = tokio::process::Command::new("tailscale")
        .args(["funnel", &port.to_string()])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to start Tailscale Funnel: {stderr}");
    }

    // Get the Funnel URL
    let status = get_status().await?;
    let url = if let (Some(hostname), Some(tailnet)) = (&status.hostname, &status.tailnet) {
        format!("https://{hostname}.{tailnet}")
    } else {
        "https://<unknown>.ts.net".to_owned()
    };

    info!("Tailscale Funnel active at {url}");
    Ok(url)
}

/// Stop Tailscale Funnel.
pub(crate) async fn stop_funnel(port: u16) -> anyhow::Result<()> {
    info!("Stopping Tailscale Funnel on port {port}");

    let output = tokio::process::Command::new("tailscale")
        .args(["funnel", "--off", &port.to_string()])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("Failed to stop Tailscale Funnel: {stderr}");
    }

    Ok(())
}

/// Look up a Tailscale identity by remote address (IP:port).
pub(crate) async fn whois(remote_addr: &str) -> anyhow::Result<TailscaleIdentity> {
    let output = tokio::process::Command::new("tailscale")
        .args(["whois", "--json", remote_addr])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Tailscale whois failed: {stderr}");
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;

    let user_profile = &json["UserProfile"];
    let node = &json["Node"];

    Ok(TailscaleIdentity {
        login_name: user_profile["LoginName"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned(),
        name: user_profile["DisplayName"]
            .as_str()
            .unwrap_or("Unknown")
            .to_owned(),
        node_name: node["Name"].as_str().unwrap_or("unknown").to_owned(),
        node_ip: remote_addr.to_owned(),
    })
}

/// Resolve Tailscale identity to a gateway scope using the config mapping.
pub(crate) fn resolve_scope(
    identity: &TailscaleIdentity,
    config: &TailscaleConfig,
) -> Option<String> {
    config
        .user_scopes
        .iter()
        .find(|us| us.login == identity.login_name)
        .map(|us| us.scope.clone())
}
