//! Capability grant loading + validation.
//!
//! Phase 8 (T8.E): load a pre-signed `ak.capability.grant` Event JSON from
//! disk, sanity-check it, and surface the `event_id` for use as
//! `authorization_ref` on outbound writes (spec applet-integration.md §8 +
//! authz/capabilities.md).
//!
//! Grants are issued by Realm admins out-of-band; savfox doesn't
//! request / renew them. The operator drops the signed Event JSON into
//! `$SAVFOX_HOME/arkret/grants/<account_id>.json` (or wherever
//! `grant_event_path` points). On startup we:
//!
//! 1. Deserialize the JSON into a [`arkret::Event`].
//! 2. Deserialize `event.payload` into a [`arkret::CapabilityGrant`].
//! 3. Require a production-shaped producer proof and validate proof bindings (digest matches
//!    content).
//! 4. Sanity-check subject / realm / effective validity window against expected values.
//! 5. Return [`ArkretGrant`] holding the event_id + the grant fields.

use std::path::Path;

use anyhow::Context as _;
use arkret::{
    CapabilityGrantPayload, CapabilitySubject, Did, Event, GrantConstraint, GrantConstraintKind,
    project_did_to_core_id,
};
use chrono::{DateTime, Utc};

/// Loaded capability grant ready for use as `authorization_ref` on
/// outbound writes.
#[derive(Debug, Clone)]
pub struct ArkretGrant {
    /// Event id of the `ak.capability.grant` Event — value goes into
    /// `Event.authorization_ref` on every outbound write.
    pub event_id: String,
    /// Grant `subject` field — must match the writer's `actor_id`.
    pub subject: String,
    /// Grant `issuer` field.
    pub issuer: String,
    /// Optional Realm scope.
    pub realm_id: Option<String>,
    /// Authorized actions (e.g. `["ak.message.create"]`).
    pub actions: Vec<String>,
    /// Constraints retained from the grant for consumers that need to inspect
    /// more than the precomputed validity window.
    pub constraints: Vec<GrantConstraint>,
    /// Effective activation time: the latest `not_before` across temporal constraints.
    pub not_before: Option<DateTime<Utc>>,
    /// Effective expiry: the earliest `expires_at` across temporal constraints.
    pub expires_at: Option<DateTime<Utc>>,
}

impl ArkretGrant {
    /// True if the grant's effective temporal window contains the current time.
    #[must_use]
    pub fn is_active(&self) -> bool {
        let now = Utc::now();
        self.not_before.is_none_or(|start| now >= start)
            && self.expires_at.is_none_or(|end| now < end)
    }

    /// True if the grant covers the given action.
    #[must_use]
    pub fn covers_action(&self, action: &str) -> bool {
        self.actions.iter().any(|a| a == action)
    }
}

