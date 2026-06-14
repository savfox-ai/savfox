//! DID-proof login + session grant tracking.
//!
//! Phase 8 (T8.B): wraps SDK `AuthManager::login_did_proof` (S-2) so the
//! gateway runtime can boot a Cokret account / applet bot from a signer
//! instead of a static `access_token`. Returns a [`CokretSession`] that
//! tracks `expires_at` so callers can decide when to refresh.

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use cokret::{AuthManager, Ed25519MoveSigner};
use cokret_http_client::Client;
use cokret_identifiers::{DeviceId, Did};

/// One-shot session state produced by [`login_with_signer`].
#[derive(Debug, Clone)]
pub struct CokretSession {
    pub session_grant: String,
    pub expires_at: DateTime<Utc>,
    pub principal_did: Did,
    pub device_id: Option<DeviceId>,
}

impl CokretSession {
    /// True if the session has expired (or is within `skew` seconds of
    /// expiring). Callers MUST re-login before the window closes.
    #[must_use]
    pub fn is_near_expiry(&self, skew_secs: i64) -> bool {
        let now = Utc::now();
        let threshold = self.expires_at - chrono::Duration::seconds(skew_secs);
        now >= threshold
    }
}

/// Run `AuthManager::login_did_proof` against the given HTTP client.
///
/// `audience` is the Cokret server's service DID. Typically the same
/// value as `CokretChannelConfig::cokret_server_did` (or, when missing,
/// derived from `service_did` / `base_url`).
pub async fn login_with_signer(
    http: &Client,
    signer: &Ed25519MoveSigner,
    principal_did: Did,
    device_id: DeviceId,
    challenge: &str,
    audience: &str,
) -> anyhow::Result<CokretSession> {
    let mut auth = AuthManager::default();
    let session = auth
        .login_did_proof(
            http,
            principal_did.clone(),
            device_id.clone(),
            signer,
            challenge,
            audience,
        )
        .await
        .map_err(|err| anyhow::anyhow!("login_did_proof failed: {err}"))
        .with_context(|| {
            format!(
                "cokret login_did_proof: principal={} audience={}",
                principal_did.as_str(),
                audience
            )
        })?;
    Ok(CokretSession {
        session_grant: session.session_grant,
        expires_at: session.expires_at,
        principal_did: session.principal_id,
        device_id: session.device_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_expiry_returns_true_when_past_threshold() {
        let s = CokretSession {
            session_grant: "g".into(),
            expires_at: Utc::now() + chrono::Duration::seconds(10),
            principal_did: Did::new("did:web:alice.example".to_owned())
                .expect("test DID should parse"),
            device_id: Some(
                DeviceId::new("ck:device:01904100-0000-7000-8000-000000000001".to_owned())
                    .expect("test device id should parse"),
            ),
        };
        // 10s until expiry; with 60s skew, "near expiry" is true.
        assert!(s.is_near_expiry(60));
        // With 1s skew, still 10s away → not near.
        assert!(!s.is_near_expiry(1));
    }

    #[test]
    fn near_expiry_handles_already_expired() {
        let s = CokretSession {
            session_grant: "g".into(),
            expires_at: Utc::now() - chrono::Duration::seconds(10),
            principal_did: Did::new("did:web:alice.example".to_owned())
                .expect("test DID should parse"),
            device_id: Some(
                DeviceId::new("ck:device:01904100-0000-7000-8000-000000000001".to_owned())
                    .expect("test device id should parse"),
            ),
        };
        assert!(s.is_near_expiry(0));
        assert!(s.is_near_expiry(60));
    }
}
