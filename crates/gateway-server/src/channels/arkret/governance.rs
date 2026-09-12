use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::Context;
use arkret::http_client;
const MAX_EVENT_BATCH: usize = 128;
const MAX_DEPENDENCY_BATCH: usize = 8;
type DependencySortKey = (String, Vec<u8>);

pub(super) async fn admit_owned_agent_welcomes(
    http: &arkret::http_client::Client,
    store: &savfox_channels::arkret::FileArkretCryptoStore,
    realm: &arkret::RealmId,
    account: &savfox_channels::arkret::ArkretAccountConfig,
) -> anyhow::Result<()> {
    if store
        .presence_ready_realm_ids()?
        .iter()
        .any(|id| id == realm.as_str())
        && !store
            .pending_mls_welcome_consume_bindings()?
            .iter()
            .any(|binding| {
                binding.realm_id.as_deref() == Some(realm.as_str())
                    && binding.recipient_durable_receipt.is_none()
            })
    {
        return Ok(());
    }
    let basis = http
        .seals_frontier(realm.clone())
        .await?
        .frontier
        .seal_basis;
    let resolved = resolve_mls_governance_checkpoint_with_http(http, realm, &basis)
        .await
        .map_err(anyhow::Error::msg)?;
    if !resolved
        .events
        .iter()
        .any(|event| event.kind == arkret::EventKind::MlsWelcome)
    {
        return Ok(());
    }
    let checkpoint = arkret::verify_mls_governance_closure(
        realm,
        &resolved.target_basis,
        &resolved.seals,
        &resolved.events,
        &resolved.dependencies,
        |_, _, _, _| {
            Box::pin(async {
                Err(arkret::WireError::Protocol(
                    "Agent-authored governance requires an independently verified PCR checkpoint"
                        .to_owned(),
                ))
            })
        },
    )
    .await?
    .checkpoint;
    let authorization = account
        .authorized_event_ref
        .as_deref()
        .context("Agent runtime authorization is unavailable")?;
    store.admit_verified_owned_agent_welcomes(&checkpoint, &account.principal_id, authorization)?;
    store.repair_pending_direct_conversation_bindings_from_accepted_events(
        &checkpoint.accepted_events,
    )?;
    Ok(())
}
pub(super) struct ResolvedMlsGovernanceCut {
    pub target_basis: arkret::SealBasis,
    pub seals: Vec<arkret::Seal>,
    pub events: Vec<arkret::Event>,
    pub dependencies: Vec<arkret::GovernanceDependency>,
}
pub(crate) async fn resolve_mls_governance_checkpoint_with_http(
    http: &arkret::http_client::Client,
    realm_id: &arkret::RealmId,
    target_basis: &arkret::SealBasis,
) -> Result<ResolvedMlsGovernanceCut, String> {
    target_basis
        .validate_protocol_bounds()
        .map_err(|error| format!("invalid MLS governance checkpoint target: {error}"))?;
    let mut seals = BTreeMap::new();
    let mut pending = target_basis.leaves.iter().cloned().collect::<BTreeSet<_>>();
    while !pending.is_empty() {
        let batch = pending
            .iter()
            .take(arkret::MAX_SEAL_RESOLVE_SELECTORS)
            .cloned()
            .collect::<Vec<_>>();
        for seal_ref in &batch {
            pending.remove(seal_ref);
        }
        for seal in fetch_seals_for_realm(http, realm_id, batch).await? {
            if let Some(predecessor) = &seal.predecessor_ref
                && !seals.contains_key(predecessor)
            {
                pending.insert(predecessor.clone());
            }
            if seals.insert(seal.id.clone(), seal).is_some() {
                return Err("MLS governance checkpoint Seal closure contains duplicates".to_owned());
            }
        }
    }
    let event_digests = seals
        .values()
        .flat_map(|seal| seal.delta.iter().cloned())
        .collect::<BTreeSet<_>>();
    let events =
        fetch_event_set(http, &event_digests, &BTreeMap::new(), "checkpoint delta").await?;
    let seal_values = seals.into_values().collect::<Vec<_>>();
    let selectors = arkret::governance_runtime_dependency_selector_coordinates_for_acquisition(
        &seal_values,
        &events,
    )
    .map_err(|error| format!("discover MLS governance checkpoint dependencies: {error}"))?;
    let dependencies = fetch_dependency_closure_for_realm(http, realm_id, selectors)
        .await?
        .into_values()
        .collect();
    Ok(ResolvedMlsGovernanceCut {
        target_basis: target_basis.clone(),
        seals: seal_values,
        events,
        dependencies,
    })
}