/// Load + verify a `ak.capability.grant` Event JSON file.
///
/// Performs these checks:
/// * Event JSON is parseable.
/// * `event.kind == "ak.capability.grant"`.
/// * Event payload deserializes into [`CapabilityGrant`].
/// * Event has a production-shaped producer proof.
/// * `event.validate_proof_bindings()` passes (proof digest matches body).
/// * `grant.subject == expected_subject` (caller's DID).
/// * If `expected_realm` is provided, the grant's `realm_id` matches.
/// * Grant's effective temporal window is currently active.
pub async fn load_and_verify_grant(
    path: &Path,
    expected_subject: &str,
    expected_realm: Option<&str>,
) -> anyhow::Result<ArkretGrant> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read capability grant {}", path.display()))?;
    let event: Event = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse capability grant {}", path.display()))?;

    if event.kind != "ak.capability.grant" {
        anyhow::bail!(
            "capability grant {}: kind must be 'ak.capability.grant', got '{}'",
            path.display(),
            event.kind
        );
    }

    // Proof binding (digest-content tie). Real cryptographic signature
    // verification (issuer DID document lookup) is still out of scope here,
    // but unsigned or dev-proof grants must not be accepted.
    let producer = event.producer_proof.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "capability grant {}: missing producer_proof",
            path.display()
        )
    })?;
    producer
        .validate_production()
        .map_err(|err| anyhow::anyhow!("grant proof is not production-grade: {err}"))?;
    event
        .validate_proof_bindings_with_digest_suite(super::DIGEST_SUITE)
        .map_err(|err| anyhow::anyhow!("grant proof binding invalid: {err}"))?;

    let payload: CapabilityGrantPayload = serde_json::from_value(
        serde_json::to_value(&event.payload)
            .with_context(|| format!("encode CapabilityGrant payload in {}", path.display()))?,
    )
    .with_context(|| format!("decode CapabilityGrant payload in {}", path.display()))?;
    let grant = payload.grant;

    if event.actor_id != grant.issuer_id {
        anyhow::bail!(
            "capability grant {}: event actor '{}' does not match grant issuer '{}'",
            path.display(),
            event.actor_id,
            grant.issuer_id
        );
    }
    let producer_controller = producer
        .verification_method
        .as_str()
        .split_once('#')
        .and_then(|(controller, _)| Did::new(controller).ok())
        .and_then(|did| project_did_to_core_id(&did).ok());
    if producer_controller.as_ref() != Some(grant.issuer_id.signing_principal_id()) {
        anyhow::bail!(
            "capability grant {}: no proof verification_method belongs to issuer '{}'",
            path.display(),
            grant.issuer_id
        );
    }

    let subject = capability_subject_did(&grant.subject).ok_or_else(|| {
        anyhow::anyhow!(
            "capability grant {}: subject must be a DID to match expected '{}'",
            path.display(),
            expected_subject
        )
    })?;

    if subject != expected_subject {
        anyhow::bail!(
            "capability grant {}: subject '{}' does not match expected '{}'",
            path.display(),
            subject,
            expected_subject
        );
    }

    let realm_id = grant.realm_id.as_ref().map(|s| s.as_str().to_owned());
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

    let temporal_constraints = || {
        grant
            .constraints
            .iter()
            .filter(|constraint| constraint.constraint_kind == GrantConstraintKind::Temporal)
    };
    let not_before = temporal_constraints()
        .filter_map(|constraint| constraint.not_before)
        .max();
    let expires_at = temporal_constraints()
        .filter_map(|constraint| constraint.expires_at)
        .min();

    if let (Some(start), Some(end)) = (not_before, expires_at)
        && start >= end
    {
        anyhow::bail!(
            "capability grant {}: effective temporal window is empty ({} >= {})",
            path.display(),
            start,
            end
        );
    }

    if let Some(start) = not_before
        && Utc::now() < start
    {
        anyhow::bail!(
            "capability grant {}: not active until {}",
            path.display(),
            start
        );
    }
    if let Some(exp) = expires_at
        && Utc::now() >= exp
    {
        anyhow::bail!("capability grant {}: expired at {}", path.display(), exp);
    }

    Ok(ArkretGrant {
        event_id: event.event_id.as_str().to_owned(),
        subject: subject.to_owned(),
        issuer: grant.issuer_id.to_string(),
        realm_id,
        actions: grant.actions,
        constraints: grant.constraints,
        not_before,
        expires_at,
    })
}

