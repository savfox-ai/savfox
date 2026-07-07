//! Applet DID-proof login + session grant tracking.
//!
//! CKP-0008 personal agent runtime must use agent_key_proof plus DPoP and
//! must not call this path.

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use cokret::http_client::Client;
use cokret::{AuthManager, DeviceId, Did, Ed25519MoveSigner};

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

/// Run applet `AuthManager::login_did_proof` against the given HTTP client.
///
/// `audience` is the Cokret server's service DID.
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
