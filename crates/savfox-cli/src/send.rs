use std::time::Duration;

use clap::Parser;
use serde_json::json;

/// Send a message to a chat channel via the gateway REST API.
#[derive(Debug, Parser)]
pub struct SendCommand {
    /// Target channel type (discord, telegram, slack, etc.).
    #[clap(long)]
    pub channel: String,

    /// Target recipient (channel ID, username, etc.).
    #[clap(long)]
    pub to: String,

    /// Message to send.
    pub message: String,

    /// Gateway URL to connect to.
    #[clap(long, default_value = "http://localhost:18881")]
    pub gateway_url: String,

    /// Authentication token.
    #[clap(long, env = "SAVFOX_TOKEN")]
    pub token: Option<String>,
}

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run_send(cmd: SendCommand) -> anyhow::Result<()> {
    // Build the channel identifier in the format the gateway expects,
    // e.g. "discord:12345" or "telegram:67890".
    let channel_target = format!("{}:{}", cmd.channel, cmd.to);

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;

    let url = format!("{}/api/message", cmd.gateway_url);

    let payload = json!({
        "channel": channel_target,
        "text": cmd.message,
    });

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&payload);

    if let Some(ref token) = cmd.token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }

    let response = request.send().await.map_err(|e| {
        if e.is_connect() {
            anyhow::anyhow!(
                "Could not connect to gateway at {}. Is the gateway running?",
                cmd.gateway_url
            )
        } else if e.is_timeout() {
            anyhow::anyhow!(
                "Request to gateway at {} timed out after {}s",
                cmd.gateway_url,
                REQUEST_TIMEOUT.as_secs()
            )
        } else {
            anyhow::anyhow!("Request failed: {e}")
        }
    })?;

    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .unwrap_or_else(|_| json!({"error": "failed to parse response"}));

    if status.is_success() {
        println!(
            "Message sent to {}:{} via gateway at {}",
            cmd.channel, cmd.to, cmd.gateway_url
        );
        if let Some(s) = body.get("status").and_then(|v| v.as_str()) {
            println!("Status: {s}");
        }
    } else {
        let error_msg = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!(
            "Failed to send message (HTTP {}): {}",
            status.as_u16(),
            error_msg
        );
    }

    Ok(())
}
