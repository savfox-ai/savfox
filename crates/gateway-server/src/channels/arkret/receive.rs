//! Agent endpoint delivery and independently authorized Commit stream tails.
//!
//! Agents never subscribe to the human-device account aggregate. Queue pulls
//! discover accepted Welcomes; bounded position scans follow only the scopes
//! installed from those deliveries (`client-sync.md` sections 5 and 10).

use super::*;

#[derive(serde::Serialize, serde::Deserialize)]
struct StreamCheckpoint {
    head: arkret::CommitStreamHead,
    baseline_through: u64,
}

/// The first trigger boundary is the accepted Welcome Add, never a later
/// snapshot head: the user's first message can already follow that Add before
/// the runtime has finished joining. Persist before ACK and never rewind.
pub(super) async fn seed_welcome_checkpoint(
    http: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    welcome: &arkret::MlsWelcomeDelivery,
    accepted: &arkret::CommittedEventFullView,
) -> anyhow::Result<()> {
    let stream_ref = arkret::CommitStreamRef::from_scope(&welcome.effective_scope, None)?;
    let scope = garth::CursorScope::CommitStream {
        service_id: account_subscription_service_id(channel, account)?,
        stream_ref: stream_ref.clone(),
    };
    if account_store.load(scope.clone()).await?.is_some() {
        return Ok(());
    }
    let request = arkret::StreamScanRequest {
        realm_id: welcome.realm_id.clone(),
        stream_ref: stream_ref.clone(),
        direction: arkret::StreamScanDirection::After(
            accepted.commit.stream_position.checked_sub(1),
        ),
        limit: 1,
    };
    let response = http.own_station()?.scan_commit_stream(&request).await?;
    let page = response.value()?;
    let [arkret::CommittedEventView::Full(exact)] = page.committed_events.as_slice() else {
        anyhow::bail!("Welcome checkpoint requires its exact visible accepted Commit");
    };
    anyhow::ensure!(
        exact == accepted,
        "Welcome checkpoint read changed the accepted Add"
    );
    exact.validate_shape()?;
    let reference = arkret::CommittedEventRef {
        event_id: exact.event.event_id.clone(),
        commit_id: exact.commit.commit_id.clone(),
        stream_ref: exact.commit.stream_ref.clone(),
        stream_position: exact.commit.stream_position,
    };
    let exact = exact.clone();
    let admitted = garth::own_station_results::consume_bound_scan_row(
        http.own_station()?,
        &reference,
        http.own_station()?
            .scan_commit_stream(&arkret::StreamScanRequest {
                direction: arkret::StreamScanDirection::Before(Some(
                    exact
                        .commit
                        .stream_position
                        .checked_add(1)
                        .context("Welcome checkpoint position overflow")?,
                )),
                ..request
            })
            .await?,
    )
    .await?;
    anyhow::ensure!(
        admitted.value()?.committed_events == vec![arkret::CommittedEventView::Full(exact.clone())],
        "Welcome checkpoint verified row changed its exact original"
    );
    exact.event.verify_producer_proof_self_consistency(
        exact.event.realm_id.digest_suite_code().digest_suite(),
    )?;
    let checkpoint = StreamCheckpoint {
        head: arkret::CommitStreamHead {
            stream_ref,
            stream_position: exact.commit.stream_position,
            commit_id: exact.commit.commit_id.clone(),
        },
        baseline_through: exact.commit.stream_position,
    };
    http.own_station()?.check_session()?;
    account_store
        .save(scope, serde_json::to_string(&checkpoint)?)
        .await?;
    Ok(())
}

pub(super) fn delivery_scope(
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) -> anyhow::Result<garth::CursorScope> {
    Ok(garth::CursorScope::AgentDeliveries {
        service_id: account_subscription_service_id(channel, account)?,
        actor_id: arkret::ActorId::account(account.actor_account_id.clone()),
        verification_method: arkret::DidUrl::new(
            account
                .verification_method
                .clone()
                .context("Agent delivery key is unavailable")?,
        )
        .map_err(anyhow::Error::msg)?,
        authorization_event_ref: EventId::new(
            account
                .authorized_event_ref
                .clone()
                .context("Agent delivery authorization is unavailable")?,
        )?,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn drive(
    provider: &ArkretAgentSessionProvider,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: garth::FileStore,
    crypto_store: FileArkretCryptoStore,
    gateway_channel: Arc<GatewayChannel>,
    session_store: Arc<SessionStore>,
) -> AccountEngineOutcome {
    let mut receive_tick = tokio::time::interval(Duration::from_secs(1));
    receive_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut presence_tick = tokio::time::interval(ACCOUNT_PRESENCE_REFRESH);
    presence_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_auth_warning = None;
    let mut recovery_due = tokio::time::Instant::now();
    let mut recovery_progress = std::collections::BTreeMap::new();
    let mut recovery_retries = ScopeMaintenanceRetries::default();
    let mut scan_retries = ScopeMaintenanceRetries::default();
    let mut read_budget = ScopeReadBudget::default();
    loop {
        tokio::select! {
            _ = receive_tick.tick() => {
                let result = receive_after_durable(
                    process_durable_account_work(provider, channel, account, &account_store,
                        &crypto_store, &gateway_channel, &session_store, &mut last_auth_warning),
                    async {
                    let client = ArkretHttpClient::from_provider(provider, &crypto_store).await?;
                    run_account_key_lifecycle_maintenance(&client, channel, account,
                        &account_store, &crypto_store, "agent_receive").await?;
                    if tokio::time::Instant::now() >= recovery_due {
                        recover_pending_content(&client, channel, account, &account_store, &crypto_store, &mut recovery_progress, &mut recovery_retries, &mut read_budget).await?;
                        recovery_due = tokio::time::Instant::now() + Duration::from_secs(20);
                    }
                    scan_installed_scopes(&client, channel, account, &account_store, &crypto_store, &mut scan_retries, &mut read_budget).await?;
                    Ok::<_, anyhow::Error>(())
                }).await;
                if let Err(error) = result {
                    return AccountEngineOutcome::Retry { error };
                }
                process_durable_account_work(provider, channel, account, &account_store,
                    &crypto_store, &gateway_channel, &session_store, &mut last_auth_warning).await;
                match account_store.pending(1).await {
                    Ok(pending) if pending.first().and_then(|item| item.last_error.as_ref()).is_some() => {
                        record_listener_failure(channel, account, "retry_wait",
                            pending[0].last_error.as_deref().unwrap_or_default());
                    }
                    _ => record_listener_phase(channel, account, "subscribing"),
                }
            }
            _ = presence_tick.tick() => {
                refresh_account_presence(provider, channel, account, &crypto_store).await;
            }
        }
    }
}

async fn receive_after_durable(
    durable: impl std::future::Future<Output = ()>,
    receive: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    durable.await;
    receive.await
}

#[derive(Debug)]
struct ReceiveStorageFailure;

impl std::fmt::Display for ReceiveStorageFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Agent receive durable storage failed")
    }
}

impl std::error::Error for ReceiveStorageFailure {}

fn receive_storage_error(error: impl Into<anyhow::Error>) -> anyhow::Error {
    error.into().context(ReceiveStorageFailure)
}

fn receive_crypto_error(error: anyhow::Error) -> anyhow::Error {
    // These methods also validate scope values. Only a durable storage failure is
    // global; a malformed current entry remains isolated to its exact scope.
    if error.is::<std::io::Error>()
        || error.is::<savfox_keyring_store::CredentialStoreError>()
        || error.is::<savfox_channels::arkret::ArkretCryptoStorageFailure>()
    {
        error.context(ReceiveStorageFailure)
    } else {
        error
    }
}

#[derive(Default)]
struct ScopeMaintenanceRetries {
    failures: std::collections::BTreeMap<
        garth::CursorScope,
        (arkret::RetrySchedule, tokio::time::Instant),
    >,
}

impl ScopeMaintenanceRetries {
    async fn run(
        &mut self,
        scope: garth::CursorScope,
        now: tokio::time::Instant,
        work: impl std::future::Future<Output = anyhow::Result<()>>,
    ) -> anyhow::Result<Option<anyhow::Error>> {
        if self
            .failures
            .get(&scope)
            .is_some_and(|(_, next)| now < *next)
        {
            return Ok(None);
        }
        match work.await {
            Ok(()) => {
                self.failures.remove(&scope);
                Ok(None)
            }
            Err(error) if error.is::<ReceiveStorageFailure>() => Err(error),
            Err(error) => {
                let now = now.max(tokio::time::Instant::now());
                let hint = scope_retry_hint(&error, now);
                let state = self.failures.entry(scope).or_insert_with(|| {
                    (
                        arkret::RetrySchedule::arkret_default()
                            .with_jitter(arkret::SPEC_JITTER_RATIO, rand::random()),
                        now,
                    )
                });
                state.1 = now + state.0.next_delay_with_hint(hint);
                Ok(Some(error))
            }
        }
    }
}

#[derive(Debug)]
struct ScopeReadDeferred(tokio::time::Instant);

impl std::fmt::Display for ScopeReadDeferred {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Agent endpoint retry budget is exhausted; pending scope retained")
    }
}