fn limit_exceeded(error: &http_client::Error) -> bool {
    matches!(
        error,
        http_client::Error::Api { error, .. }
            if error.code() == arkret::error_codes::ErrorCode::LIMIT_EXCEEDED
    )
}

async fn fetch_seals_for_realm(
    http: &arkret::Client,
    realm_id: &arkret::RealmId,
    seal_refs: Vec<arkret::SealId>,
) -> Result<Vec<arkret::Seal>, String> {
    fetch_seals_for_realm_with_access(http, realm_id, seal_refs, None).await
}

async fn fetch_seals_for_realm_with_access(
    http: &arkret::Client,
    realm_id: &arkret::RealmId,
    seal_refs: Vec<arkret::SealId>,
    history_traversal_access: Option<arkret::SelfHistoryTraversalAccess>,
) -> Result<Vec<arkret::Seal>, String> {
    let mut pending = VecDeque::new();
    for chunk in seal_refs.chunks(arkret::MAX_SEAL_RESOLVE_SELECTORS) {
        pending.push_back(chunk.to_vec());
    }
    let mut seals = BTreeMap::new();
    while let Some(batch) = pending.pop_front() {
        if batch.is_empty() {
            continue;
        }
        let resolve = arkret::SelfSealResolveRequestBody {
            realm_id: realm_id.clone(),
            seal_refs: batch.clone(),
            history_traversal_access: history_traversal_access.clone(),
        };
        match http.seals_resolve(&resolve).await {
            Ok(outcome) => {
                if !outcome.missing_seal_refs.is_empty() {
                    return Err("MLS governance Seal resolution is incomplete".to_owned());
                }
                for seal in outcome.seals {
                    if seals.insert(seal.id.clone(), seal).is_some() {
                        return Err("MLS governance Seal resolution returned duplicates".to_owned());
                    }
                }
            }
            Err(error) if limit_exceeded(&error) && batch.len() > 1 => {
                let right = batch.len() / 2;
                pending.push_front(batch[right..].to_vec());
                pending.push_front(batch[..right].to_vec());
            }
            Err(error) if limit_exceeded(&error) => {
                return Err("one canonical Seal exceeds the 8 MiB resolve ceiling".to_owned());
            }
            Err(error) => return Err(format!("resolve MLS governance Seals: {error}")),
        }
    }
    Ok(seals.into_values().collect())
}

async fn fetch_event_set(
    http: &arkret::Client,
    expected: &BTreeSet<arkret::Hash>,
    checkpoint: &BTreeMap<arkret::Hash, arkret::Event>,
    label: &str,
) -> Result<Vec<arkret::Event>, String> {
    fetch_event_set_with_access(http, expected, checkpoint, label, None).await
}

