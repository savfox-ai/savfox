/// Check an HTTP response, returning an error with body details on failure.
pub async fn check_response(response: reqwest::Response, context: &str) -> anyhow::Result<()> {
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    anyhow::bail!("{context}: HTTP {status}: {body}")
}

/// Log a warning if the HTTP response indicates failure. Does not bail.
pub async fn warn_on_error(response: reqwest::Response, context: &str) {
    if !response.status().is_success() {
        let status = response.status();
        let body = response.bytes().await.unwrap_or_default();
        tracing::warn!(
            "{context}: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
}

/// Verify a webhook payload using HMAC-SHA256.
///
/// Compares the computed hex digest against `expected_hex`. The expected value
/// may optionally carry a `sha256=` prefix which is stripped before comparison.
pub fn verify_webhook_hmac(secret: &str, body: &[u8], expected_hex: &str) -> bool {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(body);
    let result = mac.finalize();
    let computed = hex::encode(result.into_bytes());

    // Support both raw hex and "sha256=" prefix
    let expected = expected_hex.strip_prefix("sha256=").unwrap_or(expected_hex);
    computed == expected
}
