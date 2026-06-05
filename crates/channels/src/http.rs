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
/// Compares the computed hex digest against `expected_hex` in **constant time**
/// to prevent timing side-channel attacks on the signature. The expected value
/// may optionally carry a `sha256=` prefix which is stripped before comparison.
#[must_use]
pub fn verify_webhook_hmac(secret: &str, body: &[u8], expected_hex: &str) -> bool {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    use subtle::ConstantTimeEq;

    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(body);
    let result = mac.finalize();
    let computed = hex::encode(result.into_bytes());

    // Support both raw hex and "sha256=" prefix
    let expected = expected_hex.strip_prefix("sha256=").unwrap_or(expected_hex);
    bool::from(computed.as_bytes().ct_eq(expected.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "shhh-secret";
    // Pre-computed HMAC-SHA256("shhh-secret", "hello"):
    // openssl dgst -sha256 -hmac "shhh-secret" -hex (no trailing newline on body).
    // Computed at test time below; the test does not hardcode the digest.

    fn compute_expected(body: &[u8]) -> String {
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).expect("HMAC accepts any key size");
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn verify_accepts_valid_hex() {
        let body = b"hello";
        let expected = compute_expected(body);
        assert!(verify_webhook_hmac(SECRET, body, &expected));
    }

    #[test]
    fn verify_accepts_sha256_prefix() {
        let body = b"hello";
        let expected = format!("sha256={}", compute_expected(body));
        assert!(verify_webhook_hmac(SECRET, body, &expected));
    }

    #[test]
    fn verify_rejects_mismatched_signature() {
        let body = b"hello";
        let mut wrong = compute_expected(body);
        // Flip the first hex digit
        let first = wrong.remove(0);
        let flipped = if first == '0' { '1' } else { '0' };
        wrong.insert(0, flipped);
        assert!(!verify_webhook_hmac(SECRET, body, &wrong));
    }

    #[test]
    fn verify_rejects_modified_body() {
        let original = b"hello";
        let tampered = b"hellO";
        let expected = compute_expected(original);
        assert!(!verify_webhook_hmac(SECRET, tampered, &expected));
    }

    #[test]
    fn verify_rejects_empty_signature() {
        assert!(!verify_webhook_hmac(SECRET, b"hello", ""));
    }

    #[test]
    fn verify_rejects_wrong_length_signature() {
        // Truncated digest should not be accepted; ct_eq returns false on length mismatch.
        let expected = compute_expected(b"hello");
        let truncated = &expected[..expected.len() - 1];
        assert!(!verify_webhook_hmac(SECRET, b"hello", truncated));
    }
}
