use anyhow::Context;
const MLS_WELCOME_COMMIT_SCAN_PAGE: u16 = 200;
const MLS_HISTORY_MAX_PAGES: usize = 64;
const MLS_HISTORY_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Resolve the exact accepted Commit a recipient delivery names on its own
/// independent scope stream. A matching Event that is withheld is not enough
/// to open a Welcome: the MLS layer needs the full accepted Event and commit.
pub(crate) async fn accepted_commit_for_welcome(
    http: &savfox_channels::arkret::ArkretHttpClient,
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
    http: &savfox_channels::arkret::ArkretHttpClient,
    realm_id: &arkret::RealmId,
    scope: &arkret::ScopeRef,
    event_ref: &arkret::EventId,
) -> anyhow::Result<arkret::CommittedEventFullView> {
    anyhow::ensure!(
        scope.realm_id_opt() == Some(realm_id),
        "MLS Commit scope Realm mismatch"
    );
    let stream_ref = arkret::CommitStreamRef::from_scope(scope, None)?;
    let own = http.own_station()?;
    let mut after = None;
    let mut bytes = 0usize;
    for _ in 0..MLS_HISTORY_MAX_PAGES {
        let request = arkret::StreamScanRequest {
            realm_id: realm_id.clone(),
            stream_ref: stream_ref.clone(),
            direction: arkret::StreamScanDirection::After(after),
            limit: MLS_WELCOME_COMMIT_SCAN_PAGE,
        };
        let response = own.scan_commit_stream(&request).await?;
        let page = response.value()?;
        bytes = bytes.saturating_add(serde_json::to_vec(page)?.len());
        anyhow::ensure!(
            bytes <= MLS_HISTORY_MAX_BYTES,
            "Welcome original lookup exceeded byte budget"
        );
        if let Some(row) = page
            .committed_events
            .iter()
            .find(|row| row.commit().event_ref == *event_ref)
        {
            let arkret::CommittedEventView::Full(full) = row else {
                anyhow::bail!("Welcome original is withheld")
            };
            anyhow::ensure!(
                full.event.realm_id == *realm_id && full.event.scope_ref == *scope,
                "Welcome original differs from exact delivery scope"
            );
            return exact_scanned_original(own, &full.commit, Some(full)).await;
        }
        let next = page
            .committed_events
            .last()
            .map(|row| row.commit().stream_position);
        anyhow::ensure!(
            page.truncated && next.is_some() && next != after,
            "Welcome original is absent from its readable stream"
        );
        after = next;
    }
    anyhow::bail!("Welcome original lookup exceeded bounded recovery window")
}

