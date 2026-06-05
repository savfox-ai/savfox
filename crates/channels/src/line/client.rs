use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Send a reply message via the LINE Messaging API.
///
/// Uses the reply token from the incoming webhook event.
pub async fn reply_message(
    client: &reqwest::Client,
    channel_token: &str,
    reply_token: &str,
    text: &str,
) -> anyhow::Result<()> {
    let url = "https://api.line.me/v2/bot/message/reply";
    let body = serde_json::json!({
        "replyToken": reply_token,
        "messages": [{
            "type": "text",
            "text": text,
        }]
    });

    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {channel_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    crate::http::warn_on_error(response, "LINE API error").await;
    Ok(())
}

/// Push a message via the LINE Messaging API (no reply token needed).
pub async fn push_message(
    client: &reqwest::Client,
    channel_token: &str,
    user_id: &str,
    text: &str,
) -> anyhow::Result<()> {
    let url = "https://api.line.me/v2/bot/message/push";
    let body = serde_json::json!({
        "to": user_id,
        "messages": [{
            "type": "text",
            "text": text,
        }]
    });

    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {channel_token}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    crate::http::warn_on_error(response, "LINE Push API error").await;
    Ok(())
}

/// Verify a LINE webhook signature using HMAC-SHA256 + base64 in constant time.
#[must_use]
pub fn verify_signature(channel_secret: &str, body: &[u8], signature: &str) -> bool {
    use subtle::ConstantTimeEq;
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(channel_secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    let computed = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    bool::from(computed.as_bytes().ct_eq(signature.trim().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compute(secret: &str, body: &[u8]) -> String {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
        mac.update(body);
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    }

    #[test]
    fn verify_accepts_correct_signature() {
        let sig = compute("secret", b"event-body");
        assert!(verify_signature("secret", b"event-body", &sig));
    }

    #[test]
    fn verify_strips_whitespace() {
        let sig = compute("secret", b"event-body");
        let padded = format!("  {sig}  ");
        assert!(verify_signature("secret", b"event-body", &padded));
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let sig = compute("secret-a", b"event-body");
        assert!(!verify_signature("secret-b", b"event-body", &sig));
    }

    #[test]
    fn verify_rejects_tampered_body() {
        let sig = compute("secret", b"event-body");
        assert!(!verify_signature("secret", b"event-bod!", &sig));
    }
}
