use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::Context;
use arkret::http_client;
use savfox_channels::arkret::MlsWelcomeAdmissionSubject;
const MAX_EVENT_BATCH: usize = 128;
const MAX_DEPENDENCY_BATCH: usize = 8;
const MLS_WELCOME_COMMIT_SCAN_PAGE: u16 = 200;
type DependencySortKey = (String, Vec<u8>);

/// Resolve the exact accepted Commit a recipient delivery names on its own
/// independent scope stream. A matching Event that is withheld is not enough
/// to open a Welcome: the MLS layer needs the full accepted Event and commit.
pub(crate) async fn accepted_commit_for_welcome(
    http: &arkret::http_client::Client,
    delivery: &arkret::MlsWelcomeDelivery,
) -> anyhow::Result<arkret::CommittedEventFullView> {
    delivery.validate_shape()?;
    let stream_ref = arkret::CommitStreamRef::from_scope(&delivery.effective_scope, None)?;
    let mut after_position = None;
    loop {
        let page = http
            .scan_commit_stream_tail(
                delivery.realm_id.clone(),
                stream_ref.clone(),
                after_position,
                MLS_WELCOME_COMMIT_SCAN_PAGE,
            )
            .await?;
        for item in &page.committed_events {
            if item.commit().event_ref == delivery.commit_event_ref {
                let arkret::CommittedEventView::Full(full) = item else {
                    anyhow::bail!("MLS Welcome Commit Event is withheld on its scope stream")
                };
                full.validate_shape()?;
                return Ok(full.clone());
            }
        }
        let last_position = page
            .committed_events
            .last()
            .map(|item| item.commit().stream_position);
        match (page.truncated, last_position) {
            (true, Some(position)) => after_position = Some(position),
            (true, None) => anyhow::bail!("MLS Welcome Commit stream scan did not advance"),
            (false, _) => anyhow::bail!("MLS Welcome Commit is not accepted on its scope stream"),
        }
    }
}

/// Read the recipient's own Station claim and verify its historical Station
/// signature before any caller passes its KeyPackage to OpenMLS.
pub(crate) async fn verified_own_welcome_claim(
    http: &arkret::http_client::Client,
    delivery: &arkret::MlsWelcomeDelivery,
    local_station: &arkret::DidCoreId,
) -> anyhow::Result<arkret::KeyPackagesClaimOutcome> {
    let outcome = http
        .keypackages_claim_query(&arkret::KeyPackagesClaimQueryRequestBody {
            claim_id: delivery.keypackage_claim_ref.clone(),
        })
        .await?;
    outcome
        .validate_shape()
        .map_err(|error| anyhow::anyhow!("MLS Welcome claim outcome is invalid: {error}"))?;
    anyhow::ensure!(
        &outcome.claim_receipt.destination_id == local_station,
        "MLS Welcome claim receipt is not from this endpoint's Station"
    );
    let resolution = http.open_service_resolution(local_station).await?;
    arkret::verify_peer_keypackage_claim_receipt_signature(&outcome.claim_receipt, &resolution)?;
    Ok(outcome)
}

/// Consume one Station-queued Agent Welcome. The governance Station verified
/// the delivery's producer proof when it atomically accepted the Commit and
/// enqueued the delivery; the recipient independently verifies the historical
/// own-Station claim before opening its local KeyPackage private state.
pub(crate) async fn admit_owned_agent_welcome_delivery(
    http: &arkret::http_client::Client,
    store: &savfox_channels::arkret::FileArkretCryptoStore,
    delivery: &arkret::MlsWelcomeDelivery,
    account: &savfox_channels::arkret::ArkretAccountConfig,
) -> anyhow::Result<bool> {
    anyhow::ensure!(
        delivery.recipient_actor_id == arkret::ActorId::account(account.actor_account_id.clone()),
        "MLS Welcome is addressed to another Agent account"
    );
    let verification_method = arkret::DidUrl::new(
        account
            .verification_method
            .as_ref()
            .context("Agent MLS verification method is unavailable")?
            .clone(),
    )?;
    anyhow::ensure!(
        delivery.recipient_endpoint
            == arkret::MlsWelcomeRecipientEndpoint::AgentRuntime {
                verification_method: verification_method.clone(),
            },
        "MLS Welcome is addressed to another Agent runtime key"
    );
    let authorization_ref = arkret::EventId::new(
        account
            .authorized_event_ref
            .as_ref()
            .context("Agent MLS key authorization is unavailable")?
            .clone(),
    )?;
    let accepted = accepted_commit_for_welcome(http, delivery).await?;
    let claim =
        verified_own_welcome_claim(http, delivery, &account.actor_account_id.station_id).await?;
    store.install_accepted_mls_welcome(
        delivery,
        &accepted,
        &claim,
        &account.actor_account_id.station_id,
        &authorization_ref,
        arkret::RecipientMlsDurableSigner::Agent {
            recipient_agent_id: account.actor_account_id.principal_id.clone(),
            recipient_agent_verification_method: verification_method,
            agent_key_authorize_event_id: authorization_ref,
        },
        &[],
    )
}

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
    let subject = MlsWelcomeAdmissionSubject::OwnedAgent {
        agent_id: arkret::DidCoreId::new(account.principal_id.clone())?,
        agent_key_authorize_event_id: account
            .authorized_event_ref
            .clone()
            .context("Agent runtime authorization is unavailable")?,
    };
    admit_verified_welcomes(http, store, realm, &subject).await?;
    Ok(())
}