/// Recover the accepted prefix after a durable MLS base. The target is from
/// Garth's verified account delivery; HTTP lookup supplies only candidate bytes.
/// Each original Event and Commit crosses the own-Station session, exact
/// content-address, historical producer and continuous-stream checks.
pub(super) async fn verified_mls_prefix(
    http: &savfox_channels::arkret::ArkretHttpClient,
    target: &arkret::CommittedEventFullView,
    installed_base: &arkret::EventId,
    base_candidate: Option<arkret::CommittedEventFullView>,
    continuation: Option<arkret::RealmCommit>,
) -> anyhow::Result<(
    arkret::CommittedEventFullView,
    Vec<arkret::CommittedEventFullView>,
    Option<arkret::RealmCommit>,
)> {
    target.validate_shape()?;
    let realm = &target.event.realm_id;
    let scope = &target.event.scope_ref;
    let stream = arkret::CommitStreamRef::from_scope(scope, None)?;
    let own = http.own_station()?;
    let base = if let Some(saved) = base_candidate.as_ref() {
        exact_scanned_original(own, &saved.commit, Some(saved)).await?
    } else {
        accepted_commit_for_event(http, realm, scope, installed_base).await?
    };
    anyhow::ensure!(
        base.event.event_id == *installed_base
            && base.event.scope_ref == *scope
            && base.commit.stream_ref == stream
            && target.commit.stream_ref == stream,
        "historical MLS anchor scope/stream mismatch"
    );
    let mut replica = garth::own_station_results::OwnStationReplica::new(realm.clone());
    replica
        .restore_checkpoint_head(own, &stream_head(&base.commit))
        .await?;
    if let Some(head) = continuation.as_ref() {
        anyhow::ensure!(
            head.stream_ref == stream
                && head.stream_position >= base.commit.stream_position
                && head.stream_position < target.commit.stream_position,
            "MLS replay continuation differs from accepted base/target"
        );
        exact_scanned_original(own, head, None).await?;
        replica
            .restore_checkpoint_head(own, &stream_head(head))
            .await?;
    }
    anyhow::ensure!(
        base.commit.stream_position < target.commit.stream_position,
        "historical MLS target does not follow installed base"
    );
    let mut position = continuation
        .as_ref()
        .map_or(base.commit.stream_position, |head| head.stream_position);
    let mut last_head = continuation;
    let mut commits = Vec::new();
    let mut prefix_bytes = 0usize;
    for _ in 0..MLS_HISTORY_MAX_PAGES {
        let scan_request = arkret::StreamScanRequest {
            realm_id: realm.clone(),
            stream_ref: stream.clone(),
            direction: arkret::StreamScanDirection::After(Some(position)),
            limit: MLS_WELCOME_COMMIT_SCAN_PAGE,
        };
        let response = own.scan_commit_stream(&scan_request).await?;
        let truncated = response.value()?.truncated;
        let verified = admit_mls_prefix_page(own, &mut replica, response).await?;
        anyhow::ensure!(
            !verified.rows()?.is_empty(),
            "historical MLS prefix did not advance"
        );
        for item in verified.rows()? {
            let arkret::CommittedEventView::Full(full) = item else {
                anyhow::bail!("historical MLS prefix contains an undisclosed Event")
            };
            let bytes = serde_json::to_vec(full)?.len();
            if prefix_bytes.saturating_add(bytes) > MLS_HISTORY_MAX_BYTES {
                anyhow::ensure!(
                    last_head.is_some(),
                    "one MLS prefix Event exceeds byte budget"
                );
                return Ok((base, commits, last_head));
            }
            prefix_bytes += bytes;
            position = full.commit.stream_position;
            last_head = Some(full.commit.clone());
            if full.event.kind == arkret::EventKind::MlsCommit {
                commits.push(full.clone());
            }
            if full.commit.event_ref == target.commit.event_ref {
                anyhow::ensure!(
                    full == target,
                    "historical MLS target differs from verified account delivery"
                );
                return Ok((base, commits, None));
            }
            anyhow::ensure!(
                position < target.commit.stream_position,
                "historical MLS prefix crossed target without its exact Event"
            );
        }
        anyhow::ensure!(
            truncated,
            "historical MLS target is absent from verified prefix"
        );
    }
    // Persist any verified MLS progress before the original durable target is
    // retried. The next cycle resumes from that newly installed accepted base.
    Ok((base, commits, last_head))
}

pub(super) async fn admit_mls_prefix_page(
    own: &arkret::http_client::own_station_results::OwnStationResultClient,
    replica: &mut garth::own_station_results::OwnStationReplica,
    response: arkret::http_client::own_station_results::BoundOwnStationResponse<
        arkret::StreamScanRequest,
        arkret::StreamScanOutcome,
    >,
) -> anyhow::Result<garth::own_station_results::OwnStationScanPage> {
    // An installed MLS base is not a history-floor basis. Freeze a covering
    // authenticated current cut after the candidate page, before admitting
    // any original or advancing MLS state. Garth binds its floor and heads.
    let current = own.snapshot_head(&response.request().realm_id).await?;
    replica.install_bound_snapshot(&current)?;
    Ok(replica.apply_bound_scan(own, response).await?)
}

/// Consume the recipient's claim through its authenticated accepted Station.
/// Recipient MLS signatures and exact claim bindings remain independently checked.
pub(crate) async fn verified_own_welcome_claim(
    http: &savfox_channels::arkret::ArkretHttpClient,
    delivery: &arkret::MlsWelcomeDelivery,
    local_station: &arkret::DidCoreId,
) -> anyhow::Result<arkret::KeyPackagesClaimOutcome> {
    let own = http.own_station()?;
    anyhow::ensure!(
        &own.session()?.account_id().station_id == local_station,
        "MLS Welcome recipient Station differs from bound session"
    );
    let response = own
        .keypackages_claim_query(&arkret::KeyPackagesClaimQueryRequestBody {
            claim_id: delivery.keypackage_claim_ref.clone(),
        })
        .await?;
    let outcome = response.into_value()?;
    outcome
        .validate_shape()
        .map_err(|error| anyhow::anyhow!("MLS Welcome claim outcome is invalid: {error}"))?;
    anyhow::ensure!(
        &outcome.claim_receipt.destination_id == local_station,
        "MLS Welcome claim receipt is not from this endpoint's Station"
    );
    Ok(outcome)
}

