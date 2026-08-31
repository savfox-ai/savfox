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
//! 3. Require production-shaped proofs and validate proof bindings (digest matches content).
//! 4. Sanity-check subject / realm / effective validity window against expected values.
//! 5. Return [`ArkretGrant`] holding the event_id + the grant fields.

use std::path::Path;

use anyhow::Context as _;
use arkret::{
    CapabilityGrantPayload, CapabilitySubject, Did, Event, EventProof, GrantConstraint,
    GrantConstraintKind, project_did_to_core_id,
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
/// * Event has at least one production-shaped proof.
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
    if event.proofs.is_empty() {
        anyhow::bail!("capability grant {}: missing proofs", path.display());
    }
    for proof in &event.proofs {
        match proof {
            EventProof::Producer(producer) => producer
                .validate_production()
                .map_err(|err| anyhow::anyhow!("grant proof is not production-grade: {err}"))?,
            // Admission proofs are Principal Server attestations layered on
            // top of the producer proof; their kind is closed by the type, so
            // only the signature payload can be structurally absent.
            EventProof::StationAdmission(admission) => {
                if admission.jws.is_empty() {
                    anyhow::bail!(
                        "capability grant {}: admission proof is missing its JWS",
                        path.display()
                    );
                }
            }
        }
    }
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
    if !event
        .proofs
        .iter()
        .filter_map(EventProof::as_producer)
        .any(|proof| {
            let Some((controller, _)) = proof.verification_method.as_str().split_once('#') else {
                return false;
            };
            Did::new(controller)
                .ok()
                .and_then(|did| project_did_to_core_id(&did).ok())
                .is_some_and(|core_id| core_id == *grant.issuer_id.signing_principal_id())
        })
    {
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

    use arkret::Hash;
    use serde_json::json;

    use super::*;

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
        let mut grant = serde_json::Map::new();
        let issuer = "ak:did_core:webvh:z6mkadminfixture";
        grant.insert("schema".into(), json!("ak.schema.capability.v1"));
        grant.insert("issuer".into(), json!(issuer));
        grant.insert("subject".into(), json!(subject));
        grant.insert("actions".into(), json!([action]));
        grant.insert("resources".into(), json!([{"kind": "*"}]));
        grant.insert("issued_at".into(), json!("2026-05-27T00:00:00.000Z"));
        grant.insert(
            "issuer_authority_refs".into(),
            json!([{
                "kind": "realm_root",
                "realm_id": realm.unwrap_or("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI"),
                "cell_ref": "ak:cell:ak.component.realm.authority_root.v1:null",
                "controller_epoch_at_issuance": 1,
                "authority_generation": 1
            }]),
        );
        if let Some(realm) = realm {
            grant.insert("realm_id".into(), json!(realm));
        }
        if let Some(exp) = expires {
            grant.insert(
                "constraints".into(),
                json!([{
                    "constraint_kind": "temporal",
                    "effect": "allow",
                    "expires_at": exp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                }]),
            );
        }
        let mut content = serde_json::Map::new();
        content.insert("grant".into(), serde_json::Value::Object(grant));
        let mut event = json!({
            "event_id": "ak:event:AZL87nwhLc8pnnvIhrfEQSfNkZvdPzaV3rFGVoJCQWW6",
            "kind": "ak.capability.grant",
            "realm_id": realm.unwrap_or("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI"),
            "scope_ref": {
                "kind": "realm",
                "realm_id": realm.unwrap_or("ak:realm:AY789mrKRCQEVlbVgiTgLdjVO5oCMJiUCrF-D-JlRNxI")
            },
            "actor_id": issuer,
            "principal_server_id": "ak:did_core:web:principal.example",
            "actor_seq": 1,
            "created_at": "2026-05-27T00:00:00.000Z",
            "hlc": "000000000000-0000-00000000",
            "prev_refs": [],
            "refs": [],
            "payload": serde_json::Value::Object(content),
            "proofs": []
        });
        let parsed: Event = serde_json::from_value(event.clone()).expect("event");
        let digest = Hash::new(
            parsed
                .event_digest_with_digest_suite(crate::arkret::DIGEST_SUITE)
                .expect("digest"),
        )
        .expect("hash");
        event["proofs"] = json!([{
            "kind": "detached_jws",
            "verification_method": "did:webvh:z6mkadminfixture:admin.example#key-1",
            "event_digest": digest.as_str(),
            "created_at": "2026-05-27T00:00:00.000Z",
            "jws": "test.detached.signature"
        }]);
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
    async fn rejects_missing_proofs() {
        let path = unique_path("proof");
        let mut ev = make_grant_event(
            "ak:did_core:webvh:z6mksupportfixture",
            None,
            "ak.message.create",
            None,
        );
        ev["proofs"] = json!([]);
        tokio::fs::write(
            &path,
            serde_json::to_vec(&ev).expect("grant event should serialize"),
        )
        .await
        .expect("write");
        let err = load_and_verify_grant(&path, "ak:did_core:webvh:z6mksupportfixture", None)
            .await
            .expect_err("missing proofs should fail");
        assert!(err.to_string().contains("missing proofs"));
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
        ev["proofs"][0]["verification_method"] =
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
