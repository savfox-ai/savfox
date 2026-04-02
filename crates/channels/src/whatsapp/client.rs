use tracing::warn;

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

    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        warn!(
            "WhatsApp API error: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}

/// Verify a WhatsApp webhook signature using HMAC-SHA256.
///
/// The `signature` value typically has a `sha256=` prefix which is stripped
/// automatically before comparison.
#[must_use] 
pub fn verify_webhook_signature(app_secret: &str, body: &[u8], signature: &str) -> bool {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    let expected = signature.strip_prefix("sha256=").unwrap_or(signature);

    let mut mac = match Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = hex::encode(mac.finalize().into_bytes());

    computed == expected
}
