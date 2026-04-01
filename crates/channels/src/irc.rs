use tracing::warn;

/// Send a message via an IRC bridge HTTP API.
///
/// IRC does not have a native HTTP API; this calls a local IRC bridge service
/// at `bridge_url` which forwards the message to the given channel.
pub async fn send_message(
    client: &reqwest::Client,
    bridge_url: &str,
    channel: &str,
    text: &str,
) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "channel": channel,
        "message": text,
    });

    let url = format!("{bridge_url}/send");
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "IRC bridge error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}