fn capability_subject_did(subject: &CapabilitySubject) -> Option<&str> {
    match subject {
        CapabilitySubject::Actor(actor) => Some(actor.signing_principal_id().as_str()),
        CapabilitySubject::Condition(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;

    use super::*;

    const DEFAULT_REALM: &str = "ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI";
    const ISSUER_PRINCIPAL: &str = "ak:did_core:webvh:z6mkadminfixture";
    const STATION_PRINCIPAL: &str = "ak:did_core:web:principal.example";

    fn unique_path(label: &str) -> std::path::PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "savfox-arkret-test-grant-{}-{}-{}.json",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ))
    }

    /// Minimal `ak.capability.grant` Event JSON with a production-shaped proof
    /// whose digest binds to the envelope. The test proof does not verify a
    /// real JWS signature because this layer has no DID document resolver.
    fn make_grant_event(
        subject: &str,
        realm: Option<&str>,
        action: &str,
        expires: Option<DateTime<Utc>>,
    ) -> serde_json::Value {
        let realm_id = arkret::RealmId::new(realm.unwrap_or(DEFAULT_REALM)).expect("realm id");
        let issuer_account = arkret::AccountId::new(
            arkret::DidCoreId::new(ISSUER_PRINCIPAL).expect("issuer core id"),
            arkret::DidCoreId::new(STATION_PRINCIPAL).expect("station core id"),
        );
        let subject_account = arkret::AccountId::new(
            arkret::DidCoreId::new(subject.to_owned()).expect("subject core id"),
            arkret::DidCoreId::new(STATION_PRINCIPAL).expect("station core id"),
        );
        let constraints = expires
            .map(|exp| {
                let mut constraint = GrantConstraint::new(
                    GrantConstraintKind::Temporal,
                    arkret::GrantConstraintEffect::Allow,
                );
                constraint.expires_at = Some(exp);
                vec![constraint]
            })
            .unwrap_or_default();
        let payload = arkret::CapabilityGrantPayload {
            grant: arkret::CapabilityGrantCreateBody {
                schema: arkret::SchemaId::CAPABILITY_V1.to_owned(),
                realm_id: realm.map(|_| realm_id.clone()),
                issuer_id: arkret::ActorId::account(issuer_account.clone()),
                subject: CapabilitySubject::Actor(arkret::ActorId::account(subject_account)),
                actions: vec![action.to_owned()],
                resources: vec![arkret::WireResourceSelector::realm(realm_id.clone())],
                constraints,
                issuer_authority_refs: vec![arkret::IssuerAuthorityRef::RealmRoot {
                    realm_id: realm_id.clone(),
                    authority_event_ref: arkret::EventId::from_digest(
                        crate::arkret::DIGEST_SUITE,
                        [0x51; 32],
                    ),
                    authority_generation: 1,
                }],
                issued_at: DateTime::from_timestamp_millis(0).expect("epoch timestamp"),
            },
        };
        let intent: arkret::EventIntent = serde_json::from_value(json!({
            "kind": "ak.capability.grant",
            "scope_ref": arkret::ScopeRef::Realm { realm_id },
            "actor_id": arkret::ActorId::account(issuer_account),
            "created_at": "2026-05-27T00:00:00.000Z",
            "payload": payload,
        }))
        .expect("grant intent");
        let mut authored = intent
            .author_with_digest_suite(crate::arkret::DIGEST_SUITE)
            .expect("grant Event");
        let signer = arkret::signatures::Ed25519DetachedJwsSigner::new(
            ed25519_dalek::SigningKey::from_bytes(&[67; 32]),
            "did:webvh:z6mkadminfixture:admin.example#key-1",
        );
        arkret::signatures::sign_event(
            &mut authored,
            &signer,
            arkret::signatures::SignEventOptions::new(),
        )
        .expect("signed grant");
        let event = serde_json::to_value(authored.event()).expect("grant Event value");
        event
    }

    #[tokio::test]
    async fn loads_valid_grant() {
        let path = unique_path("valid");
        let expires_at = DateTime::from_timestamp_millis(Utc::now().timestamp_millis() + 60_000)
            .expect("future timestamp should be valid");
        let ev = make_grant_event(
            "ak:did_core:webvh:z6mksupportfixture",
            Some("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI"),
            "ak.message.create",
            Some(expires_at),
        );
        tokio::fs::write(
            &path,
            serde_json::to_vec_pretty(&ev).expect("grant event should serialize"),
        )
        .await
        .expect("write");
        let grant = load_and_verify_grant(
            &path,
            "ak:did_core:webvh:z6mksupportfixture",
            Some("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI"),
        )
        .await
        .expect("load");
        assert_eq!(grant.subject, "ak:did_core:webvh:z6mksupportfixture");
        assert!(grant.covers_action("ak.message.create"));
        assert_eq!(grant.constraints.len(), 1);
        assert_eq!(grant.expires_at, Some(expires_at));
        assert!(grant.is_active());
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn rejects_subject_mismatch() {
        let path = unique_path("subj");
        let ev = make_grant_event(
            "ak:did_core:webvh:z6mkotherfixture",
            Some("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI"),
            "ak.message.create",
            None,
        );
        tokio::fs::write(
            &path,
            serde_json::to_vec(&ev).expect("grant event should serialize"),
        )
        .await
        .expect("write");
        let err = load_and_verify_grant(&path, "ak:did_core:webvh:z6mksupportfixture", None)
            .await
            .expect_err("subject mismatch should fail");
        assert!(err.to_string().contains("does not match expected"));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn rejects_expired_grant() {
        let path = unique_path("exp");
        let ev = make_grant_event(
            "ak:did_core:webvh:z6mksupportfixture",
            None,
            "ak.message.create",
            Some(Utc::now() - chrono::Duration::seconds(60)),
        );
        tokio::fs::write(
            &path,
            serde_json::to_vec(&ev).expect("grant event should serialize"),
        )
        .await
        .expect("write");
        let err = load_and_verify_grant(&path, "ak:did_core:webvh:z6mksupportfixture", None)
            .await
            .expect_err("expired grant should fail");
        assert!(
            err.to_string().contains("expired"),
            "unexpected error: {err:#}"
        );
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn rejects_wrong_kind() {
        let path = unique_path("kind");
        let mut ev = make_grant_event(
            "ak:did_core:webvh:z6mksupportfixture",
            None,
            "ak.message.create",
            None,
        );
        ev["kind"] = json!("ak.message.create");
        tokio::fs::write(
            &path,
            serde_json::to_vec(&ev).expect("grant event should serialize"),
        )
        .await
        .expect("write");
        let err = load_and_verify_grant(&path, "ak:did_core:webvh:z6mksupportfixture", None)
            .await
            .expect_err("wrong event kind should fail");
        assert!(err.to_string().contains("ak.capability.grant"));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn rejects_missing_producer_proof() {
        let path = unique_path("proof");
        let mut ev = make_grant_event(
            "ak:did_core:webvh:z6mksupportfixture",
            None,
            "ak.message.create",
            None,
        );
        ev["producer_proof"] = serde_json::Value::Null;
        tokio::fs::write(
            &path,
            serde_json::to_vec(&ev).expect("grant event should serialize"),
        )
        .await
        .expect("write");
        let err = load_and_verify_grant(&path, "ak:did_core:webvh:z6mksupportfixture", None)
            .await
            .expect_err("missing producer proof should fail");
        assert!(err.to_string().contains("missing producer_proof"));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn rejects_issuer_proof_mismatch() {
        let path = unique_path("issuer");
        let mut ev = make_grant_event(
            "ak:did_core:webvh:z6mksupportfixture",
            None,
            "ak.message.create",
            None,
        );
        ev["producer_proof"]["verification_method"] =
            json!("did:webvh:z6mkotheradminfixture:admin.example#key-1");
        tokio::fs::write(
            &path,
            serde_json::to_vec(&ev).expect("grant event should serialize"),
        )
        .await
        .expect("write");
        let err = load_and_verify_grant(&path, "ak:did_core:webvh:z6mksupportfixture", None)
            .await
            .expect_err("issuer proof mismatch should fail");
        assert!(err.to_string().contains("verification_method"));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[test]
    fn covers_action_and_is_active() {
        let g = ArkretGrant {
            event_id: "ak:event:x".into(),
            subject: "did:webvh:s".into(),
            issuer: "did:webvh:i".into(),
            realm_id: None,
            actions: vec!["ak.message.create".into()],
            constraints: Vec::new(),
            not_before: None,
            expires_at: Some(Utc::now() + chrono::Duration::seconds(60)),
        };
        assert!(g.covers_action("ak.message.create"));
        assert!(!g.covers_action("ak.message.redact"));
        assert!(g.is_active());
    }
}