impl std::error::Error for ScopeReadDeferred {}

fn scope_retry_hint(error: &anyhow::Error, now: tokio::time::Instant) -> Option<Duration> {
    if let Some(deferred) = error.downcast_ref::<ScopeReadDeferred>() {
        return Some(deferred.0.saturating_duration_since(now));
    }
    let millis = match error.downcast_ref::<arkret::http_client::Error>() {
        Some(arkret::http_client::Error::Api { error, .. }) => error.retry_after_ms(),
        _ => match error.downcast_ref::<garth::Error>() {
            Some(garth::Error::Api { error, .. }) => error.retry_after_ms(),
            _ => None,
        },
    };
    millis.map(Duration::from_millis)
}

fn scope_service(scope: &garth::CursorScope) -> Option<arkret::DidCoreId> {
    match scope {
        garth::CursorScope::CommitStream { service_id, .. } => service_id.clone(),
        _ => None,
    }
}

#[derive(Default)]
struct ScopeReadBudget {
    // One drive is bound to a single authenticated Agent Account. Both scan
    // and recovery share these service/endpoint windows across exact scopes.
    attempts: std::collections::BTreeMap<
        (Option<arkret::DidCoreId>, &'static str),
        std::collections::VecDeque<tokio::time::Instant>,
    >,
}

impl ScopeReadBudget {
    async fn run<T>(
        &mut self,
        service: &Option<arkret::DidCoreId>,
        endpoint: &'static str,
        retrying: bool,
        now: tokio::time::Instant,
        read: impl std::future::Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        if retrying {
            let attempts = self
                .attempts
                .entry((service.clone(), endpoint))
                .or_default();
            while attempts
                .front()
                .is_some_and(|at| now.saturating_duration_since(*at) >= arkret::SPEC_RETRY_WINDOW)
            {
                attempts.pop_front();
            }
            if attempts.len() >= arkret::SPEC_MAX_RETRIES as usize {
                return Err(ScopeReadDeferred(
                    *attempts.front().expect("nonempty retry window") + arkret::SPEC_RETRY_WINDOW,
                )
                .into());
            }
            attempts.push_back(now);
        }
        read.await
    }
}

// Private candidate carriers are retained only in memory. Unrelated history
// pages are released after continuous verification, regardless of prefix length.
const MAX_RECOVERY_BYTES: usize = 8 * 1024 * 1024;

struct RecoveryProgress {
    session: arkret::http_client::own_station_results::OwnStationSessionSnapshot,
    expected_head: arkret::CommitStreamHead,
    baseline_through: u64,
    after: u64,
    replica: garth::own_station_results::OwnStationReplica,
    pages: Vec<garth::own_station_results::OwnStationScanPage>,
    events: Vec<ClientEvent>,
    retained_bytes: usize,
}

impl RecoveryProgress {
    fn accepts_checkpoint(&self, checkpoint: &StreamCheckpoint) -> bool {
        checkpoint.baseline_through == self.baseline_through
            && checkpoint.head.stream_ref == self.expected_head.stream_ref
            && (checkpoint.head.stream_position > self.expected_head.stream_position
                || checkpoint.head == self.expected_head)
    }

    fn complete(&self) -> bool {
        self.replica
            .head(&self.expected_head.stream_ref)
            .is_some_and(|head| {
                head.stream_position >= self.expected_head.stream_position
                    && self.after >= self.expected_head.stream_position
            })
    }

    fn stage(
        &mut self,
        page: garth::own_station_results::OwnStationScanPage,
        targets: &mut std::collections::BTreeSet<arkret::EventId>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            page.session() == &self.session,
            "recovery scan session changed"
        );
        let rows = page.rows()?;
        let previous_candidates = self.events.len();
        for item in rows {
            if item.commit().stream_position == self.expected_head.stream_position {
                anyhow::ensure!(
                    item.commit().commit_id == self.expected_head.commit_id,
                    "recovery scan forks its retained Commit head"
                );
            }
            if let arkret::CommittedEventView::Full(full) = item {
                if full.commit.stream_position <= self.expected_head.stream_position
                    && targets.remove(&full.event.event_id)
                {
                    self.events.push(ClientEvent::Committed(
                        garth::CommittedDelta::from_committed_event_view(
                            self.expected_head.stream_ref.realm_id().clone(),
                            item.clone(),
                        )?,
                    ));
                }
            }
        }
        if let Some(last) = rows.last() {
            self.after = last.commit().stream_position;
        }
        if self.events.len() != previous_candidates {
            // Keep the actual private HTTP carrier for each candidate. Pages
            // without candidates may be released once the closed replica has
            // checked their continuity; total history length is not a limit.
            let candidate_bytes = serde_json::to_vec(&self.events[previous_candidates..])?.len();
            self.retained_bytes = self
                .retained_bytes
                .checked_add(serde_json::to_vec(rows)?.len())
                .and_then(|bytes| bytes.checked_add(candidate_bytes))
                .context("recovery proof size overflow")?;
            anyhow::ensure!(
                self.retained_bytes <= MAX_RECOVERY_BYTES,
                "recovery candidates exceed bounded in-memory capacity"
            );
            self.pages.push(page);
        }
        Ok(())
    }

    fn check_publish_session(
        &self,
        own: &arkret::http_client::own_station_results::OwnStationResultClient,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(own.session()? == &self.session, "recovery session changed");
        for page in &self.pages {
            page.rows()?;
        }
        own.check_session()?;
        Ok(())
    }

    fn check_publish(
        &self,
        own: &arkret::http_client::own_station_results::OwnStationResultClient,
        checkpoint: &StreamCheckpoint,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.accepts_checkpoint(checkpoint),
            "recovery checkpoint regressed or forked"
        );
        anyhow::ensure!(
            self.complete(),
            "recovery has not reached its retained exact head"
        );
        self.check_publish_session(own)
    }
}