async fn fetch_event_set_with_access(
    http: &arkret::Client,
    expected: &BTreeSet<arkret::Hash>,
    checkpoint: &BTreeMap<arkret::Hash, arkret::Event>,
    label: &str,
    history_traversal_access: Option<arkret::SelfHistoryTraversalAccess>,
) -> Result<Vec<arkret::Event>, String> {
    let mut events = expected
        .iter()
        .filter_map(|digest| {
            checkpoint
                .get(digest)
                .cloned()
                .map(|event| (digest.clone(), event))
        })
        .collect::<BTreeMap<_, _>>();
    let missing = expected
        .iter()
        .filter(|digest| !events.contains_key(*digest))
        .cloned()
        .collect::<Vec<_>>();
    let mut pending = VecDeque::new();
    for chunk in missing.chunks(MAX_EVENT_BATCH) {
        pending.push_back(chunk.to_vec());
    }
    while let Some(batch) = pending.pop_front() {
        let resolve = arkret::EventsResolveRequestBody {
            event_ids: Vec::new(),
            event_digests: batch.clone(),
            include_payload: Some(true),
            history_traversal_access: history_traversal_access.clone(),
            max_response_bytes: Some(arkret::MAX_PEER_RESOLVE_RESPONSE_BYTES),
        };
        // A just-accepted membership Event and its Realm policy can reach the
        // durable Event log before the membership-gated read projection. In
        // that bounded convergence window, resolve correctly reports the
        // checkpoint delta as missing/unauthorized. Retry the same closed
        // selector set; never accept a partial response and never widen the
        // caller's history traversal authority.
        const PROJECTION_ATTEMPTS: usize = 20;
        let mut projection_attempt = 0;
        let outcome = loop {
            let outcome = http.events_resolve(&resolve).await;
            let retry = matches!(
                &outcome,
                Ok(outcome)
                    if (!outcome.missing.is_empty() || !outcome.unauthorized.is_empty())
                        && projection_attempt + 1 < PROJECTION_ATTEMPTS
            );
            if !retry {
                break outcome;
            }
            projection_attempt += 1;
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        };
        match outcome {
            Ok(outcome) => {
                if !outcome.missing.is_empty() || !outcome.unauthorized.is_empty() {
                    return Err(format!(
                        "MLS governance {label} Event resolution is incomplete: missing={}, unauthorized={}, requested={}",
                        outcome.missing.len(),
                        outcome.unauthorized.len(),
                        batch.len(),
                    ));
                }
                let requested = batch.iter().collect::<BTreeSet<_>>();
                let mut returned = BTreeSet::new();
                for event in outcome.events {
                    let digest = event_with_digest_claim(&event)?;
                    if !requested.contains(&digest)
                        || arkret::EventId::from_event_digest(&digest)
                            .map_err(|error| format!("retype Event digest: {error}"))?
                            != event.event_id
                        || !returned.insert(digest.clone())
                        || events.insert(digest, event).is_some()
                    {
                        return Err(format!(
                            "MLS governance {label} Event outcome is not every-and-only the request"
                        ));
                    }
                }
                if returned.len() != batch.len() {
                    return Err(format!(
                        "MLS governance {label} Event resolution is incomplete: returned={}, requested={}",
                        returned.len(),
                        batch.len(),
                    ));
                }
            }
            Err(error) if limit_exceeded(&error) && batch.len() > 1 => {
                let right = batch.len() / 2;
                pending.push_front(batch[right..].to_vec());
                pending.push_front(batch[..right].to_vec());
            }
            Err(error) if limit_exceeded(&error) => {
                return Err(format!(
                    "one canonical {label} Event exceeds the 8 MiB resolve ceiling"
                ));
            }
            Err(error) => return Err(format!("resolve MLS governance {label} Events: {error}")),
        }
    }
    if events.keys().collect::<BTreeSet<_>>() != expected.iter().collect::<BTreeSet<_>>() {
        return Err(format!("MLS governance {label} Event set mismatch"));
    }
    Ok(events.into_values().collect())
}

fn event_with_digest_claim(event: &arkret::Event) -> Result<arkret::Hash, String> {
    let digest = arkret::signed_event_digest_claim(event)
        .map_err(|error| format!("read signed Event digest claim: {error}"))?;
    if arkret::EventId::from_event_digest(&digest)
        .map_err(|error| format!("retype accepted Event digest: {error}"))?
        != event.event_id
    {
        return Err("accepted Event id does not bind its canonical content".to_owned());
    }
    Ok(digest)
}

async fn fetch_dependency_closure_for_realm(
    http: &arkret::Client,
    realm_id: &arkret::RealmId,
    selectors: Vec<arkret::GovernanceDependencySelector>,
) -> Result<BTreeMap<DependencySortKey, arkret::GovernanceDependency>, String> {
    fetch_dependency_closure_for_realm_with_access(http, realm_id, selectors, None).await
}

