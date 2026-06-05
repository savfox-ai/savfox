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

    crate::http::warn_on_error(response, "Teams API error").await;
    Ok(())
}