/// Re-read pending ciphertext through a continuous authorized scan all the way
/// to its captured durable head. Stored reconstructed headers are not evidence.
async fn recover_pending_content(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    progress: &mut std::collections::BTreeMap<String, RecoveryProgress>,
    retries: &mut ScopeMaintenanceRetries,
    read_budget: &mut ScopeReadBudget,
) -> anyhow::Result<()> {
    let pending_scopes = account_store.pending_cursor_scopes()?;
    // This runtime produces per-CommitStream batches. An unsupported aggregate
    // cannot be treated as independent private work or bypass its whole-frame ACK.
    if pending_scopes
        .iter()
        .any(|scope| !matches!(scope, garth::CursorScope::CommitStream { .. }))
    {
        return Ok(());
    }
    let state = crypto_store.load().map_err(receive_crypto_error)?;
    if state.unable_to_decrypt.is_empty() {
        progress.clear();
        return Ok(());
    }
    progress.retain(|group_id, _| {
        state
            .unable_to_decrypt
            .values()
            .any(|item| item.encrypted_content.group_id.as_str() == group_id)
    });
    let actor = arkret::ActorId::account(account.actor_account_id.clone());
    let installed = installed_scan_scopes(
        &state,
        account_store.cursor_scopes()?,
        &actor,
        &account_subscription_service_id(channel, account)?,
        &account_mls_endpoint(account)?,
    )?;
    for (group_id, epochs) in &state.mls_station_currents {
        if !state
            .mls_group_states
            .get(group_id)
            .is_some_and(|group| group.actor_id == actor)
        {
            continue;
        }
        let Some(current) = epochs.values().last() else {
            continue;
        };
        let mut targets = state
            .unable_to_decrypt
            .values()
            .filter(|item| item.encrypted_content.group_id.as_str() == group_id)
            .map(|item| item.event_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        if targets.is_empty() {
            continue;
        }
        let stream_ref = arkret::CommitStreamRef::from_scope(&current.effective_scope, None)?;
        if !installed.contains(&stream_ref) {
            continue;
        }
        let scope = garth::CursorScope::CommitStream {
            service_id: account_subscription_service_id(channel, account)?,
            stream_ref: stream_ref.clone(),
        };
        if !recovery_scope_available(&pending_scopes, &scope) {
            continue;
        }
        let retrying = retries.failures.contains_key(&scope);
        let error = retries
            .run(scope.clone(), tokio::time::Instant::now(), async {
                let Some(bytes) = account_store
                    .load(scope.clone())
                    .await
                    .map_err(receive_storage_error)?
                else {
                    return Ok(());
                };
                let checkpoint: StreamCheckpoint =
                    serde_json::from_str(&bytes).map_err(receive_storage_error)?;
                anyhow::ensure!(
                    checkpoint.head.stream_ref == stream_ref,
                    "recovery checkpoint belongs to another stream"
                );
                let own = client.own_station()?;
                let session = own.session()?.clone();
                // Removal before any fallible work also discards a failed or stale pass.
                let retained = progress.remove(group_id);
                let mut recovery = if let Some(retained) = retained.filter(|retained| {
                    retained.session == session && retained.accepts_checkpoint(&checkpoint)
                }) {
                    retained
                } else {
                    let mut replica = garth::own_station_results::OwnStationReplica::new(
                        stream_ref.realm_id().clone(),
                    );
                    let position = checkpoint.baseline_through;
                    anyhow::ensure!(
                        position <= checkpoint.head.stream_position,
                        "recovery baseline exceeds its retained head"
                    );
                    let request = arkret::StreamScanRequest {
                        realm_id: stream_ref.realm_id().clone(),
                        stream_ref: stream_ref.clone(),
                        direction: arkret::StreamScanDirection::Before(Some(
                            position
                                .checked_add(1)
                                .context("recovery position overflow")?,
                        )),
                        limit: 1,
                    };
                    let anchor = read_budget
                        .run(
                            &scope_service(&scope),
                            ServiceOperationId::SELF_COMMITTED_EVENT_READ_SCAN_V1,
                            retrying,
                            tokio::time::Instant::now(),
                            async { Ok(own.scan_commit_stream(&request).await?) },
                        )
                        .await?;
                    let [row] = anchor.value()?.committed_events.as_slice() else {
                        anyhow::bail!("recovery position has no exact visible anchor");
                    };
                    let commit = row.commit();
                    anyhow::ensure!(
                        commit.stream_position == position,
                        "recovery anchor changed position"
                    );
                    let head = arkret::CommitStreamHead {
                        stream_ref: stream_ref.clone(),
                        stream_position: position,
                        commit_id: commit.commit_id.clone(),
                    };
                    if position == checkpoint.head.stream_position {
                        anyhow::ensure!(
                            head == checkpoint.head,
                            "recovery anchor forks durable head"
                        );
                    }
                    replica.restore_bound_head(own, &head, anchor).await?;
                    RecoveryProgress {
                        session,
                        expected_head: checkpoint.head.clone(),
                        baseline_through: checkpoint.baseline_through,
                        after: position,
                        replica,
                        pages: Vec::new(),
                        events: Vec::new(),
                        retained_bytes: 0,
                    }
                };
                // The per-pass network budget remains eight pages of at most 200 rows.
                // Finding ciphertext alone is insufficient: its chain must reach the
                // durable head captured when this recovery started.
                for _ in 0..8 {
                    if recovery.complete() {
                        break;
                    }
                    let request = arkret::StreamScanRequest {
                        realm_id: stream_ref.realm_id().clone(),
                        stream_ref: stream_ref.clone(),
                        direction: arkret::StreamScanDirection::After(Some(recovery.after)),
                        limit: 200,
                    };
                    let snapshot_response = read_budget
                        .run(
                            &scope_service(&scope),
                            ServiceOperationId::SELF_REALM_STATE_SNAPSHOT_READ_MANIFEST_HEAD_V1,
                            retrying,
                            tokio::time::Instant::now(),
                            async { Ok(own.snapshot_head(&request.realm_id).await?) },
                        )
                        .await?;
                    recovery
                        .replica
                        .install_bound_snapshot(&snapshot_response)?;
                    let snapshot = snapshot_response.into_value()?;
                    let response = read_budget
                        .run(
                            &scope_service(&scope),
                            ServiceOperationId::SELF_COMMITTED_EVENT_READ_SCAN_V1,
                            retrying,
                            tokio::time::Instant::now(),
                            async { Ok(own.scan_commit_stream(&request).await?) },
                        )
                        .await?;
                    let admitted = recovery.replica.apply_bound_scan(own, response).await?;
                    for item in admitted.rows()? {
                        require_own_station_scan_cut(
                            item,
                            &snapshot.visible_stream_heads,
                            snapshot.governance_generation,
                        )?;
                    }
                    let page_len = admitted.rows()?.len();
                    recovery.stage(admitted, &mut targets)?;
                    if recovery.complete() {
                        break;
                    }
                    // Empty or short pages do not prove the captured head. Retain only
                    // bounded, fenced progress and try again on the next maintenance pass.
                    if page_len < 200 {
                        break;
                    }
                }
                if recovery.complete() {
                    let latest = account_store
                        .load(scope.clone())
                        .await
                        .map_err(receive_storage_error)?
                        .context("recovery durable checkpoint disappeared")?;
                    let latest: StreamCheckpoint =
                        serde_json::from_str(&latest).map_err(receive_storage_error)?;
                    publish_recovery(recovery, own, &latest, account_store, scope).await?;
                } else {
                    recovery.check_publish_session(own)?;
                    progress.insert(group_id.clone(), recovery);
                }
                Ok(())
            })
            .await?;
        if let Some(error) = error {
            warn!(
                "arkret: independent stream recovery failed; exact scope retained for retry: {error:#}"
            );
        }
    }
    Ok(())
}

async fn publish_recovery(
    recovery: RecoveryProgress,
    own: &arkret::http_client::own_station_results::OwnStationResultClient,
    checkpoint: &StreamCheckpoint,
    store: &garth::FileStore,
    scope: garth::CursorScope,
) -> anyhow::Result<()> {
    recovery.check_publish(own, checkpoint)?;
    if !recovery.events.is_empty() {
        // Do not replace the live cursor with this older recovery checkpoint.
        store
            .commit(scope, None, recovery.events)
            .await
            .map_err(receive_storage_error)?;
    }
    Ok(())
}

fn recovery_scope_available(
    pending_scopes: &std::collections::BTreeSet<garth::CursorScope>,
    scope: &garth::CursorScope,
) -> bool {
    // Aggregate Account delivery has no independently acknowledged stream cut.
    pending_scopes.iter().all(|pending| {
        matches!(pending, garth::CursorScope::CommitStream { .. }) && pending != scope
    })
}

fn account_mls_endpoint(
    account: &ArkretAccountConfig,
) -> anyhow::Result<arkret::MlsEndpointIdentity> {
    Ok(arkret::MlsEndpointIdentity::AgentRuntime {
        agent_id: account.actor_account_id.principal_id.clone(),
        verification_method: arkret::DidUrl::new(
            account
                .verification_method
                .clone()
                .context("Agent scan runtime key is unavailable")?,
        )
        .map_err(anyhow::Error::msg)?,
        agent_key_authorize_event_id: arkret::EventId::new(
            account
                .authorized_event_ref
                .clone()
                .context("Agent scan runtime authorization is unavailable")?,
        )?,
    })
}

/// The authenticated own Station verifies governance history. Consumers still
/// bind each independent stream and retained producer proof to its current cut.
fn require_own_station_scan_cut(
    item: &arkret::CommittedEventView,
    heads: &[arkret::CommitStreamHead],
    generation: u64,
) -> anyhow::Result<()> {
    let commit = item.commit();
    let mut matching = heads
        .iter()
        .filter(|head| head.stream_ref == commit.stream_ref);
    let head = matching
        .next()
        .context("Agent scan scope has no authorized own-Station head")?;
    anyhow::ensure!(
        matching.next().is_none(),
        "Agent scan scope has duplicate own-Station heads"
    );
    anyhow::ensure!(
        commit.governance_generation <= generation
            && commit.stream_position <= head.stream_position,
        "Agent scan exceeds the authorized own-Station cut"
    );
    if commit.stream_position == head.stream_position {
        anyhow::ensure!(
            commit.commit_id == head.commit_id,
            "Agent scan forks the authorized own-Station head"
        );
    }
    if let arkret::CommittedEventView::Full(full) = item {
        full.event.verify_producer_proof_self_consistency(
            full.event.realm_id.digest_suite_code().digest_suite(),
        )?;
    }
    Ok(())
}

fn installed_scan_scopes(
    state: &savfox_channels::arkret::ArkretCryptoStateFile,
    cursors: Vec<garth::CursorScope>,
    actor: &arkret::ActorId,
    service: &Option<arkret::DidCoreId>,
    endpoint: &arkret::MlsEndpointIdentity,
) -> anyhow::Result<std::collections::BTreeSet<arkret::CommitStreamRef>> {
    let mut scopes = std::collections::BTreeSet::new();
    for cursor in cursors {
        let garth::CursorScope::CommitStream {
            service_id,
            stream_ref,
        } = cursor
        else {
            continue;
        };
        if &service_id != service {
            continue;
        }
        let scope = match &stream_ref {
            arkret::CommitStreamRef::Realm { realm_id } => arkret::ScopeRef::Realm {
                realm_id: realm_id.clone(),
            },
            arkret::CommitStreamRef::Circle {
                realm_id,
                circle_id,
            } => arkret::ScopeRef::Circle {
                realm_id: realm_id.clone(),
                circle_id: circle_id.clone(),
            },
            arkret::CommitStreamRef::Sidecar {
                realm_id,
                sidecar_id,
            } => arkret::ScopeRef::Sidecar {
                realm_id: realm_id.clone(),
                sidecar_id: sidecar_id.clone(),
            },
            _ => anyhow::bail!("unsupported installed Agent stream scope"),
        };
        let group_id = scope.canonical_mls_group_id()?;
        if state
            .mls_group_states
            .get(group_id.as_str())
            .is_some_and(|group| {
                &group.actor_id == actor
                    && &group.endpoint == endpoint
                    && group.group_id == group_id
            })
        {
            scopes.insert(stream_ref);
        }
    }
    Ok(scopes)
}

#[cfg(test)]
mod installed_scope_tests {
    use super::*;

    #[tokio::test]
    async fn endpoint_retry_budget_is_shared_by_scopes_and_preserves_fresh_private_reads() {
        let mut budget = ScopeReadBudget::default();
        let now = tokio::time::Instant::now();
        let service = Some(arkret::DidCoreId::new("ak:did_core:web:station.example").unwrap());
        let endpoint = ServiceOperationId::SELF_COMMITTED_EVENT_READ_SCAN_V1;
        let calls = std::cell::Cell::new(0);
        for _ in 0..arkret::SPEC_MAX_RETRIES {
            budget
                .run(&service, endpoint, true, now, async {
                    calls.set(calls.get() + 1);
                    Ok(())
                })
                .await
                .unwrap();
        }
        let error = budget
            .run::<()>(&service, endpoint, true, now, async {
                panic!("another failed scope bypassed the endpoint retry window")
            })
            .await
            .unwrap_err();
        assert!(error.is::<ScopeReadDeferred>());
        budget
            .run(&service, endpoint, false, now, async {
                calls.set(calls.get() + 1);
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(calls.get(), arkret::SPEC_MAX_RETRIES + 1);
        budget
            .run(
                &service,
                endpoint,
                true,
                now + arkret::SPEC_RETRY_WINDOW,
                async {
                    calls.set(calls.get() + 1);
                    Ok(())
                },
            )
            .await
            .unwrap();
        assert_eq!(calls.get(), arkret::SPEC_MAX_RETRIES + 2);
    }

    #[tokio::test]
    async fn scope_retry_preserves_long_http_hint_and_local_jitter_ladder() {
        let (scope, _) = maintenance_test_scopes();
        let now = tokio::time::Instant::now() + Duration::from_secs(1);
        let mut retries = ScopeMaintenanceRetries::default();
        let error = arkret::http_client::Error::Api {
            status: 503,
            error: Box::new(
                arkret::Problem::new("temporarily_unavailable", 503, "scope is unavailable")
                    .with_retry_after_ms(Some(600_000)),
            ),
        };
        assert!(
            retries
                .run(scope.clone(), now, async { Err(error.into()) })
                .await
                .unwrap()
                .is_some()
        );
        let next = retries.failures[&scope].1;
        assert_eq!(next.duration_since(now), Duration::from_secs(600));
        assert!(
            retries
                .run(scope.clone(), next, async {
                    Err(arkret::http_client::Error::Api {
                        status: 503,
                        error: Box::new(
                            arkret::Problem::new(
                                "temporarily_unavailable",
                                503,
                                "scope is unavailable",
                            )
                            .with_retry_after_ms(Some(0)),
                        ),
                    }
                    .into())
                })
                .await
                .unwrap()
                .is_some()
        );
        let delay = retries.failures[&scope].1.duration_since(next);
        assert!(delay >= Duration::from_secs(2));
        assert!(delay <= Duration::from_millis(2400));
    }

    #[tokio::test]
    async fn scope_retry_delay_starts_after_failed_work_instead_of_old_request_start() {
        let (scope, _) = maintenance_test_scopes();
        let mut retries = ScopeMaintenanceRetries::default();
        let start = tokio::time::Instant::now() - Duration::from_secs(5);
        let completed = std::cell::Cell::new(tokio::time::Instant::now());
        assert!(
            retries
                .run(scope.clone(), start, async {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    completed.set(tokio::time::Instant::now());
                    anyhow::bail!("scope transport timed out");
                })
                .await
                .unwrap()
                .is_some()
        );
        assert!(retries.failures[&scope].1 >= completed.get() + arkret::SPEC_INITIAL_DELAY);
    }

    #[tokio::test]
    async fn non_io_credential_backend_failure_is_global_but_current_validation_is_local() {
        let (scope, _) = maintenance_test_scopes();
        let mut retries = ScopeMaintenanceRetries::default();
        let failure = savfox_keyring_store::CredentialStoreError::new(keyring::Error::NoEntry);
        let error = retries
            .run(scope.clone(), tokio::time::Instant::now(), async {
                Err(receive_crypto_error(failure.into()))
            })
            .await
            .unwrap_err();
        assert!(error.is::<ReceiveStorageFailure>());
        assert!(error.is::<savfox_keyring_store::CredentialStoreError>());
        assert!(retries.failures.is_empty());
        assert!(
            retries
                .run(scope, tokio::time::Instant::now(), async {
                    Err(receive_crypto_error(anyhow::anyhow!(
                        "current value differs from scope selector"
                    )))
                })
                .await
                .unwrap()
                .is_some()
        );
    }

    fn maintenance_test_scopes() -> (garth::CursorScope, garth::CursorScope) {
        let id = |byte| arkret::EventId::from_digest(arkret::DigestSuite::Sha256, [byte; 32]);
        let realm = arkret::RealmId::from_event_id(&id(30));
        (
            garth::CursorScope::CommitStream {
                service_id: None,
                stream_ref: arkret::CommitStreamRef::Realm {
                    realm_id: realm.clone(),
                },
            },
            garth::CursorScope::CommitStream {
                service_id: None,
                stream_ref: arkret::CommitStreamRef::Sidecar {
                    realm_id: realm,
                    sidecar_id: arkret::SidecarId::from_event_id(&id(31)),
                },
            },
        )
    }

    #[tokio::test]
    async fn durable_private_work_precedes_failing_network_maintenance() {
        let home = tempfile::tempdir().unwrap();
        let store = garth::FileStore::open(home.path().join("receive-order.json")).unwrap();
        let (_, private) = maintenance_test_scopes();
        store
            .commit(
                private.clone(),
                Some("verified-private-cut".into()),
                vec![ClientEvent::AccountUpdates(Default::default())],
            )
            .await
            .unwrap();
        let order = std::cell::RefCell::new(Vec::new());
        let result = receive_after_durable(
            async {
                order.borrow_mut().push("durable");
                let heads = store.ready_inbox_heads(0, &BTreeSet::new(), 1).unwrap();
                assert_eq!(heads[0].scope, private);
                store.ack(heads[0].id).await.unwrap();
            },
            async {
                order.borrow_mut().push("maintenance");
                anyhow::bail!("source watch transport unavailable");
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(*order.borrow(), vec!["durable", "maintenance"]);
        assert!(store.pending(1).await.unwrap().is_empty());
        assert_eq!(
            store.load(private).await.unwrap().as_deref(),
            Some("verified-private-cut")
        );
    }

    #[tokio::test]
    async fn independent_scope_maintenance_retains_bad_shared_and_persists_private() {
        let home = tempfile::tempdir().unwrap();
        let store = garth::FileStore::open(home.path().join("scope-maintenance.json")).unwrap();
        let (shared, private) = maintenance_test_scopes();
        store
            .save(shared.clone(), "verified-shared-cut".into())
            .await
            .unwrap();
        let mut retries = ScopeMaintenanceRetries::default();
        let now = tokio::time::Instant::now();
        let mut calls = Vec::new();
        for scope in [shared.clone(), private.clone()] {
            let error = retries
                .run(scope.clone(), now, async {
                    calls.push(scope.clone());
                    if scope == shared {
                        anyhow::bail!("shared page has no verified current cut");
                    }
                    store
                        .commit(
                            scope.clone(),
                            Some("verified-private-cut".into()),
                            vec![ClientEvent::AccountUpdates(Default::default())],
                        )
                        .await
                        .map_err(receive_storage_error)?;
                    Ok(())
                })
                .await
                .unwrap();
            assert_eq!(error.is_some(), scope == shared);
        }
        assert_eq!(calls, vec![shared.clone(), private.clone()]);
        assert_eq!(
            store.load(shared.clone()).await.unwrap().as_deref(),
            Some("verified-shared-cut")
        );
        let heads = store.ready_inbox_heads(0, &BTreeSet::new(), 32).unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].scope, private);
        let mut other_service = shared.clone();
        if let garth::CursorScope::CommitStream { service_id, .. } = &mut other_service {
            *service_id =
                Some(arkret::DidCoreId::new("ak:did_core:web:other-station.example").unwrap());
        }
        let mut other_service_ran = false;
        assert!(
            retries
                .run(other_service, now, async {
                    other_service_ran = true;
                    Ok(())
                })
                .await
                .unwrap()
                .is_none()
        );
        assert!(other_service_ran);
        let blocked = retries
            .run(shared.clone(), now, async {
                panic!("failed shared scope retried before its deadline")
            })
            .await
            .unwrap();
        assert!(blocked.is_none());
        assert!(
            retries
                .run(shared.clone(), now + Duration::from_secs(2), async {
                    Ok(())
                })
                .await
                .unwrap()
                .is_none()
        );
        assert!(retries.failures.is_empty());
        let storage = retries
            .run(shared, now, async {
                Err(receive_storage_error(anyhow::anyhow!(
                    "durable write failed"
                )))
            })
            .await;
        assert!(storage.unwrap_err().is::<ReceiveStorageFailure>());
        assert!(retries.failures.is_empty());
    }

    #[test]
    fn recovery_isolates_exact_stream_pending_but_preserves_account_batch_barrier() {
        let id = |byte| arkret::EventId::from_digest(arkret::DigestSuite::Sha256, [byte; 32]);
        let realm = arkret::RealmId::from_event_id(&id(20));
        let shared = garth::CursorScope::CommitStream {
            service_id: None,
            stream_ref: arkret::CommitStreamRef::Realm {
                realm_id: realm.clone(),
            },
        };
        let private = garth::CursorScope::CommitStream {
            service_id: None,
            stream_ref: arkret::CommitStreamRef::Sidecar {
                realm_id: realm,
                sidecar_id: arkret::SidecarId::from_event_id(&id(21)),
            },
        };
        let pending = std::collections::BTreeSet::from([shared.clone()]);
        assert!(!recovery_scope_available(&pending, &shared));
        assert!(recovery_scope_available(&pending, &private));
        assert!(!recovery_scope_available(
            &std::collections::BTreeSet::from([shared, private.clone()]),
            &private
        ));
        let aggregate = garth::CursorScope::Account {
            service_id: None,
            actor_id: arkret::ActorId::account(arkret::AccountId::new(
                arkret::DidCoreId::new("ak:did_core:web:agent.example").unwrap(),
                arkret::DidCoreId::new("ak:did_core:web:station.example").unwrap(),
            )),
            device_id: arkret::DeviceId::new("ak:device:01964139-0000-7000-8000-000000000001")
                .unwrap(),
        };
        assert!(!recovery_scope_available(
            &std::collections::BTreeSet::from([aggregate]),
            &private
        ));
    }

    #[test]
    fn recovery_and_live_scans_reject_wrong_stream_generation_and_forked_cuts() {
        let event = arkret::EventId::from_digest(arkret::DigestSuite::Sha256, [1; 32]);
        let realm = arkret::RealmId::from_event_id(&event);
        let stream = arkret::CommitStreamRef::Realm {
            realm_id: realm.clone(),
        };
        let commit_id = arkret::RealmCommitId::from_digest([2; 32]);
        // This fixture tests cut binding of an already authenticated result,
        // not governance signature verification or producer-proof validity.
        let item = arkret::CommittedEventView::Withheld(arkret::CommittedEventWithheldView {
            commit: arkret::RealmCommit {
                commit_id: commit_id.clone(),
                realm_id: realm.clone(),
                stream_ref: stream.clone(),
                stream_position: 2,
                previous_commit_ref: Some(arkret::RealmCommitId::from_digest([1; 32])),
                event_ref: event.clone(),
                governance_generation: 1,
                authority_ref: arkret::RealmCommitAuthorityRef::GenesisOrChangeEvent(event),
                committed_at: Utc::now(),
                producer_signer_fact_digest: None,
                signature: arkret::DetachedObjectSignature {
                    context: arkret::DetachedSignatureContext::RealmCommit,
                    signature_algorithm: arkret::DetachedSignatureAlgorithm::Ed25519,
                    verification_method: arkret::DidUrl::new("did:web:station.example#notary")
                        .unwrap(),
                    signed_digest: arkret::Hash::new(format!("sha256:{}", "00".repeat(32)))
                        .unwrap(),
                    created_at: Utc::now(),
                    sig: arkret::Base64UrlString::new("AA").unwrap(),
                },
            },
            event_disclosure: arkret::EventDisclosure {
                status: arkret::EventDisclosureStatus::Withheld,
            },
        });
        let head = arkret::CommitStreamHead {
            stream_ref: stream,
            stream_position: 2,
            commit_id,
        };
        assert!(require_own_station_scan_cut(&item, std::slice::from_ref(&head), 1).is_ok());
        assert!(require_own_station_scan_cut(&item, std::slice::from_ref(&head), 0).is_err());
        assert!(require_own_station_scan_cut(&item, &[], 1).is_err());
        assert!(require_own_station_scan_cut(&item, &[head.clone(), head.clone()], 1).is_err());
        let mut forked = head.clone();
        forked.commit_id = arkret::RealmCommitId::from_digest([3; 32]);
        assert!(require_own_station_scan_cut(&item, &[forked], 1).is_err());
        let mut stale = head.clone();
        stale.stream_position = 1;
        assert!(require_own_station_scan_cut(&item, &[stale], 1).is_err());
        let mut foreign = head;
        foreign.stream_ref = arkret::CommitStreamRef::Sidecar {
            realm_id: realm,
            sidecar_id: arkret::SidecarId::from_event_id(&arkret::EventId::from_digest(
                arkret::DigestSuite::Sha256,
                [4; 32],
            )),
        };
        assert!(require_own_station_scan_cut(&item, &[foreign], 1).is_err());
    }

    #[test]
    fn accepted_welcome_checkpoint_discovers_scope_before_current_cache_and_fences_endpoint() {
        let home = tempfile::tempdir().unwrap();
        let crypto = FileArkretCryptoStore::new(home.path(), "installed-scope-discovery".into());
        let mut state = crypto.load().unwrap();
        let id = |byte| arkret::EventId::from_digest(arkret::DigestSuite::Sha256, [byte; 32]);
        let service = arkret::DidCoreId::new("ak:did_core:web:scope-station.example").unwrap();
        let principal = arkret::DidCoreId::new("ak:did_core:web:scope-agent.example").unwrap();
        let actor =
            arkret::ActorId::account(arkret::AccountId::new(principal.clone(), service.clone()));
        let endpoint = arkret::MlsEndpointIdentity::AgentRuntime {
            agent_id: principal,
            verification_method: arkret::DidUrl::new("did:web:scope-agent.example#runtime")
                .unwrap(),
            agent_key_authorize_event_id: id(9),
        };
        let realm = arkret::RealmId::from_event_id(&id(1));
        let scopes = [
            arkret::ScopeRef::Realm {
                realm_id: realm.clone(),
            },
            arkret::ScopeRef::Sidecar {
                realm_id: realm,
                sidecar_id: arkret::SidecarId::from_event_id(&id(2)),
            },
        ];
        let streams = scopes
            .iter()
            .map(|scope| arkret::CommitStreamRef::from_scope(scope, None).unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        let cursors = streams
            .iter()
            .map(|stream| garth::CursorScope::CommitStream {
                service_id: Some(service.clone()),
                stream_ref: stream.clone(),
            })
            .collect::<Vec<_>>();
        for scope in scopes {
            let group = scope.canonical_mls_group_id().unwrap();
            // This test covers discovery metadata only, not MLS installation.
            state.mls_group_states.insert(
                group.to_string(),
                arkret::MlsGroupStateRecord {
                    group_id: group,
                    actor_id: actor.clone(),
                    endpoint: endpoint.clone(),
                    epoch: 1,
                    serialized_state: Vec::new(),
                    updated_at: Utc::now(),
                },
            );
        }
        assert!(state.mls_station_currents.is_empty());
        assert_eq!(
            installed_scan_scopes(
                &state,
                cursors.clone(),
                &actor,
                &Some(service.clone()),
                &endpoint
            )
            .unwrap(),
            streams
        );
        assert!(
            installed_scan_scopes(
                &state,
                Vec::new(),
                &actor,
                &Some(service.clone()),
                &endpoint
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            installed_scan_scopes(&state, cursors.clone(), &actor, &None, &endpoint)
                .unwrap()
                .is_empty()
        );
        let other_actor = arkret::ActorId::account(arkret::AccountId::new(
            arkret::DidCoreId::new("ak:did_core:web:other-agent.example").unwrap(),
            service.clone(),
        ));
        assert!(
            installed_scan_scopes(
                &state,
                cursors.clone(),
                &other_actor,
                &Some(service.clone()),
                &endpoint
            )
            .unwrap()
            .is_empty()
        );
        let mut replaced = endpoint.clone();
        if let arkret::MlsEndpointIdentity::AgentRuntime {
            agent_key_authorize_event_id,
            ..
        } = &mut replaced
        {
            *agent_key_authorize_event_id = id(10);
        }
        assert!(
            installed_scan_scopes(&state, cursors, &actor, &Some(service), &replaced)
                .unwrap()
                .is_empty()
        );
    }
}

async fn scan_installed_scopes(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
    retries: &mut ScopeMaintenanceRetries,
    read_budget: &mut ScopeReadBudget,
) -> anyhow::Result<()> {
    let actor = arkret::ActorId::account(account.actor_account_id.clone());
    let state = crypto_store.load().map_err(receive_crypto_error)?;
    // Welcome installation persists private group state and its accepted Add
    // checkpoint before ACK. Discover those streams before fetching current;
    // the current cache can legitimately be empty at the first receive tick.
    let service_id = account_subscription_service_id(channel, account)?;
    let endpoint = account_mls_endpoint(account)?;
    let scopes = installed_scan_scopes(
        &state,
        account_store.cursor_scopes()?,
        &actor,
        &service_id,
        &endpoint,
    )?;
    for stream_ref in scopes {
        let scope = garth::CursorScope::CommitStream {
            service_id: service_id.clone(),
            stream_ref: stream_ref.clone(),
        };
        let retrying = retries.failures.contains_key(&scope);
        let error = retries
            .run(scope.clone(), tokio::time::Instant::now(), async {
                let realm_id = stream_ref.realm_id().clone();
                let checkpoint: Option<StreamCheckpoint> = account_store
                    .load(scope.clone())
                    .await
                    .map_err(receive_storage_error)?
                    .map(|bytes| serde_json::from_str(&bytes))
                    .transpose()
                    .map_err(receive_storage_error)?;
                let own = client.own_station()?;
                let mut replica =
                    garth::own_station_results::OwnStationReplica::new(realm_id.clone());
                let previous = checkpoint
                    .as_ref()
                    .context("installed Agent scope has no durable checkpoint")?;
                read_budget
                    .run(
                        &scope_service(&scope),
                        ServiceOperationId::SELF_COMMITTED_EVENT_READ_SCAN_V1,
                        retrying,
                        tokio::time::Instant::now(),
                        async { Ok(replica.restore_checkpoint_head(own, &previous.head).await?) },
                    )
                    .await?;
                let snapshot_response = read_budget
                    .run(
                        &scope_service(&scope),
                        ServiceOperationId::SELF_REALM_STATE_SNAPSHOT_READ_MANIFEST_HEAD_V1,
                        retrying,
                        tokio::time::Instant::now(),
                        async { Ok(own.snapshot_head(&realm_id).await?) },
                    )
                    .await?;
                replica.install_bound_snapshot(&snapshot_response)?;
                let snapshot = snapshot_response.into_value()?;
                own.check_session()?;
                crypto_store
                    .record_verified_mls_current_entries(&realm_id, &snapshot.current_state_entries)
                    .map_err(receive_crypto_error)?;
                crypto_store
                    .record_verified_direct_conversation_current_entries(
                        &realm_id,
                        &actor,
                        &snapshot.current_state_entries,
                    )
                    .map_err(receive_crypto_error)?;
                let baseline_through = checkpoint
                    .as_ref()
                    .context("installed Agent scope has no durable accepted Welcome checkpoint")?
                    .baseline_through;
                let request = arkret::StreamScanRequest {
                    realm_id: realm_id.clone(),
                    stream_ref: stream_ref.clone(),
                    direction: arkret::StreamScanDirection::After(
                        checkpoint.as_ref().map(|c| c.head.stream_position),
                    ),
                    limit: 200,
                };
                let response = read_budget
                    .run(
                        &scope_service(&scope),
                        ServiceOperationId::SELF_COMMITTED_EVENT_READ_SCAN_V1,
                        retrying,
                        tokio::time::Instant::now(),
                        async { Ok(own.scan_commit_stream(&request).await?) },
                    )
                    .await?;
                let page = response.value()?.clone();
                let admitted = replica.apply_bound_scan(own, response).await?;
                admitted.rows()?;
                if page.committed_events.is_empty() {
                    return Ok(());
                }
                page.validate_for_request(&request)?;
                let (after, _) = read_budget
                    .run(
                        &scope_service(&scope),
                        ServiceOperationId::SELF_REALM_STATE_SNAPSHOT_READ_MANIFEST_HEAD_V1,
                        retrying,
                        tokio::time::Instant::now(),
                        governance::verified_scope_snapshot(client, &realm_id),
                    )
                    .await?;
                if let Some(previous) = &checkpoint {
                    anyhow::ensure!(
                        previous.head.stream_ref == stream_ref
                            && page.committed_events[0]
                                .commit()
                                .previous_commit_ref
                                .as_ref()
                                == Some(&previous.head.commit_id),
                        "Agent scan does not extend the durable verified Commit head"
                    );
                }
                for item in &page.committed_events {
                    require_own_station_scan_cut(
                        item,
                        &after.visible_stream_heads,
                        after.governance_generation,
                    )?;
                }
                // Validate the whole page before its first write. Split the baseline
                // prefix from live work; each unit and exact head persist atomically.
                let split = page
                    .committed_events
                    .partition_point(|item| item.commit().stream_position <= baseline_through);
                for (rows, baseline) in [
                    (&page.committed_events[..split], true),
                    (&page.committed_events[split..], false),
                ] {
                    let Some(last) = rows.last().map(|item| item.commit()) else {
                        continue;
                    };
                    let mut events = rows
                        .iter()
                        .map(|item| {
                            garth::CommittedDelta::from_committed_event_view(
                                realm_id.clone(),
                                item.clone(),
                            )
                            .map(ClientEvent::Committed)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if baseline {
                        // Local delivery metadata only; no account aggregate or human
                        // DeviceId is read or manufactured on the Agent transport.
                        events.insert(
                            0,
                            ClientEvent::AccountUpdates(garth::AccountUpdateContext {
                                initial_catchup: true,
                                ..Default::default()
                            }),
                        );
                    }
                    let new_checkpoint = StreamCheckpoint {
                        head: arkret::CommitStreamHead {
                            stream_ref: stream_ref.clone(),
                            stream_position: last.stream_position,
                            commit_id: last.commit_id.clone(),
                        },
                        baseline_through,
                    };
                    own.check_session()?;
                    account_store
                        .commit(
                            scope.clone(),
                            Some(serde_json::to_string(&new_checkpoint)?),
                            events,
                        )
                        .await
                        .map_err(receive_storage_error)?;
                }
                Ok(())
            })
            .await?;
        if let Some(error) = error {
            warn!(
                "arkret: independent stream scan failed; exact scope retained for retry: {error:#}"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod recovery_tests {
    use std::io::{Read, Write};

    use arkret::http_client::own_station_results::{
        OwnStationResultClient, OwnStationSessionSnapshot, OwnStationSessionSource,
    };

    use super::*;

    struct Source(StdMutex<OwnStationSessionSnapshot>);
    impl OwnStationSessionSource for Source {
        fn snapshot(&self) -> arkret::http_client::Result<OwnStationSessionSnapshot> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    fn fixture_commit(
        event: &arkret::Event,
        position: u64,
        previous: Option<arkret::RealmCommitId>,
    ) -> arkret::RealmCommit {
        let mut commit = arkret::RealmCommit {
            commit_id: arkret::RealmCommitId::from_digest([0; 32]),
            realm_id: event.realm_id.clone(),
            stream_ref: arkret::CommitStreamRef::Realm {
                realm_id: event.realm_id.clone(),
            },
            stream_position: position,
            previous_commit_ref: previous,
            event_ref: if position == 0 {
                event.event_id.clone()
            } else {
                arkret::EventId::from_digest(
                    arkret::canonical::DigestSuite::Sha256,
                    [position as u8; 32],
                )
            },
            governance_generation: 0,
            authority_ref: arkret::RealmCommitAuthorityRef::GenesisOrChangeEvent(
                event.event_id.clone(),
            ),
            committed_at: "2026-10-05T00:00:01.000Z".parse().unwrap(),
            producer_signer_fact_digest: None,
            signature: arkret::DetachedObjectSignature {
                context: arkret::DetachedSignatureContext::RealmCommit,
                signature_algorithm: arkret::DetachedSignatureAlgorithm::Ed25519,
                verification_method: arkret::DidUrl::new("did:web:station.example#key-1").unwrap(),
                signed_digest: arkret::Hash::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
                created_at: "2026-10-05T00:00:01.000Z".parse().unwrap(),
                sig: arkret::Base64UrlString::new("AA").unwrap(),
            },
        };
        seal_fixture_commit(&mut commit);
        commit
    }

    fn seal_fixture_commit(commit: &mut arkret::RealmCommit) {
        let body =
            arkret::canonical::canonical::unsigned_value(commit, &["commit_id", "signature"])
                .unwrap();
        commit.commit_id = arkret::RealmCommitId::from_digest(arkret::canonical::sha256_bytes(
            &arkret::canonical::canonical_json_bytes(&body).unwrap(),
        ));
        commit.signature = arkret::signatures::detached_object::sign_detached_object(
            &arkret::canonical::canonical::unsigned_value(commit, &["signature"]).unwrap(),
            arkret::DetachedSignatureContext::RealmCommit,
            arkret::DidUrl::new("did:web:station.example#key-1").unwrap(),
            commit.committed_at,
            &ed25519_dalek::SigningKey::from_bytes(&[83; 32]),
        )
        .unwrap();
        commit.validate_content_address().unwrap();
    }

    fn target() -> (arkret::Event, ed25519_dalek::SigningKey) {
        let key = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
        let realm_id =
            arkret::RealmId::new("ak:realm:AfF-hFqRoMbajXkPapH-xaq0xwK-UKt2ph2zTs9JZRAO").unwrap();
        let event = arkret::Event {
            event_id: arkret::EventId::from_digest(arkret::canonical::DigestSuite::Sha256, [0; 32]),
            kind: arkret::EventKind::MessageCreate,
            scope_ref: arkret::ScopeRef::Realm {
                realm_id: realm_id.clone(),
            },
            realm_id,
            actor_id: arkret::ActorId::account(arkret::AccountId::new(
                DidCoreId::new("ak:did_core:web:alice.example").unwrap(),
                DidCoreId::new("ak:did_core:web:station.example").unwrap(),
            )),
            executed_by: None,
            authorization_ref: None,
            applet_id: None,
            external_ref: None,
            created_at: "2026-10-05T00:00:00.000Z".parse().unwrap(),
            semantic_refs: Vec::new(),
            payload: serde_json::from_value(json!({
                "strand_id": "ak:strand:AXh0mpVGb536xVxbSPfM4Wc_1WuXAxTYgmtXEncKM9T0",
                "track_name": "discussion",
                "content": {"kind": "ak.content.text", "body": "recovery fixture"}
            }))
            .unwrap(),
            producer_proof: None,
        };
        let mut event = arkret::AuthoredEvent::finalize_with_digest_suite(
            event,
            arkret::canonical::DigestSuite::Sha256,
        )
        .unwrap();
        arkret::signatures::sign_event(
            &mut event,
            &arkret::signatures::Ed25519DetachedJwsSigner::new(
                key.clone(),
                "did:web:alice.example#ak:device:0196419b-0000-7000-8000-000000000001",
            ),
            arkret::signatures::SignEventOptions::new()
                .with_created_at("2026-10-05T00:00:00.000Z".parse().unwrap()),
        )
        .unwrap();
        (event.into_event(), key)
    }

    fn http(
        bodies: Vec<Value>,
    ) -> (
        OwnStationResultClient,
        Arc<Source>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let station = DidCoreId::new("ak:did_core:web:station.example").unwrap();
        let binding = arkret::StationConnectionBinding {
            service_id: station.clone(),
            base_url: base.clone(),
            trust_domain: arkret::TrustDomainId::new("ak:trust_domain:station.example").unwrap(),
            auth_metadata: arkret::AuthMetadata::minimal(),
        };
        let session = OwnStationSessionSnapshot::new(
            binding.clone(),
            arkret::AccountId::new(
                DidCoreId::new("ak:did_core:web:alice.example").unwrap(),
                station.clone(),
            ),
            station,
            arkret_wire::SessionGrantId::from_issuance_digest([3; 32]),
            0,
            "test-grant".into(),
        )
        .unwrap()
        .with_provider_identity(Default::default());
        let source = Arc::new(Source(StdMutex::new(session)));
        let raw = arkret::http_client::ClientBuilder::new(url::Url::parse(&base).unwrap())
            .allow_insecure_localhost()
            .auth(arkret::http_client::Auth::Bearer("test-grant".into()))
            .build()
            .unwrap();
        let server = std::thread::spawn(move || {
            for body in bodies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 8192];
                let header_end = loop {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(index) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        break index + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                assert!(headers.contains("authorization: bearer test-grant"));
                let len = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + len {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                let body = if let Some(key) = body.get("__signer_key") {
                    let request: arkret::SignerKeysQueryRequestBody =
                        serde_json::from_slice(&request[header_end..header_end + len]).unwrap();
                    serde_json::to_value(arkret::SignerKeysQueryOutcome {
                        request_id: request.request_id,
                        realm_id: request.realm_id,
                        recipient_account_id: request.recipient_account_id,
                        results: vec![arkret::SignerKeyQueryResult::HistoricalResolved {
                            selector: request.queries[0].clone(),
                            key: serde_json::from_value(key.clone()).unwrap(),
                            accepted_at: serde_json::from_value(body["__accepted_at"].clone())
                                .unwrap(),
                        }],
                    })
                    .unwrap()
                } else {
                    body
                };
                let bytes = serde_json::to_vec(&body).unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", bytes.len()).unwrap();
                stream.write_all(&bytes).unwrap();
            }
        });
        (
            OwnStationResultClient::new(raw, binding, source.clone()).unwrap(),
            source,
            server,
        )
    }

    fn setup(
        fork: bool,
    ) -> (
        OwnStationResultClient,
        Arc<Source>,
        std::thread::JoinHandle<()>,
        RecoveryProgress,
        arkret::EventId,
        Vec<arkret::RealmCommit>,
    ) {
        let (event, key) = target();
        // Transport fixture inputs describe an independently located original
        // PCR authorization. They do not prove actual PG admission/archival or
        // download PCR history; the real historical query and Event Ed checks
        // remain the client's only producer-key verification path here.
        let authorization_event = arkret::EventId::from_digest(
            arkret::canonical::DigestSuite::Sha256,
            arkret::canonical::sha256_bytes(b"recovery original Device authorization"),
        );
        let pcr_create = arkret::EventId::from_digest(
            arkret::canonical::DigestSuite::Sha256,
            arkret::canonical::sha256_bytes(b"recovery original PCR genesis"),
        );
        let mut pcr_genesis = fixture_commit(&event, 0, None);
        pcr_genesis.realm_id = arkret::RealmId::from_event_id(&pcr_create);
        pcr_genesis.stream_ref = arkret::CommitStreamRef::Realm {
            realm_id: pcr_genesis.realm_id.clone(),
        };
        pcr_genesis.event_ref = pcr_create.clone();
        pcr_genesis.authority_ref =
            arkret::RealmCommitAuthorityRef::GenesisOrChangeEvent(pcr_create.clone());
        pcr_genesis.committed_at = "2026-10-05T00:00:00.000Z".parse().unwrap();
        seal_fixture_commit(&mut pcr_genesis);
        let mut authorization = fixture_commit(&event, 1, Some(pcr_genesis.commit_id.clone()));
        authorization.event_ref = authorization_event;
        authorization.realm_id = arkret::RealmId::from_event_id(&pcr_create);
        authorization.stream_ref = arkret::CommitStreamRef::Realm {
            realm_id: authorization.realm_id.clone(),
        };
        authorization.authority_ref =
            arkret::RealmCommitAuthorityRef::GenesisOrChangeEvent(pcr_create);
        authorization.committed_at = "2026-10-05T00:00:00.000Z".parse().unwrap();
        seal_fixture_commit(&mut authorization);
        let resolved = arkret::ResolvedSignerKey {
            public_key_b64u: arkret::Base64UrlString::new(arkret::canonical::base64url_encode(
                key.verifying_key().as_bytes(),
            ))
            .unwrap(),
            authorization_ref: arkret::CommittedEventRef {
                event_id: authorization.event_ref.clone(),
                commit_id: authorization.commit_id.clone(),
                stream_ref: authorization.stream_ref.clone(),
                stream_position: authorization.stream_position,
            },
            revision: arkret::CurrentRevision {
                commit_id: authorization.commit_id.clone(),
                stream_position: authorization.stream_position,
            },
            governance_generation: authorization.governance_generation,
        };
        let human = event.human_device_producer().unwrap().unwrap();
        assert_eq!(
            human.account_id.station_id,
            DidCoreId::new("ak:did_core:web:station.example").unwrap()
        );
        assert!(
            authorization.committed_at
                <= "2026-10-05T00:00:01.000Z"
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .unwrap()
        );
        let original_fact =
            arkret_models_collaboration::authority_commit::HumanHistoricalSignerFact {
                event_id: event.event_id.clone(),
                actor: event.actual_signer().clone(),
                device_id: human.device_id,
                verification_method: event
                    .producer_proof
                    .as_ref()
                    .unwrap()
                    .verification_method
                    .clone(),
                key: resolved.clone(),
                accepted_at: authorization.committed_at,
            };
        original_fact
            .validate_event_binding(&event, arkret::canonical::DigestSuite::Sha256)
            .unwrap();
        let mut first = fixture_commit(&event, 0, None);
        first.producer_signer_fact_digest = Some(original_fact.digest().unwrap());
        seal_fixture_commit(&mut first);
        assert_ne!(resolved.authorization_ref.commit_id, first.commit_id);
        assert_ne!(resolved.authorization_ref.event_id, first.event_ref);
        assert_ne!(resolved.authorization_ref.stream_ref, first.stream_ref);
        assert_ne!(original_fact.accepted_at, first.committed_at);
        let second = fixture_commit(&event, 1, Some(first.commit_id.clone()));
        let third = fixture_commit(
            &event,
            2,
            Some(if fork {
                first.commit_id.clone()
            } else {
                second.commit_id.clone()
            }),
        );
        let bodies = vec![
            json!({"committed_events":[{"commit":first,"event":event}],"readable_floor":{"oldest_position":0,"floor_commit_id":first.commit_id,"floor_reason":"stream_start"},"truncated":false}),
            json!({"__signer_key":resolved,"__accepted_at":original_fact.accepted_at}),
            json!({"committed_events":[{"commit":second,"event_disclosure":{"status":"withheld"}}],"truncated":false}),
            json!({"committed_events":[{"commit":third,"event_disclosure":{"status":"withheld"}}],"truncated":false}),
        ];
        let (own, source, server) = http(bodies);
        let progress = RecoveryProgress {
            session: own.session().unwrap().clone(),
            expected_head: arkret::CommitStreamHead {
                stream_ref: third.stream_ref.clone(),
                stream_position: 2,
                commit_id: third.commit_id.clone(),
            },
            baseline_through: 0,
            after: 0,
            replica: garth::own_station_results::OwnStationReplica::new(event.realm_id.clone()),
            pages: Vec::new(),
            events: Vec::new(),
            retained_bytes: 0,
        };
        (
            own,
            source,
            server,
            progress,
            event.event_id,
            vec![first, second, third],
        )
    }

    async fn advance(
        recovery: &mut RecoveryProgress,
        own: &OwnStationResultClient,
        after: Option<u64>,
        targets: &mut std::collections::BTreeSet<arkret::EventId>,
    ) -> anyhow::Result<()> {
        let request = arkret::StreamScanRequest {
            realm_id: recovery.expected_head.stream_ref.realm_id().clone(),
            stream_ref: recovery.expected_head.stream_ref.clone(),
            direction: arkret::StreamScanDirection::After(after),
            limit: 200,
        };
        let admitted = recovery
            .replica
            .apply_bound_scan(own, own.scan_commit_stream(&request).await?)
            .await?;
        recovery.stage(admitted, targets)
    }

    #[tokio::test]
    async fn recovery_actual_prefix_stages_target_until_exact_durable_head() {
        let (own, _, server, mut recovery, target_id, commits) = setup(false);
        let mut targets = std::collections::BTreeSet::from([target_id]);
        let home = tempfile::tempdir().unwrap();
        let store = garth::FileStore::open(home.path().join("inbox.json")).unwrap();
        let checkpoint = StreamCheckpoint {
            head: recovery.expected_head.clone(),
            baseline_through: 0,
        };
        let scope = garth::CursorScope::CommitStream {
            service_id: None,
            stream_ref: checkpoint.head.stream_ref.clone(),
        };
        advance(&mut recovery, &own, None, &mut targets)
            .await
            .unwrap();
        assert!(
            targets.is_empty(),
            "target is found before its durable head"
        );
        assert_eq!(recovery.events.len(), 1);
        assert!(recovery.check_publish(&own, &checkpoint).is_err());
        assert!(store.pending(10).await.unwrap().is_empty());
        advance(&mut recovery, &own, Some(0), &mut targets)
            .await
            .unwrap();
        assert!(!recovery.complete());
        assert_eq!(
            recovery.pages.len(),
            1,
            "unrelated verified prefix pages are released"
        );
        advance(&mut recovery, &own, Some(1), &mut targets)
            .await
            .unwrap();
        assert!(recovery.complete());
        let mut forked = StreamCheckpoint {
            head: checkpoint.head.clone(),
            baseline_through: 0,
        };
        forked.head.commit_id = commits[0].commit_id.clone();
        assert!(
            !recovery.accepts_checkpoint(&forked),
            "same-position durable fork is rejected"
        );
        forked.head.stream_position = 1;
        assert!(
            !recovery.accepts_checkpoint(&forked),
            "durable rollback is rejected"
        );
        let mut advanced = checkpoint;
        advanced.head.stream_position += 1;
        advanced.head.commit_id =
            fixture_commit(&target().0, 3, Some(commits[2].commit_id.clone())).commit_id;
        // A concurrently advanced normal cursor is never overwritten by the
        // older, now complete recovery unit.
        let cursor = serde_json::to_string(&advanced).unwrap();
        store.save(scope.clone(), cursor.clone()).await.unwrap();
        publish_recovery(recovery, &own, &advanced, &store, scope.clone())
            .await
            .unwrap();
        assert_eq!(
            store.load(scope).await.unwrap().as_deref(),
            Some(cursor.as_str())
        );
        let inbox = store.pending(10).await.unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].payload.len(), 1);
        assert!(matches!(&inbox[0].payload[0], ClientEvent::Committed(_)));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn recovery_actual_fork_after_target_does_not_enqueue() {
        let (own, _, server, mut recovery, target, _) = setup(true);
        let mut targets = std::collections::BTreeSet::from([target]);
        advance(&mut recovery, &own, None, &mut targets)
            .await
            .unwrap();
        advance(&mut recovery, &own, Some(0), &mut targets)
            .await
            .unwrap();
        let error = advance(&mut recovery, &own, Some(1), &mut targets)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("fork"));
        let home = tempfile::tempdir().unwrap();
        let store = garth::FileStore::open(home.path().join("inbox.json")).unwrap();
        let checkpoint = StreamCheckpoint {
            head: recovery.expected_head.clone(),
            baseline_through: 0,
        };
        let scope = garth::CursorScope::CommitStream {
            service_id: None,
            stream_ref: checkpoint.head.stream_ref.clone(),
        };
        assert!(
            publish_recovery(recovery, &own, &checkpoint, &store, scope)
                .await
                .is_err()
        );
        assert!(store.pending(10).await.unwrap().is_empty());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn recovery_late_session_aba_discards_completed_candidates() {
        let (own, source, server, mut recovery, target, _) = setup(false);
        let mut targets = std::collections::BTreeSet::from([target]);
        advance(&mut recovery, &own, None, &mut targets)
            .await
            .unwrap();
        advance(&mut recovery, &own, Some(0), &mut targets)
            .await
            .unwrap();
        advance(&mut recovery, &own, Some(1), &mut targets)
            .await
            .unwrap();
        let old = source.snapshot().unwrap();
        *source.0.lock().unwrap() = OwnStationSessionSnapshot::new(
            old.binding().clone(),
            old.account_id().clone(),
            old.account_id().station_id.clone(),
            old.grant_id().clone(),
            old.epoch(),
            "test-grant".into(),
        )
        .unwrap()
        .with_provider_identity(Default::default());
        assert!(
            recovery.pages[0].rows().is_err(),
            "retained actual HTTP carrier is stale"
        );
        let home = tempfile::tempdir().unwrap();
        let store = garth::FileStore::open(home.path().join("inbox.json")).unwrap();
        let checkpoint = StreamCheckpoint {
            head: recovery.expected_head.clone(),
            baseline_through: 0,
        };
        let scope = garth::CursorScope::CommitStream {
            service_id: None,
            stream_ref: checkpoint.head.stream_ref.clone(),
        };
        assert!(
            publish_recovery(recovery, &own, &checkpoint, &store, scope)
                .await
                .is_err()
        );
        assert!(store.pending(10).await.unwrap().is_empty());
        server.join().unwrap();
    }
}
