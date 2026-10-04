use anyhow::Context;
const MLS_WELCOME_COMMIT_SCAN_PAGE: u16 = 200;

/// Resolve the exact accepted Commit a recipient delivery names on its own
/// independent scope stream. A matching Event that is withheld is not enough
/// to open a Welcome: the MLS layer needs the full accepted Event and commit.
pub(crate) async fn accepted_commit_for_welcome(
    http: &arkret::http_client::Client,
    delivery: &arkret::MlsWelcomeDelivery,
) -> anyhow::Result<arkret::CommittedEventFullView> {
    delivery.validate_shape()?;
    accepted_commit_for_event(
        http,
        &delivery.realm_id,
        &delivery.effective_scope,
        &delivery.commit_event_ref,
    )
    .await
}

pub(crate) async fn accepted_commit_for_event(
    http: &arkret::http_client::Client,
    realm_id: &arkret::RealmId,
    scope: &arkret::ScopeRef,
    event_ref: &arkret::EventId,
) -> anyhow::Result<arkret::CommittedEventFullView> {
    anyhow::ensure!(
        scope.realm_id_opt() == Some(realm_id),
        "MLS Commit scope Realm mismatch"
    );
    let stream_ref = arkret::CommitStreamRef::from_scope(scope, None)?;
    let mut after_position = None;
    loop {
        let page = http
            .scan_commit_stream_tail(
                realm_id.clone(),
                stream_ref.clone(),
                after_position,
                MLS_WELCOME_COMMIT_SCAN_PAGE,
            )
            .await?;
        for item in &page.committed_events {
            if item.commit().event_ref == *event_ref {
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
) -> anyhow::Result<(bool, arkret::CommittedEventFullView)> {
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
    )
    .map_err(anyhow::Error::msg)?;
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
    store.reserve_welcome_key_package(
        delivery,
        &accepted,
        &arkret::MlsEndpointIdentity::AgentRuntime {
            agent_id: account.actor_account_id.principal_id.clone(),
            verification_method: verification_method.clone(),
            agent_key_authorize_event_id: authorization_ref.clone(),
        },
    )?;
    let claim =
        verified_own_welcome_claim(http, delivery, &account.actor_account_id.station_id).await?;
    let roster = welcome_roster(http, store, delivery, &accepted, account).await?;
    let admitted = store.install_accepted_mls_welcome(
        delivery,
        &accepted,
        &claim,
        &account.actor_account_id.station_id,
        &authorization_ref,
        arkret::RecipientMlsDurableSigner::Agent {
            recipient_agent_id: account.actor_account_id.principal_id.clone(),
            recipient_agent_verification_method: verification_method,
            agent_key_authorize_event_id: authorization_ref.clone(),
        },
        &roster,
    )?;
    Ok((admitted, accepted))
}

/// Consume scope current from the authenticated own Account Station, retaining
/// exact Realm and generation bindings without native DID replay or public
/// authority discovery on a member Station.
pub(super) async fn verified_scope_snapshot(
    http: &arkret::http_client::Client,
    realm_id: &arkret::RealmId,
) -> anyhow::Result<(arkret::RealmStateSnapshot, arkret::DidCoreId)> {
    let snapshot = http.realm_state_snapshot_head(realm_id).await?;
    anyhow::ensure!(
        snapshot.realm_id == *realm_id,
        "own-Station snapshot differs from requested Realm"
    );
    let (controller, _) = snapshot
        .signature
        .verification_method
        .as_str()
        .split_once('#')
        .context("own-Station snapshot signer has no method fragment")?;
    let did = arkret::Did::new(controller).map_err(anyhow::Error::msg)?;
    let governance = arkret::project_did_to_core_id(&did)?;
    Ok((snapshot, governance))
}

async fn welcome_roster(
    http: &arkret::http_client::Client,
    store: &savfox_channels::arkret::FileArkretCryptoStore,
    delivery: &arkret::MlsWelcomeDelivery,
    accepted: &arkret::CommittedEventFullView,
    account: &savfox_channels::arkret::ArkretAccountConfig,
) -> anyhow::Result<savfox_channels::arkret::ArkretMlsRosterMaterial> {
    if matches!(delivery.effective_scope, arkret::ScopeRef::Realm { .. }) {
        store.upsert_realm_policy(savfox_channels::arkret::ArkretRealmCryptoPolicy {
            realm_id: delivery.realm_id.to_string(),
            content_encryption_floor:
                savfox_channels::arkret::ArkretContentEncryptionFloor::E2eeRequired,
            encryption_profile: Some("mls_rfc9420".to_owned()),
            mls_group_id: Some(
                delivery
                    .effective_scope
                    .canonical_mls_group_id()?
                    .to_string(),
            ),
            source: "accepted_mls_welcome".to_owned(),
            updated_at: chrono::Utc::now(),
        })?;
    }
    let payload: arkret::MlsCommitPayload =
        serde_json::from_value(serde_json::to_value(&accepted.event.payload)?)?;
    anyhow::ensure!(
        accepted.event.realm_id == delivery.realm_id
            && accepted.event.scope_ref == delivery.effective_scope,
        "Welcome differs from accepted target"
    );
    let mut request = arkret::MlsMemberRosterAuthorityReadRequestBody {
        realm_id: delivery.realm_id.clone(),
        effective_scope: delivery.effective_scope.clone(),
        mls_group_id: payload.mls_group_id()?,
        target_commit_event_ref: accepted.event.event_id.clone(),
        target_epoch: payload.next_epoch(),
        caller_actor_id: arkret::ActorId::account(account.actor_account_id.clone()),
        cursor: None,
    };
    let first_request = request.clone();
    let mut pages = Vec::new();
    let mut seen_cursors = std::collections::BTreeSet::new();
    loop {
        let page = http.self_mls_roster_authority(&request).await?;
        anyhow::ensure!(
            page.roster.page_index == pages.len() as u64
                && page.roster.page_index < page.roster.manifest.page_count,
            "MLS roster page is reordered or exceeds its signed count"
        );
        request.cursor = page.roster.next_cursor.clone();
        if let Some(cursor) = &request.cursor {
            anyhow::ensure!(
                seen_cursors.insert(cursor.clone()),
                "MLS roster cursor repeats"
            );
        }
        pages.push(page);
        if request.cursor.is_none() {
            break;
        }
    }
    let peer = arkret::verify_mls_member_roster_authority_pages(&pages, &first_request)?;
    let material_request =
        arkret::mls_roster_genesis_material_request(&peer, &pages[0].roster.manifest);
    let material = http
        .self_mls_group_state_material(&material_request)
        .await?;
    Ok(savfox_channels::arkret::ArkretMlsRosterMaterial {
        request: first_request,
        pages,
        genesis_material: material,
    })
}
