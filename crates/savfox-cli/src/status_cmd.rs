//! `savfox status` — System overview showing gateway status, channels, sessions, providers.

use clap::Parser;

/// Show system status overview
#[derive(Debug, Parser)]
pub struct StatusCommand {
    /// Gateway host
    #[clap(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Gateway port
    #[clap(long, default_value_t = 18881)]
    pub port: u16,
    /// Gateway auth token
    #[clap(long, env = "SAVFOX_TOKEN")]
    pub token: Option<String>,
    /// Output format (table, json)
    #[clap(long, default_value = "table")]
    pub format: String,
}

pub async fn run(cmd: StatusCommand) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!("http://{}:{}", cmd.host, cmd.port);
    eprintln!("Checking gateway status at {url}...");

    // Try health endpoint first
    match reqwest::get(format!("{url}/health")).await {
        Ok(resp) => {
            if resp.status().is_success() {
                eprintln!("Gateway: ONLINE");
                if let Ok(body) = resp.text().await {
                    eprintln!("Health: {body}");
                }
            } else {
                eprintln!("Gateway: ERROR (status {})", resp.status());
            }
        }
        Err(e) => {
            eprintln!("Gateway: OFFLINE ({e})");
            eprintln!(
                "\nTip: Start the gateway with: savfox gateway --port {}",
                cmd.port
            );
        }
    }

    // TODO: Connect via WS and get detailed status (channels, sessions, providers)
    eprintln!("\n(Detailed WS status not yet implemented)");

    Ok(())
}