async fn fetch_dependency_closure_for_realm_with_access(
    http: &arkret::Client,
    realm_id: &arkret::RealmId,
    selectors: Vec<arkret::GovernanceDependencySelector>,
    history_traversal_access: Option<arkret::SelfHistoryTraversalAccess>,
) -> Result<BTreeMap<DependencySortKey, arkret::GovernanceDependency>, String> {
    let mut dependencies = fetch_dependency_batches_for_realm(
        http,
        realm_id,
        selectors,
        history_traversal_access.clone(),
    )
    .await?;
    loop {
        let material = dependencies.values().cloned().collect::<Vec<_>>();
        let next = arkret::governance_transitive_signer_evidence_selectors(&material)
            .map_err(|error| format!("discover governance attester evidence: {error}"))?
            .into_iter()
            .filter(|selector| {
                selector_key(selector)
                    .map(|key| !dependencies.contains_key(&key))
                    .unwrap_or(true)
            })
            .collect::<Vec<_>>();
        if next.is_empty() {
            return Ok(dependencies);
        }
        dependencies.extend(
            fetch_dependency_batches_for_realm(
                http,
                realm_id,
                next,
                history_traversal_access.clone(),
            )
            .await?,
        );
    }
}

async fn fetch_dependency_batches_for_realm(
    http: &arkret::Client,
    realm_id: &arkret::RealmId,
    selectors: Vec<arkret::GovernanceDependencySelector>,
    history_traversal_access: Option<arkret::SelfHistoryTraversalAccess>,
) -> Result<BTreeMap<DependencySortKey, arkret::GovernanceDependency>, String> {
    let selectors = canonical_dependency_selectors(selectors)?;
    let expected_count = selectors.len();
    let items = fetch_available_dependency_batches_for_realm(
        http,
        realm_id,
        selectors,
        history_traversal_access,
    )
    .await?;
    if items.len() != expected_count {
        return Err("governance dependency resolution is incomplete".to_owned());
    }
    Ok(items)
}

async fn fetch_available_dependency_batches_for_realm(
    http: &arkret::Client,
    realm_id: &arkret::RealmId,
    selectors: Vec<arkret::GovernanceDependencySelector>,
    history_traversal_access: Option<arkret::SelfHistoryTraversalAccess>,
) -> Result<BTreeMap<DependencySortKey, arkret::GovernanceDependency>, String> {
    let selectors = canonical_dependency_selectors(selectors)?;
    let mut pending = VecDeque::new();
    for chunk in selectors.chunks(MAX_DEPENDENCY_BATCH) {
        pending.push_back(chunk.to_vec());
    }
    let mut items = BTreeMap::new();
    while let Some(batch) = pending.pop_front() {
        if batch.is_empty() {
            continue;
        }
        let resolve = arkret::SelfGovernanceDependencyResolveRequestBody {
            realm_id: realm_id.clone(),
            selectors: batch.clone(),
            byte_limit: arkret::MAX_GOVERNANCE_DEPENDENCY_RESPONSE_BYTES,
            history_traversal_access: history_traversal_access.clone(),
        };
        match http.governance_dependencies_resolve(&resolve).await {
            Ok(outcome) => {
                for item in outcome.items {
                    let key = selector_key(item.selector())?;
                    if items.insert(key, item).is_some() {
                        return Err("governance dependency resolver returned duplicates".to_owned());
                    }
                }
            }
            Err(error) if limit_exceeded(&error) && batch.len() > 1 => {
                let right = batch.len() / 2;
                pending.push_front(batch[right..].to_vec());
                pending.push_front(batch[..right].to_vec());
            }
            Err(error) if limit_exceeded(&error) => {
                return Err(
                    "one governance dependency exceeds the 8 MiB resolve ceiling".to_owned(),
                );
            }
            Err(error) => return Err(format!("resolve governance dependencies: {error}")),
        }
    }
    Ok(items)
}

fn canonical_dependency_selectors(
    selectors: Vec<arkret::GovernanceDependencySelector>,
) -> Result<Vec<arkret::GovernanceDependencySelector>, String> {
    let mut ordered = BTreeMap::new();
    for selector in selectors {
        ordered.insert(selector_key(&selector)?, selector);
    }
    Ok(ordered.into_values().collect())
}

fn selector_key(
    selector: &arkret::GovernanceDependencySelector,
) -> Result<DependencySortKey, String> {
    selector
        .canonical_sort_key()
        .map(|(kind, bytes)| (kind.to_owned(), bytes))
        .map_err(|error| format!("canonicalize governance dependency selector: {error}"))
}
