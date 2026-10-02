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
    http: &arkret::http_client::Client,
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
    let page = http.scan_commit_stream(&request).await?;
    let [arkret::CommittedEventView::Full(exact)] = page.committed_events.as_slice() else {
        anyhow::bail!("Welcome checkpoint requires its exact visible accepted Commit");
    };
    anyhow::ensure!(
        exact == accepted,
        "Welcome checkpoint read changed the accepted Add"
    );
    let authority_request = arkret::AuthorityBundleRequest {
        realm_id: welcome.realm_id.clone(),
        nonce: arkret::Base64UrlString::new(arkret::base64url_encode(rand::random::<[u8; 32]>()))
            .map_err(anyhow::Error::msg)?,
    };
    let bundle = http.realm_authority_bundle(&authority_request).await?;
    let keys =
        garth::fetch_historical_station_key_directory(http, &bundle, Some(&page), None).await?;
    let freshness =
        arkret::identity::RealmAuthorityFreshness::new(Utc::now(), authority_request.nonce);
    let authority = arkret::identity::verify_realm_authority_bundle(&bundle, &freshness, &keys)?;
    authority.verify_committed_item(exact, &keys)?;
    let checkpoint = StreamCheckpoint {
        head: arkret::CommitStreamHead {
            stream_ref,
            stream_position: exact.commit.stream_position,
            commit_id: exact.commit.commit_id.clone(),
        },
        baseline_through: exact.commit.stream_position,
    };
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
    loop {
        tokio::select! {
            _ = receive_tick.tick() => {
                let result = async {
                    let client = ArkretHttpClient::from_inner(provider.provide().await?);
                    run_account_key_lifecycle_maintenance(&client, channel, account,
                        &account_store, &crypto_store, "agent_receive").await?;
                    scan_installed_scopes(&client, channel, account, &account_store, &crypto_store).await?;
                    process_durable_account_work(provider, channel, account, &account_store,
                        &crypto_store, &gateway_channel, &session_store, &mut last_auth_warning).await;
                    Ok::<_, anyhow::Error>(())
                }.await;
                if let Err(error) = result {
                    return AccountEngineOutcome::Retry { error };
                }
                record_listener_phase(channel, account, "subscribing");
            }
            _ = presence_tick.tick() => {
                refresh_account_presence(provider, channel, account, &crypto_store).await;
            }
        }
    }
}

async fn scan_installed_scopes(
    client: &ArkretHttpClient,
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
    account_store: &garth::FileStore,
    crypto_store: &FileArkretCryptoStore,
) -> anyhow::Result<()> {
    let actor = arkret::ActorId::account(account.actor_account_id.clone());
    let state = crypto_store.load()?;
    let scopes = state
        .mls_station_currents
        .iter()
        .filter(|(group_id, _)| {
            state
                .mls_group_states
                .get(*group_id)
                .is_some_and(|group| group.actor_id == actor)
        })
        .flat_map(|(_, epochs)| epochs.values())
        .map(|current| arkret::CommitStreamRef::from_scope(&current.effective_scope, None))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    for stream_ref in scopes {
        let realm_id = stream_ref.realm_id().clone();
        let scope = garth::CursorScope::CommitStream {
            service_id: account_subscription_service_id(channel, account)?,
            stream_ref: stream_ref.clone(),
        };
        let checkpoint: Option<StreamCheckpoint> = account_store
            .load(scope.clone())
            .await?
            .map(|bytes| serde_json::from_str(&bytes))
            .transpose()?;
        let (snapshot, _) = governance::verified_scope_snapshot(client.inner(), &realm_id).await?;
        crypto_store
            .record_verified_mls_current_entries(&realm_id, &snapshot.current_state_entries)?;
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
        let page = client.inner().scan_commit_stream(&request).await?;
        if page.committed_events.is_empty() {
            continue;
        }
        let authority_request = arkret::AuthorityBundleRequest {
            realm_id: realm_id.clone(),
            nonce: arkret::Base64UrlString::new(arkret::base64url_encode(
                rand::random::<[u8; 32]>(),
            ))
            .map_err(anyhow::Error::msg)?,
        };
        let bundle = client
            .inner()
            .realm_authority_bundle(&authority_request)
            .await?;
        let keys = garth::fetch_historical_station_key_directory(
            client.inner(),
            &bundle,
            Some(&page),
            None,
        )
        .await?;
        let freshness =
            arkret::identity::RealmAuthorityFreshness::new(Utc::now(), authority_request.nonce);
        let authority =
            arkret::identity::verify_realm_authority_bundle(&bundle, &freshness, &keys)?;
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
            match item {
                arkret::CommittedEventView::Full(full) => {
                    authority.verify_committed_item(full, &keys)?
                }
                arkret::CommittedEventView::Withheld(withheld) => {
                    authority.verify_commit(&withheld.commit, &keys)?
                }
            }
            let commit = item.commit();
            if stream_ref == bundle.realm_stream_head.stream_ref {
                anyhow::ensure!(
                    commit.stream_position <= bundle.realm_stream_head.stream_position,
                    "Agent scan exceeds the verified authority cut"
                );
                if commit.stream_position == bundle.realm_stream_head.stream_position {
                    anyhow::ensure!(
                        commit.commit_id == bundle.realm_stream_head.commit_id,
                        "Agent scan forks the verified authority head"
                    );
                }
            }
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
                    garth::CommittedDelta::from_committed_event_view(realm_id.clone(), item.clone())
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
            account_store
                .commit(
                    scope.clone(),
                    Some(serde_json::to_string(&new_checkpoint)?),
                    events,
                )
                .await?;
        }
    }
    Ok(())
}
