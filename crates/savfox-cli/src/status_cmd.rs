//! `savfox status` — authenticated Gateway status for humans and automation.

use std::fmt::Write as _;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use clap::{Parser, ValueEnum};
use serde_json::Value;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum StatusFormat {
    Table,
    Json,
}

/// Show system status overview.
#[derive(Debug, Parser)]
pub struct StatusCommand {
    /// Gateway host.
    #[clap(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Gateway port.
    #[clap(long, default_value_t = 18881)]
    pub port: u16,
    /// Gateway Bearer token.
    #[clap(long, env = "SAVFOX_GATEWAY_TOKEN")]
    pub token: Option<String>,
    /// Output format.
    #[clap(long, value_enum, default_value_t = StatusFormat::Table)]
    pub format: StatusFormat,
}

pub async fn run(cmd: StatusCommand) -> anyhow::Result<()> {
    let token = cmd
        .token
        .or_else(|| std::env::var("SAVFOX_TOKEN").ok())
        .filter(|value| !value.trim().is_empty())
        .context("gateway auth token is required; pass --token or set SAVFOX_GATEWAY_TOKEN")?;
    let endpoint = gateway_status_endpoint(&cmd.host, cmd.port)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build Gateway HTTP client")?;

    let response = client
        .get(endpoint.clone())
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("Gateway is unreachable at {endpoint}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .context("failed to read Gateway status response")?;

    if !status.is_success() {
        let detail = String::from_utf8_lossy(&body);
        let detail = detail.chars().take(2_000).collect::<String>();
        bail!("Gateway status request failed with HTTP {status}: {detail}");
    }

    let payload: Value =
        serde_json::from_slice(&body).context("Gateway returned invalid status JSON")?;
    match cmd.format {
        StatusFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .context("failed to format Gateway status JSON")?
        ),
        StatusFormat::Table => print!("{}", format_status_table(endpoint.as_str(), &payload)),
    }

    Ok(())
}

fn gateway_status_endpoint(host: &str, port: u16) -> anyhow::Result<reqwest::Url> {
    let host = host.trim();
    if host.is_empty() {
        bail!("Gateway host must not be empty");
    }

    let normalized_host = match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(_)) => format!("[{host}]"),
        _ => host.to_owned(),
    };
    let mut endpoint = reqwest::Url::parse("http://127.0.0.1/api/status")
        .context("failed to construct Gateway status URL")?;
    endpoint
        .set_host(Some(&normalized_host))
        .map_err(|_| anyhow!("invalid Gateway host: {host}"))?;
    endpoint
        .set_port(Some(port))
        .map_err(|()| anyhow!("invalid Gateway port: {port}"))?;
    Ok(endpoint)
}

fn format_status_table(endpoint: &str, payload: &Value) -> String {
    let connected_clients = payload
        .get("connected_clients")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let sessions = payload
        .get("session_ids")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let plugin_routes = payload
        .get("plugin_routes")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);

    let mut output = String::new();
    let _ = writeln!(output, "{:<20} ONLINE", "Gateway");
    let _ = writeln!(output, "{:<20} {endpoint}", "Endpoint");
    let _ = writeln!(output, "{:<20} {connected_clients}", "Connected clients");
    let _ = writeln!(output, "{:<20} {}", "Active sessions", sessions.len());
    let _ = writeln!(output, "{:<20} {plugin_routes}", "Plugin routes");

    if !sessions.is_empty() {
        let _ = writeln!(output, "\nSessions");
        for session in sessions.iter().filter_map(Value::as_str) {
            let _ = writeln!(output, "  {session}");
        }
    }

    if let Some(audit) = payload.get("security_audit") {
        let _ = writeln!(output, "\nSecurity audit");
        let _ = writeln!(output, "  {audit}");
    }

    output
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{StatusCommand, StatusFormat, format_status_table, gateway_status_endpoint, run};

    #[tokio::test]
    async fn unreachable_gateway_returns_an_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("test should reserve an ephemeral TCP port");
        let port = listener
            .local_addr()
            .expect("test listener should have a local address")
            .port();
        drop(listener);

        let result = run(StatusCommand {
            host: "127.0.0.1".to_owned(),
            port,
            token: Some("test-token".to_owned()),
            format: StatusFormat::Json,
        })
        .await;

        assert!(
            result.is_err(),
            "offline Gateway must produce a non-zero CLI result"
        );
    }

    #[test]
    fn endpoint_builder_supports_ipv6_and_rejects_path_injection() {
        assert_eq!(
            gateway_status_endpoint("::1", 18881)
                .expect("IPv6 loopback should produce a valid Gateway URL")
                .as_str(),
            "http://[::1]:18881/api/status"
        );
        assert!(gateway_status_endpoint("localhost/other", 18881).is_err());
    }

    #[test]
    fn table_format_includes_status_counts_and_sessions() {
        let output = format_status_table(
            "http://127.0.0.1:18881/api/status",
            &json!({
                "connected_clients": 2,
                "session_ids": ["session-a", "session-b"],
                "plugin_routes": [{"path": "/plugins/example"}],
                "security_audit": {"high": 0},
            }),
        );

        assert!(output.contains("Gateway              ONLINE"));
        assert!(output.contains("Connected clients    2"));
        assert!(output.contains("Active sessions      2"));
        assert!(output.contains("session-a"));
        assert!(output.contains("Plugin routes        1"));
    }
}
