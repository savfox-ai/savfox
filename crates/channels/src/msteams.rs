use tracing::warn;

/// Send a message via a Microsoft Teams webhook URL.
pub async fn send_webhook_message(
    client: &reqwest::Client,
    webhook_url: &str,
    text: &str,
) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "type": "message",
        "text": text,
    });

    let response = client
        .post(webhook_url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "Teams API error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}
