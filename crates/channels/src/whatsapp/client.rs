/// Send a text message via the WhatsApp Cloud API.
pub async fn send_message(
    client: &reqwest::Client,
    phone_number_id: &str,
    access_token: &str,
    to: &str,
    text: &str,
) -> anyhow::Result<()> {
    let url = format!("https://graph.facebook.com/v18.0/{phone_number_id}/messages");
    let body = serde_json::json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": to,
        "type": "text",
        "text": {
            "preview_url": false,
            "body": text
        }
    });

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    crate::http::warn_on_error(response, "WhatsApp API error").await;
    Ok(())
}

/// Verify a WhatsApp webhook signature using HMAC-SHA256 in constant time.
///
/// The `signature` value typically has a `sha256=` prefix which is stripped
/// automatically before comparison.
#[must_use]
pub fn verify_webhook_signature(app_secret: &str, body: &[u8], signature: &str) -> bool {
    crate::http::verify_webhook_hmac(app_secret, body, signature)
}