/// Consume one Station-queued Agent Welcome. The governance Station verified
/// the delivery's producer proof when it atomically accepted the Commit and
/// enqueued the delivery; the recipient independently checks the exact claim and MLS recipient
/// material before opening its local KeyPackage private state.
pub(crate) async fn admit_owned_agent_welcome_delivery(
    http: &savfox_channels::arkret::ArkretHttpClient,
    store: &savfox_channels::arkret::FileArkretCryptoStore,
    delivery: &arkret::MlsWelcomeDelivery,
    account: &savfox_channels::arkret::ArkretAccountConfig,
) -> anyhow::Result<(bool, arkret::CommittedEventFullView)> {
    anyhow::ensure!(
        http.own_station()?.session()?.account_id() == &account.actor_account_id,
        "MLS Welcome host Account differs from current bound session"
    );
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
    http.own_station()?.check_session()?;
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
    http.own_station()?.check_session()?;
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
    http: &savfox_channels::arkret::ArkretHttpClient,
    realm_id: &arkret::RealmId,
) -> anyhow::Result<(arkret::RealmStateSnapshot, arkret::DidCoreId)> {
    let response = http.own_station()?.snapshot_head(realm_id).await?;
    let mut replica = garth::own_station_results::OwnStationReplica::new(realm_id.clone());
    replica.install_bound_snapshot(&response)?;
    let snapshot = response.into_value()?;
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
    http: &savfox_channels::arkret::ArkretHttpClient,
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

    anyhow::ensure!(
        accepted.event.scope_ref.realm_id_opt() == Some(&accepted.event.realm_id),
        "MLS roster target differs from accepted Realm"
    );
    accepted_commit_roster(http, accepted, account).await
}

/// Obtain the exact accepted transition's complete Add provenance through the
/// authenticated own-Station reader, including the original recipient signatures.
pub(super) async fn accepted_commit_roster(
    http: &savfox_channels::arkret::ArkretHttpClient,
    accepted: &arkret::CommittedEventFullView,
    account: &savfox_channels::arkret::ArkretAccountConfig,
) -> anyhow::Result<savfox_channels::arkret::ArkretMlsRosterMaterial> {
    accepted.validate_shape()?;
    anyhow::ensure!(
        accepted.event.kind == arkret::EventKind::MlsCommit,
        "MLS roster target is not an accepted Commit"
    );
    let payload: arkret::MlsCommitPayload =
        serde_json::from_value(serde_json::to_value(&accepted.event.payload)?)?;
    payload.validate()?;
    anyhow::ensure!(
        payload.governance_binding().effective_scope() == &accepted.event.scope_ref,
        "MLS roster target payload differs from its accepted scope"
    );
    let mut request = arkret::MlsMemberRosterAuthorityReadRequestBody {
        realm_id: accepted.event.realm_id.clone(),
        effective_scope: accepted.event.scope_ref.clone(),
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
        let page = http
            .own_station()?
            .self_mls_roster_authority(&request)
            .await?
            .into_value()?;
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
        .own_station()?
        .self_mls_group_state_material(&material_request)
        .await?
        .into_value()?;
    Ok(savfox_channels::arkret::ArkretMlsRosterMaterial {
        request: first_request,
        pages,
        genesis_material: material,
    })
}

fn commit_reference(commit: &arkret::RealmCommit) -> arkret::CommittedEventRef {
    arkret::CommittedEventRef {
        event_id: commit.event_ref.clone(),
        commit_id: commit.commit_id.clone(),
        stream_ref: commit.stream_ref.clone(),
        stream_position: commit.stream_position,
    }
}

fn stream_head(commit: &arkret::RealmCommit) -> arkret::CommitStreamHead {
    arkret::CommitStreamHead {
        stream_ref: commit.stream_ref.clone(),
        stream_position: commit.stream_position,
        commit_id: commit.commit_id.clone(),
    }
}

async fn exact_scanned_original(
    client: &arkret::http_client::own_station_results::OwnStationResultClient,
    commit: &arkret::RealmCommit,
    expected: Option<&arkret::CommittedEventFullView>,
) -> anyhow::Result<arkret::CommittedEventFullView> {
    let request = arkret::StreamScanRequest {
        realm_id: commit.realm_id.clone(),
        stream_ref: commit.stream_ref.clone(),
        direction: arkret::StreamScanDirection::Before(Some(
            commit
                .stream_position
                .checked_add(1)
                .context("MLS original position overflow")?,
        )),
        limit: 1,
    };
    let response = client.scan_commit_stream(&request).await?;
    let admitted = garth::own_station_results::consume_bound_scan_row(
        client,
        &commit_reference(commit),
        response,
    )
    .await?;
    let page = admitted.into_value()?;
    let [arkret::CommittedEventView::Full(full)] = page.committed_events.as_slice() else {
        anyhow::bail!("MLS exact stream row is withheld or absent");
    };
    anyhow::ensure!(
        &full.commit == commit && expected.is_none_or(|saved| saved == full),
        "MLS historical original differs from its exact retained reference"
    );
    Ok(full.clone())
}
