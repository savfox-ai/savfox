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

/// Verify a WhatsApp webhook signature using HMAC-SHA256 in constant time.
///
/// The `signature` value typically has a `sha256=` prefix which is stripped
/// automatically before comparison.
#[must_use]
pub fn verify_webhook_signature(app_secret: &str, body: &[u8], signature: &str) -> bool {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    use subtle::ConstantTimeEq;

    let expected = signature.strip_prefix("sha256=").unwrap_or(signature);

    let mut mac = match Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = hex::encode(mac.finalize().into_bytes());

    bool::from(computed.as_bytes().ct_eq(expected.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compute(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn verify_accepts_raw_hex_and_prefixed() {
        let sig = compute("k", b"payload");
        assert!(verify_webhook_signature("k", b"payload", &sig));
        let prefixed = format!("sha256={sig}");
        assert!(verify_webhook_signature("k", b"payload", &prefixed));
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let sig = compute("k1", b"payload");
        assert!(!verify_webhook_signature("k2", b"payload", &sig));
    }

    #[test]
    fn verify_rejects_modified_body() {
        let sig = compute("k", b"payload");
        assert!(!verify_webhook_signature("k", b"payloaD", &sig));
    }

    #[test]
    fn verify_rejects_truncated_signature() {
        let sig = compute("k", b"payload");
        assert!(!verify_webhook_signature(
            "k",
            b"payload",
            &sig[..sig.len() - 2]
        ));
    }
}