/// The single verified-governance Welcome admission path. Every edge reaches
/// group state through this function, so the governance closure verification
/// that decides what "verified" means has exactly one implementation and the
/// Agent and Applet Bot edges cannot drift apart.
///
/// Zero side effects unless the closure is complete: the Seal/Event/dependency
/// closure must resolve in full, `verify_mls_governance_closure` must accept it,
/// and only then is the checkpoint handed to the crypto store.
pub(crate) async fn admit_verified_welcomes(
    http: &arkret::http_client::Client,
    store: &savfox_channels::arkret::FileArkretCryptoStore,
    realm: &arkret::RealmId,
    subject: &MlsWelcomeAdmissionSubject,
) -> anyhow::Result<usize> {
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
        return Ok(0);
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
    let accepted_leaf_authority = if subject.needs_accepted_leaf_authority() {
        fetch_accepted_leaf_authority(http, &checkpoint, subject).await?
    } else {
        BTreeMap::new()
    };
    let admitted = store.admit_verified_welcomes(&checkpoint, subject, &accepted_leaf_authority)?;
    store.repair_pending_direct_conversation_bindings_from_accepted_events(
        &checkpoint.accepted_events,
    )?;
    Ok(admitted)
}

/// Read the Station's accepted historical leaf authority for each Welcome this
/// subject may consume. An ordinary multi-member group cannot be persisted
/// without complete member attribution, and the SDK re-checks every returned
/// authorization against the joined group's own governance binding and RFC 9420
/// leaves, so this never widens what the verified closure already decided.
async fn fetch_accepted_leaf_authority(
    http: &arkret::http_client::Client,
    checkpoint: &arkret::MlsGovernanceVerificationCheckpoint,
    subject: &MlsWelcomeAdmissionSubject,
) -> anyhow::Result<BTreeMap<arkret::EventId, arkret::MlsAcceptedArtifactOutcome>> {
    let mut authority = BTreeMap::new();
    for event in &checkpoint.accepted_events {
        if event.kind != arkret::EventKind::MlsWelcome {
            continue;
        }
        let payload: arkret::MlsWelcomePayload =
            serde_json::from_value(serde_json::to_value(&event.payload)?)?;
        if !subject.matches_welcome_recipient(&payload) {
            continue;
        }
        let request = arkret::MlsAcceptedArtifactRequestBody {
            effective_scope: payload.governance_binding.effective_scope().clone(),
            mls_group_id: arkret::Base64UrlString::new(payload.mls_group_id())
                .map_err(anyhow::Error::msg)?,
            artifact_ref: event.event_id.clone(),
        };
        let outcome = http.mls_accepted_artifact(&request).await?;
        authority.insert(event.event_id.clone(), outcome);
    }
    Ok(authority)
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
            selection: arkret::SealResolveSelection::SealRefs {
                seal_refs: batch.clone(),
            },
            history_traversal_access: history_traversal_access.clone(),
        };
        match http.seals_resolve(&resolve).await {
            Ok(arkret::SealResolveOutcome::Seals {
                seals: resolved,
                missing_seal_refs,
            }) => {
                if !missing_seal_refs.is_empty() {
                    return Err("MLS governance Seal resolution is incomplete".to_owned());
                }
                for seal in resolved {
                    if seals.insert(seal.id.clone(), seal).is_some() {
                        return Err("MLS governance Seal resolution returned duplicates".to_owned());
                    }
                }
            }
            Ok(arkret::SealResolveOutcome::Conclusions { .. }) => {
                return Err(
                    "MLS governance Seal resolution returned conclusions for a seal_refs selection"
                        .to_owned(),
                );
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
