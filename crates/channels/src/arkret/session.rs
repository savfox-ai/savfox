//! Session grant state shared by Arkret client integrations.

use arkret::{DeviceId, DidCoreId};
use chrono::{DateTime, Utc};

/// One-shot view of an active Arkret session grant.
#[derive(Debug, Clone)]
pub struct ArkretSession {
    pub session_grant: String,
    pub expires_at: DateTime<Utc>,
    pub principal_did: DidCoreId,
    pub device_id: Option<DeviceId>,
}

impl ArkretSession {
    /// True if the session has expired (or is within `skew` seconds of
    /// expiring). Callers MUST re-login before the window closes.
    #[must_use]
    pub fn is_near_expiry(&self, skew_secs: i64) -> bool {
        let now = Utc::now();
        let threshold = self.expires_at - chrono::Duration::seconds(skew_secs);
        now >= threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_expiry_returns_true_when_past_threshold() {
        let s = ArkretSession {
            session_grant: "g".into(),
            expires_at: Utc::now() + chrono::Duration::seconds(10),
            principal_did: DidCoreId::new("did:web:alice.example".to_owned())
                .expect("test DID should parse"),
            device_id: Some(
                DeviceId::new("ak:device:01904100-0000-7000-8000-000000000001".to_owned())
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
        let s = ArkretSession {
            session_grant: "g".into(),
            expires_at: Utc::now() - chrono::Duration::seconds(10),
            principal_did: DidCoreId::new("did:web:alice.example".to_owned())
                .expect("test DID should parse"),
            device_id: Some(
                DeviceId::new("ak:device:01904100-0000-7000-8000-000000000001".to_owned())
                    .expect("test device id should parse"),
            ),
        };
        assert!(s.is_near_expiry(0));
        assert!(s.is_near_expiry(60));
    }
}
