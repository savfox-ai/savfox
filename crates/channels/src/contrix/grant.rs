//! Capability grant loading + validation.
//!
//! Phase 8 (T8.E): load a pre-signed `cx.capability.grant` Event JSON from
//! disk, sanity-check it, and surface the `event_id` for use as
//! `authorization_ref` on outbound writes (spec applet-integration.md §8 +
//! authz/capabilities.md).
//!
//! Grants are issued by Realm admins out-of-band; savfox doesn't
//! request / renew them. The operator drops the signed Event JSON into
//! `$SAVFOX_HOME/contrix/grants/<account_id>.json` (or wherever
//! `grant_event_path` points). On startup we:
//!
//! 1. Deserialize the JSON into a [`contrix_core::Event`].
//! 2. Deserialize `event.content` into a [`contrix::CapabilityGrant`].
//! 3. Require production-shaped proofs and validate proof bindings
//!    (digest matches content).
//! 4. Sanity-check subject / realm / expiry against expected values.
//! 5. Return [`ContrixGrant`] holding the event_id + the grant fields.

use std::path::Path;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use contrix::CapabilityGrant;
use contrix_core::Event;

/// Loaded capability grant ready for use as `authorization_ref` on
/// outbound writes.
#[derive(Debug, Clone)]
pub struct ContrixGrant {
    /// Event id of the `cx.capability.grant` Event — value goes into
    /// `Event.authorization_ref` on every outbound write.
    pub event_id: String,
    /// Grant `subject` field — must match the writer's `actor_id`.
    pub subject: String,
    /// Grant `issuer` field.
    pub issuer: String,
    /// Optional Realm scope.
    pub realm_id: Option<String>,
    /// Authorized actions (e.g. `["cx.message.create"]`).
    pub actions: Vec<String>,
    /// Expiry, if any.
    pub expires_at: Option<DateTime<Utc>>,
}

impl ContrixGrant {
    /// True if the grant has not expired (or has no expiry).
    pub fn is_active(&self) -> bool {
        match &self.expires_at {
            Some(t) => Utc::now() < *t,
            None => true,
        }
    }

    /// True if the grant covers the given action.
    pub fn covers_action(&self, action: &str) -> bool {
        self.actions.iter().any(|a| a == action)
    }
}

/// Load + verify a `cx.capability.grant` Event JSON file.
///
/// Performs these checks:
/// * Event JSON is parseable.
/// * `event.kind == "cx.capability.grant"`.
/// * Event content deserializes into [`CapabilityGrant`].
/// * Event has at least one production-shaped proof.
/// * `event.validate_proof_bindings()` passes (proof digest matches body).
/// * `grant.subject == expected_subject` (caller's DID).
/// * If `expected_realm` is provided, the grant's `space_id` matches.
/// * Grant has not expired.
pub async fn load_and_verify_grant(
    path: &Path,
    expected_subject: &str,
    expected_realm: Option<&str>,
) -> anyhow::Result<ContrixGrant> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read capability grant {}", path.display()))?;
    let event: Event = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse capability grant {}", path.display()))?;

    if event.kind != "cx.capability.grant" {
        anyhow::bail!(
            "capability grant {}: kind must be 'cx.capability.grant', got '{}'",
            path.display(),
            event.kind
        );
    }

    // Proof binding (digest-content tie). Real cryptographic signature
    // verification (issuer DID document lookup) is still out of scope here,
    // but unsigned or dev-proof grants must not be accepted.
    if event.proofs.is_empty() {
        anyhow::bail!("capability grant {}: missing proofs", path.display());
    }
    for proof in &event.proofs {
        proof
            .validate_production()
            .map_err(|err| anyhow::anyhow!("grant proof is not production-grade: {err}"))?;
    }
    event
        .validate_proof_bindings()
        .map_err(|err| anyhow::anyhow!("grant proof binding invalid: {err}"))?;

    let grant: CapabilityGrant = serde_json::from_value(event.content.clone())
        .with_context(|| format!("decode CapabilityGrant content in {}", path.display()))?;

    if event.actor_id.as_str() != grant.issuer.as_str() {
        anyhow::bail!(
            "capability grant {}: event actor '{}' does not match grant issuer '{}'",
            path.display(),
            event.actor_id.as_str(),
            grant.issuer.as_str()
        );
    }
    let issuer_vm_prefix = format!("{}#", grant.issuer.as_str());
    if !event.proofs.iter().any(|proof| {
        proof.verification_method == grant.issuer.as_str()
            || proof.verification_method.starts_with(&issuer_vm_prefix)
    }) {
        anyhow::bail!(
            "capability grant {}: no proof verification_method belongs to issuer '{}'",
            path.display(),
            grant.issuer.as_str()
        );
    }

    if grant.subject.as_str() != expected_subject {
        anyhow::bail!(
            "capability grant {}: subject '{}' does not match expected '{}'",
            path.display(),
            grant.subject.as_str(),
            expected_subject
        );
    }

    let realm_id = grant.space_id.as_ref().map(|s| s.as_str().to_owned());
    if let Some(expected) = expected_realm {
        match &realm_id {
            Some(actual) if actual.eq_ignore_ascii_case(expected) => {}
            Some(actual) => anyhow::bail!(
                "capability grant {}: realm '{}' does not match expected '{}'",
                path.display(),
                actual,
                expected
            ),
            None => anyhow::bail!(
                "capability grant {}: no realm scope, expected '{}'",
                path.display(),
                expected
            ),
        }
    }

    if let Some(exp) = grant.expires_at
        && Utc::now() >= exp
    {
        anyhow::bail!("capability grant {}: expired at {}", path.display(), exp);
    }

    Ok(ContrixGrant {
        event_id: event.event_id.as_str().to_owned(),
        subject: grant.subject.as_str().to_owned(),
        issuer: grant.issuer.as_str().to_owned(),
        realm_id,
        actions: grant.actions,
        expires_at: grant.expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use contrix_core::Hash;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_path(label: &str) -> std::path::PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "savfox-contrix-test-grant-{}-{}-{}.json",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ))
    }

    /// Minimal `cx.capability.grant` Event JSON with a production-shaped proof
    /// whose digest binds to the envelope. The test proof does not verify a
    /// real JWS signature because this layer has no DID document resolver.
    fn make_grant_event(
        subject: &str,
        realm: Option<&str>,
        action: &str,
        expires: Option<DateTime<Utc>>,
    ) -> serde_json::Value {
        let mut content = serde_json::Map::new();
        content.insert(
            "id".into(),
            json!("cx:grant:01904100-0000-7000-8000-000000000abc"),
        );
        content.insert("issuer".into(), json!("did:webvh:example.com:admin"));
        content.insert("subject".into(), json!(subject));
        content.insert("actions".into(), json!([action]));
        content.insert("resources".into(), json!([]));
        if let Some(realm) = realm {
            content.insert("space_id".into(), json!(realm));
        }
        if let Some(exp) = expires {
            content.insert("expires_at".into(), json!(exp.to_rfc3339()));
        }
        let mut event = json!({
            "event_id": "cx:event:01904100-0000-7000-8000-000000000def",
            "kind": "cx.capability.grant",
            "realm_id": realm.unwrap_or("cx:realm:01904100-0000-7000-8000-000000000001"),
            "actor_id": "did:webvh:example.com:admin",
            "actor_seq": 1,
            "created_at": "2026-05-27T00:00:00Z",
            "hlc": "000000000000-0000-00000000",
            "prev_refs": [],
            "refs": [],
            "payload": serde_json::Value::Object(content),
            "proofs": []
        });
        let parsed: Event = serde_json::from_value(event.clone()).expect("event");
        let digest = Hash::new(parsed.event_digest().expect("digest")).expect("hash");
        event["proofs"] = json!([{
            "kind": "detached_jws",
            "alg": "EdDSA",
            "verification_method": "did:webvh:example.com:admin#key-1",
            "payload_digest": digest.as_str(),
            "created_at": "2026-05-27T00:00:00Z",
            "jws": "test.detached.signature"
        }]);
        event
    }

    #[tokio::test]
    async fn loads_valid_grant() {
        let path = unique_path("valid");
        let ev = make_grant_event(
            "did:webvh:example.org:agents:support",
            Some("cx:realm:01904100-0000-7000-8000-000000000001"),
            "cx.message.create",
            None,
        );
        tokio::fs::write(&path, serde_json::to_vec_pretty(&ev).unwrap())
            .await
            .expect("write");
        let grant = load_and_verify_grant(
            &path,
            "did:webvh:example.org:agents:support",
            Some("cx:realm:01904100-0000-7000-8000-000000000001"),
        )
        .await
        .expect("load");
        assert_eq!(grant.subject, "did:webvh:example.org:agents:support");
        assert!(grant.covers_action("cx.message.create"));
        assert!(grant.is_active());
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn rejects_subject_mismatch() {
        let path = unique_path("subj");
        let ev = make_grant_event(
            "did:webvh:example.org:agents:other",
            Some("cx:realm:01904100-0000-7000-8000-000000000001"),
            "cx.message.create",
            None,
        );
        tokio::fs::write(&path, serde_json::to_vec(&ev).unwrap())
            .await
            .expect("write");
        let err = load_and_verify_grant(&path, "did:webvh:example.org:agents:support", None)
            .await
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("does not match expected"));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn rejects_expired_grant() {
        let path = unique_path("exp");
        let ev = make_grant_event(
            "did:webvh:example.org:agents:support",
            None,
            "cx.message.create",
            Some(Utc::now() - chrono::Duration::seconds(60)),
        );
        tokio::fs::write(&path, serde_json::to_vec(&ev).unwrap())
            .await
            .expect("write");
        let err = load_and_verify_grant(&path, "did:webvh:example.org:agents:support", None)
            .await
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("expired"));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn rejects_wrong_kind() {
        let path = unique_path("kind");
        let mut ev = make_grant_event(
            "did:webvh:example.org:agents:support",
            None,
            "cx.message.create",
            None,
        );
        ev["kind"] = json!("cx.message.create");
        tokio::fs::write(&path, serde_json::to_vec(&ev).unwrap())
            .await
            .expect("write");
        let err = load_and_verify_grant(&path, "did:webvh:example.org:agents:support", None)
            .await
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("cx.capability.grant"));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn rejects_missing_proofs() {
        let path = unique_path("proof");
        let mut ev = make_grant_event(
            "did:webvh:example.org:agents:support",
            None,
            "cx.message.create",
            None,
        );
        ev["proofs"] = json!([]);
        tokio::fs::write(&path, serde_json::to_vec(&ev).unwrap())
            .await
            .expect("write");
        let err = load_and_verify_grant(&path, "did:webvh:example.org:agents:support", None)
            .await
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("missing proofs"));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn rejects_issuer_proof_mismatch() {
        let path = unique_path("issuer");
        let mut ev = make_grant_event(
            "did:webvh:example.org:agents:support",
            None,
            "cx.message.create",
            None,
        );
        ev["proofs"][0]["verification_method"] = json!("did:webvh:other.example:admin#key-1");
        tokio::fs::write(&path, serde_json::to_vec(&ev).unwrap())
            .await
            .expect("write");
        let err = load_and_verify_grant(&path, "did:webvh:example.org:agents:support", None)
            .await
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("verification_method"));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[test]
    fn covers_action_and_is_active() {
        let g = ContrixGrant {
            event_id: "cx:event:x".into(),
            subject: "did:webvh:s".into(),
            issuer: "did:webvh:i".into(),
            realm_id: None,
            actions: vec!["cx.message.create".into()],
            expires_at: Some(Utc::now() + chrono::Duration::seconds(60)),
        };
        assert!(g.covers_action("cx.message.create"));
        assert!(!g.covers_action("cx.flow.create"));
        assert!(g.is_active());
    }
}
