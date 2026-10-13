//! Native branch of the existing Arkret listener, not a generic coordinator.
use futures_util::StreamExt;
use savfox_channels::arkret::{ArkretNativeCredentialSource, load_arkret_channel_configs};
use savfox_core::models_manager::manager::ModelsManager;
use savfox_core::{AuthManager, ModelClient, ResponseEvent, TransportManager};
use savfox_http_client::{HttpClient, ReqwestTransport};

use super::super::*;
use super::guarded_transport::{NativeGuardedTransport, NativeTransportHandle};
use super::*;
use crate::arkret_delivery::{
    ArkretExecutionBindingStore, NativeReplyPreparation, NativeReplySlot,
};

pub(in crate::channels::arkret) struct NativeReceiver {
    home: PathBuf,
    host: Arc<dyn NativePublicReplyHost>,
    ledger: Arc<ArkretExecutionBindingStore>,
    config: Arc<savfox_core::config::Config>,
    private_channel: Arc<GatewayChannel>,
    private_sessions: Arc<SessionStore>,
}

pub async fn start_native_arkret_public_replies(
    home: PathBuf,
    host: Arc<dyn NativePublicReplyHost>,
) -> anyhow::Result<NativePublicReplyHandle> {
    let config = Arc::new(
        savfox_core::config::ConfigBuilder::default()
            .savfox_home(home.clone())
            .build()
            .await?,
    );
    let private_sessions = Arc::new(SessionStore::from_home(&home));
    let (outgoing_tx, _outgoing_rx) =
        tokio::sync::mpsc::channel::<crate::channel::BridgeOutgoing>(128);
    let private_channel = Arc::new(GatewayChannel::new(crate::channel::GatewayBridgeArgs {
        config: Arc::clone(&config),
        session_store: Arc::clone(&private_sessions),
        cli_overrides: Vec::new(),
        cloud_requirements: savfox_core::config_loader::CloudRequirementsLoader::default(),
        feedback: savfox_feedback::SavfoxFeedback::new(),
        savfox_linux_sandbox_exe: None,
        websocket_manager: crate::session::GatewaySessionManager::new(),
        outgoing_tx,
        channel_registry: crate::channels::create_channel_registry(),
        channel_recovery_registry: crate::channels::recovery::create_channel_recovery_registry(),
        channel_recovery_supervisors:
            crate::channels::recovery::create_channel_recovery_supervisors(),
    }));
    crate::ws_rpc::inject_all_provider_auth(&private_channel).await;
    let ledger = Arc::new(ArkretExecutionBindingStore::new(&home));
    // No implicit missing-file initialize; original onboarding owns initialize_once.
    let receiver = Arc::new(NativeReceiver {
        home,
        host,
        ledger,
        config,
        private_channel,
        private_sessions,
    });
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
    let children = Arc::new(std::sync::Mutex::new((
        false,
        Vec::<tokio::task::AbortHandle>::new(),
    )));
    let child_handles = Arc::clone(&children);
    let task = tokio::spawn(async move {
        let mut accounts = NativeAccountTasks(HashMap::new());
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let channels = tokio::select! {
                biased;
                _ = &mut stop_rx => break,
                result = async { tick.tick().await; load_arkret_channel_configs(&receiver.home).await } => match result {
                Ok(value) => value,
                Err(_) => {
                    // Invalid/unbound material revokes every previous child;
                    // retaining an old consumer would ignore an explicit unbind.
                    accounts.abort_and_wait().await;
                    continue;
                }
            }
            };
            if stop_rx.try_recv().is_ok() {
                break;
            }
            let mut seen = HashSet::new();
            for channel in channels {
                for account in channel.accounts.iter().filter(|account| account.listen) {
                    let key = task_key(&channel.id, &account.id);
                    seen.insert(key.clone());
                    let fingerprint = match native_account_configuration_digest(&channel, account) {
                        Ok(value) => value,
                        Err(_) => continue,
                    };
                    if accounts
                        .0
                        .get(&key)
                        .is_some_and(|(old, handle)| old == &fingerprint && !handle.is_finished())
                    {
                        continue;
                    }
                    if let Some((_, handle)) = accounts.0.remove(&key) {
                        handle.abort();
                        let _ = handle.await;
                    }
                    advance_account_runtime_generation(&key);
                    let receiver = Arc::clone(&receiver);
                    let channel = channel.clone();
                    let account = account.clone();
                    let handle = {
                        // The same mutex serializes cancellation with creation.
                        // Once Drop marks closed, no new native account can start.
                        let mut owned = child_handles.lock().expect("native child ownership");
                        if owned.0 {
                            break;
                        }
                        let handle = tokio::spawn(async move {
                            let _ = receiver.listen(channel, account).await;
                        });
                        owned.1.retain(|task| !task.is_finished());
                        owned.1.push(handle.abort_handle());
                        handle
                    };
                    accounts.0.insert(key, (fingerprint, handle));
                }
            }
            let removed: Vec<_> = accounts
                .0
                .keys()
                .filter(|key| !seen.contains(*key))
                .cloned()
                .collect();
            for key in removed {
                if let Some((_, handle)) = accounts.0.remove(&key) {
                    handle.abort();
                    let _ = handle.await;
                    advance_account_runtime_generation(&key);
                }
            }
        }
        // Abort before waiting: no receive/model/body guard survives native
        // handle cancellation. Await task drops before the private manager
        // receives its existing normal Shutdown operations.
        accounts.abort_and_wait().await;
        let _ = receiver
            .private_channel
            .session_manager()
            .remove_and_close_all_sessions()
            .await;
    });
    Ok(NativePublicReplyHandle {
        task,
        stop: Some(stop_tx),
        children,
    })
}
fn native_account_configuration_digest(
    channel: &ArkretChannelConfig,
    account: &ArkretAccountConfig,
) -> anyhow::Result<String> {
    Ok(arkret::canonical::canonical_sha256(&serde_json::json!({
        "channel": channel.id, "base_url": channel.base_url,
        "service_id": channel.service_id, "delivery_mode": channel.delivery_mode,
        "account": account.id, "mode": account.mode.as_str(),
        "principal_id": account.principal_id, "actor": account.actor_account_id,
        "controller": account.controller_account_id, "device_id": account.device_id,
        "key_ref": account.key_ref, "verification_method": account.verification_method,
        "bootstrap": account.inkson_bootstrap, "authorized_event": account.authorized_event_ref,
        "scope": account.requested_scope, "signer_evidence": account.current_signer_evidence,
        "signer_resolution": account.signer_resolution_evidence_ref,
        "listen": account.listen, "send": account.send
    }))?)
}

async fn native_require_current_account_config(
    home: &std::path::Path,
    channel_id: &str,
    account_id: &str,
    captured_digest: &str,
) -> anyhow::Result<()> {
    // The child may have been scheduled AFTER disabled/erase. Re-read after
    // acquiring the owner lease, before touching any inbox/crypto. Once this
    // matches, an unbind cannot erase until the same lease is dropped.
    for channel in load_arkret_channel_configs(&home.to_path_buf()).await? {
        if channel.id != channel_id {
            continue;
        }
        for account in &channel.accounts {
            if account.id == account_id && account.listen {
                anyhow::ensure!(
                    native_account_configuration_digest(&channel, account)? == captured_digest,
                    "native account configuration changed before ownership"
                );
                return Ok(());
            }
        }
    }
    anyhow::bail!("native account no longer enabled for ownership")
}

impl NativeAccountTasks {
    async fn abort_and_wait(&mut self) {
        // Abort every task first. Then wait for all guard/body/owner drops.
        for (_, task) in self.0.values() {
            task.abort();
        }
        for (_, (_, task)) in self.0.drain() {
            let _ = task.await;
        }
    }
}

impl NativeReceiver {
    async fn listen(
        &self,
        channel: ArkretChannelConfig,
        account: ArkretAccountConfig,
    ) -> anyhow::Result<()> {
        let crypto = FileArkretCryptoStore::for_account(&self.home, &channel.id, &account.id);
        let _native_owner = crypto.try_acquire_native_owner()?;
        native_require_current_account_config(
            &self.home,
            &channel.id,
            &account.id,
            &native_account_configuration_digest(&channel, &account)?,
        )
        .await?;
        // Maintenance/Private receipt continues before explicit shared onboarding.
        // Public prepare itself requires the durable existing native lane.
        let store = open_account_store(
            &self.home,
            &channel.id,
            &account.id,
            ACCOUNT_EVENT_DEDUPE_MAX,
        )?;
        crypto.ensure_created()?;
        let (provider, credentials) =
            construct_native_account_provider(&self.home, &channel, &account).await?;
        receive::drive_native(
            self,
            &provider,
            &credentials,
            &channel,
            &account,
            store,
            crypto,
        )
        .await
    }

    pub(in crate::channels::arkret) async fn process_inbox(
        &self,
        provider: &ArkretAgentSessionProvider,
        credentials: &ArkretNativeCredentialSource,
        client: &ArkretHttpClient,
        channel: &ArkretChannelConfig,
        account: &ArkretAccountConfig,
        store: &garth::FileStore,
        crypto: &FileArkretCryptoStore,
    ) -> anyhow::Result<()> {
        let mut blocked = BTreeSet::new();
        loop {
            let deliveries =
                store.ready_inbox_heads(Utc::now().timestamp_millis(), &blocked, 32)?;
            if deliveries.is_empty() {
                return Ok(());
            }
            for delivery in deliveries {
                let inbound_mode = account_inbound_mode(&delivery.payload);
                let mut result = Ok(());
                for event in delivery.payload {
                    if let Err(error) = self
                        .accept_client_event(
                            provider,
                            credentials,
                            client,
                            event,
                            inbound_mode,
                            channel,
                            account,
                            store,
                            crypto,
                        )
                        .await
                    {
                        result = Err(error);
                        break;
                    }
                }
                if !settle_account_delivery(store, delivery.id, delivery.attempts, result).await? {
                    blocked.insert(delivery.scope);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn accept_client_event(
        &self,
        provider: &ArkretAgentSessionProvider,
        credentials: &ArkretNativeCredentialSource,
        client: &ArkretHttpClient,
        event: ClientEvent,
        mode: AccountInboundMode,
        channel: &ArkretChannelConfig,
        account: &ArkretAccountConfig,
        store: &garth::FileStore,
        crypto: &FileArkretCryptoStore,
    ) -> anyhow::Result<()> {
        // One inbox/MLS owner. Only the fixed shared branch changes; Sidecar
        // and the exact private Direct keep their ORIGINAL dispatcher/context.
        let original_private = match &event {
            ClientEvent::Committed(delta) => match delta.event() {
                Some(event) => {
                    matches!(event.scope_ref, arkret::ScopeRef::Sidecar { .. })
                        || event_declares_direct_conversation_realm(event)
                        || event.kind == "ak.direct_conversation.bound"
                        || crypto.realm_is_direct_conversation(event.realm_id.as_str())?
                }
                None => false,
            },
            _ => false,
        };
        if original_private {
            return handle_account_client_event(
                provider,
                client,
                event,
                mode,
                channel,
                account,
                store,
                crypto,
                &self.private_channel,
                &self.private_sessions,
            )
            .await;
        }
        let delta = match event {
            ClientEvent::AccountUpdates(updates) => {
                return handle_sync_updates_for_account(
                    client, updates, channel, account, store, crypto,
                )
                .await;
            }
            ClientEvent::RealmProjection { value, .. } => {
                crypto.record_station_mls_currents(&value)?;
                return Ok(());
            }
            ClientEvent::Committed(delta) => delta,
            _ => return Ok(()),
        };
        let Some(event) = delta.event().cloned() else {
            return Ok(());
        };
        let accepted = arkret::CommittedEventFullView {
            event: event.clone(),
            commit: delta.commit().clone(),
        };
        apply_account_accepted_mls_commit(client, crypto, &accepted, account).await?;
        if event.kind != "ak.message.create" {
            return Ok(());
        }
        anyhow::ensure!(
            matches!(
                event.scope_ref,
                arkret::ScopeRef::Realm { .. } | arkret::ScopeRef::Circle { .. }
            ),
            "native factory handles only shared MessageCreate"
        );

        if account_event_seen(store, channel, account, event.event_id.as_str()).await? {
            return Ok(());
        }
        if event.actor_id == arkret::ActorId::account(account.actor_account_id.clone()) {
            remember_account_event(store, event.event_id.as_str()).await?;
            return Ok(());
        }
        let reference = delta.committed_ref();
        let parsed =
            parse_backfill_events_for_account(&delta.realm_id, vec![event.clone()], account);
        let mut inbound = parsed.events.into_iter().next();
        if inbound.is_none() {
            for skipped in parsed.skipped {
                let Some(payload) = skipped.encrypted_payload.as_ref() else {
                    continue;
                };
                // Reuses the original sealed accepted cache/RFC decrypt, not a second crypto
                // implementation.
                let ArkretDecryptDetailedOutcome::Decrypted {
                    content,
                    consume_bindings,
                } = crypto.try_decrypt_native_shared_reply(&reference, payload)?
                else {
                    anyhow::bail!("native source waits for original MLS state");
                };
                consume_account_mls_key_packages(
                    client,
                    channel,
                    account,
                    crypto,
                    &consume_bindings,
                )
                .await;
                let body = decrypted_text_body(&content).context("native source is not text")?;
                inbound = Some(ArkretInboundEvent {
                    account_id: skipped.account_id,
                    event_id: event.event_id.to_string(),
                    realm_id: event.realm_id.to_string(),
                    scope_ref: event.scope_ref.clone(),
                    chat_type: skipped.chat_type,
                    participant_count: skipped.participant_count,
                    strand_id: skipped.strand_id,
                    sender_did: skipped
                        .sender_did
                        .context("native source lacks signer identity")?,
                    sender_actor_id: skipped.sender_actor_id,
                    body,
                    thread_root_id: skipped.reply_to,
                    mentioned_account_ids: structured_mention_account_ids(&content),
                    sidecar_exchange: None,
                });
                break;
            }
        }
        if mode.suppresses_agent_dispatch() {
            remember_account_event(store, event.event_id.as_str()).await?;
            return Ok(());
        }
        let inbound = inbound.context("native source cannot be parsed")?;
        anyhow::ensure!(
            inbound.sidecar_exchange.is_none(),
            "native reply is not an exchange action"
        );
        // A validated full Account mention selects this runtime. Delivery by
        // itself does not dispatch any model or tool. Host rechecks current
        // Public participation/third-party selection under the same cut.
        if !inbound_agent_is_mentioned(&inbound, account) {
            remember_account_event(store, event.event_id.as_str()).await?;
            return Ok(());
        }
        let caller = event
            .actor_id
            .as_account_id()
            .context("native MessageCreate caller is not an Account")?
            .clone();
        anyhow::ensure!(
            inbound.sender_actor_id.as_ref() == Some(&event.actor_id),
            "native parser changed full caller"
        );
        let source = Arc::new(NativeReplySource {
            channel_id: channel.id.clone(),
            configured_account_id: account.id.clone(),
            agent: account.actor_account_id.clone(),
            controller: account.controller_account_id.clone(),
            caller,
            scope: event.scope_ref.clone(),
            strand: arkret::StrandId::new(
                inbound.strand_id.context("native source has no Strand")?,
            )?,
            text: inbound.body,
            credentials: credentials.source_request(&event.event_id).await?,
            accepted,
        });
        self.reply(source, channel, account, store, crypto).await?;
        remember_account_event(store, event.event_id.as_str()).await?;
        Ok(())
    }

    async fn reply(
        &self,
        source: Arc<NativeReplySource>,
        channel: &ArkretChannelConfig,
        account: &ArkretAccountConfig,
        store: &garth::FileStore,
        crypto: &FileArkretCryptoStore,
    ) -> anyhow::Result<()> {
        let source_ref = arkret::CommittedEventRef {
            event_id: source.accepted.event.event_id.clone(),
            commit_id: source.accepted.commit.commit_id.clone(),
            stream_ref: source.accepted.commit.stream_ref.clone(),
            stream_position: source.accepted.commit.stream_position,
        };
        let slot = NativeReplySlot {
            conversation: crate::arkret_delivery::RemoteConversationKey {
                channel_config_id: channel.id.clone(),
                account_id: account.id.clone(),
                realm_id: source.accepted.event.realm_id.to_string(),
                strand_id: source.strand.to_string(),
                stream_ref: source_ref.stream_ref.clone(),
            },
            source: source_ref,
            scope: source.scope.clone(),
            strand_id: source.strand.clone(),
            agent: source.agent.clone(),
            controller: source.controller.clone(),
            caller: source.caller.clone(),
        };
        let input_digest = arkret::Hash::new(arkret::canonical::canonical_sha256(&(
            &slot,
            &source.accepted,
            &source.text,
            ModelClient::native_reply_instructions(),
            &self.config.model,
            &self.config.model_provider_id,
            &self.config.model_provider.base_url,
            &self.config.model_provider.wire_api,
            &self.config.model_provider.query_params,
            &self.config.model_provider.http_headers,
        ))?)?;
        let (text, submission) = match self
            .ledger
            .native_prepare(slot.clone(), input_digest.clone())
            .await?
        {
            // Keep the durable Unknown slot forever. Settle this input locally,
            // without accepted reply/model rerun, so newer inputs are not starved.
            NativeReplyPreparation::StartedUnknown => return Ok(()),
            NativeReplyPreparation::Accepted { .. }
            | NativeReplyPreparation::SuppressedBeforeBootstrap => return Ok(()),
            NativeReplyPreparation::OutputRetained { text, submission } => (text, submission),
            NativeReplyPreparation::Ready => {
                let defaults = savfox_core::default_client::native_reply_default_headers()?;
                let http=savfox_core::default_client::try_build_native_reply_reqwest_client_with_headers(defaults.clone())?;
                let transport = Arc::new(NativeGuardedTransport::new(
                    HttpClient::new(http.clone()),
                    defaults,
                    Arc::clone(&source),
                    Arc::clone(&self.host),
                    Arc::clone(&self.ledger),
                    slot.clone(),
                    input_digest.clone(),
                ));
                let model = self
                    .config
                    .model
                    .clone()
                    .context("native fixed reply has no configured model")?;
                let auth = AuthManager::shared(
                    self.home.clone(),
                    true,
                    self.config.cli_auth_credentials_store_mode,
                );
                let models = ModelsManager::new(self.home.clone(), Arc::clone(&auth));
                let info = models.get_model_info(&model, &self.config).await;
                let session = savfox_protocol::SessionId::new();
                let session_source = savfox_protocol::protocol::SessionSource::Unknown;
                let otel = savfox_otel::OtelManager::new(
                    session,
                    &model,
                    &info.slug,
                    None,
                    None,
                    None,
                    false,
                    "native_public_reply".into(),
                    session_source.clone(),
                );
                let client = ModelClient::new_with_http_transport(
                    Arc::clone(&self.config),
                    Some(auth),
                    info,
                    otel,
                    self.config.model_provider.clone(),
                    None,
                    self.config.model_reasoning_summary,
                    session,
                    session_source,
                    TransportManager::new(),
                    ReqwestTransport::new(http),
                );
                let mut session = client.new_session(None);
                let mut events = session
                    .stream_native_single_text(
                        &source.text,
                        NativeTransportHandle(Arc::clone(&transport)),
                    )
                    .await?;
                let mut text = String::new();
                let mut completed = false;
                while let Some(event) = events.next().await {
                    match event? {
                        ResponseEvent::OutputTextDelta(delta) => {
                            anyhow::ensure!(
                                text.len() + delta.len() <= 65536,
                                "native output exceeds limit"
                            );
                            text.push_str(&delta);
                        }
                        ResponseEvent::Completed { .. } => {
                            completed = true;
                            break;
                        }
                        // Strict native parser already validates no tools/unknown output.
                        _ => {}
                    }
                }
                anyhow::ensure!(
                    completed && !text.trim().is_empty(),
                    "native text reply did not complete"
                );
                transport.validate_completed_output().await?;
                let text = self
                    .ledger
                    .native_retain_output(transport.take_handoff().await?, text)
                    .await?;
                transport.finish_after_complete().await?;
                (text, None)
            }
        };
        let frozen = if let Some(value) = submission {
            value
        } else {
            let request = MessageCreateRequest {
                scope_ref: source.scope.clone(),
                strand_id: source.strand.to_string(),
                track_name: source
                    .accepted
                    .event
                    .payload
                    .get("track_name")
                    .and_then(Value::as_str)
                    .unwrap_or("discussion")
                    .to_owned(),
                body: text.clone(),
                actor_account_id: source.agent.clone(),
                thread_root_id: None,
                sidecar_exchange: None,
            };
            let mut event = build_message_create_event(&request)?;
            apply_account_outbound_encryption(
                crypto,
                source.accepted.event.realm_id.as_str(),
                &mut event,
                None,
                None,
            )?;
            let authored = savfox_channels::arkret::finalize_outbound_event(event)?;
            let key = account
                .key_ref
                .as_ref()
                .context("native signer key missing")?;
            let method = account
                .verification_method
                .as_deref()
                .context("native signer method missing")?;
            let frozen = garth::MessageAuthoringSession::from_authored_event(authored)?.sign_with(
                |event| {
                    savfox_channels::arkret::sign_outbound_event_with_key(event, key, method)
                        .map_err(|error| garth::Error::Protocol(error.to_string()))
                },
            )?;
            self.ledger
                .native_retain_submission(
                    slot.clone(),
                    input_digest.clone(),
                    arkret::Hash::new(arkret::canonical::sha256_digest(text.as_bytes()))?,
                    frozen,
                )
                .await?
        };
        anyhow::ensure!(
            queued_message_matches_runtime(&frozen.request().submission.event, account, crypto)?,
            "native frozen message binding changed"
        );
        let outbound = OutboundEngine::new(store.clone(), garth::SystemClock);
        let event_id = frozen.request().submission.event.event_id.clone();
        outbound.enqueue(frozen.into_queued_submission()).await?;
        // Only frozen Event retries. Final original UOW rechecks every current gate.
        let provider = construct_account_provider(&self.home, channel, account).await?;
        drain_pending_account_outbound(&provider, store, channel, account, crypto).await;
        let snapshot = outbound.snapshot().await?;
        let item = snapshot
            .items
            .iter()
            .find(|item| item.event_id() == &event_id)
            .context("native frozen submission missing")?;
        anyhow::ensure!(
            item.status == garth::SendQueueStatus::Committed,
            "native Event still queued or refused"
        );
        self.ledger
            .native_mark_accepted(
                slot,
                input_digest,
                item.commit()
                    .context("native accepted outcome lacks Commit")?
                    .clone(),
            )
            .await
    }
}

struct NativeAccountTasks(HashMap<String, (String, tokio::task::JoinHandle<()>)>);
impl Drop for NativeAccountTasks {
    fn drop(&mut self) {
        for (_, (_, task)) in self.0.drain() {
            task.abort();
        }
    }
}

/// Called only by the original explicit configuration Save owner, never by
/// startup, cached delivery, watcher, or missing-file recovery.
fn native_previous_is_explicitly_unbound(
    previous: &savfox_core::config::channel_store::ChannelConfig,
) -> bool {
    previous
        .config
        .get("mode")
        .and_then(serde_json::Value::as_str)
        == Some("agent")
        && [
            "keyRef",
            "verificationMethod",
            "authorizedEventRef",
            "inksonBootstrap",
            "requestedScope",
        ]
        .iter()
        .all(|key| {
            previous
                .config
                .get(*key)
                .is_none_or(serde_json::Value::is_null)
        })
}

pub(crate) async fn native_save_onboarding(
    home: &std::path::Path,
    original: &savfox_core::config::channel_store::ChannelConfig,
    previous: Option<&savfox_core::config::channel_store::ChannelConfig>,
) -> anyhow::Result<()> {
    let parsed = ArkretChannelConfig::from_strict_agent_config(original)?;
    parsed.validate()?;
    let ledger = ArkretExecutionBindingStore::new(home);
    for account in &parsed.accounts {
        let initial_digest = arkret::Hash::new(arkret::canonical::canonical_sha256(&(
            "native_first_configuration_v1",
            &parsed.id,
            &parsed.base_url,
            &parsed.service_id,
            &account.id,
            &account.actor_account_id,
            &account.controller_account_id,
            &account.authorized_event_ref,
            &account.verification_method,
            &account.current_signer_evidence,
            &account.signer_resolution_evidence_ref,
            &account.requested_scope,
        ))?)?;
        // This original provider obtains a real qualified current grant. A
        // JSON key/event label alone cannot initialize an owned runtime.
        let provider = construct_account_provider_for_onboarding(home, &parsed, account).await?;
        let crypto = FileArkretCryptoStore::for_account(home, &parsed.id, &account.id);
        let previous_agent = match previous {
            Some(value) if !native_previous_is_explicitly_unbound(value) => {
                ArkretChannelConfig::from_strict_agent_pairing_config(value)?
                    .accounts
                    .first()
                    .map(|account| account.actor_account_id.clone())
            }
            // The original unbind clears ALL binding fields. This is an
            // explicit configuration transition, not a parsing-error fallback.
            _ => None,
        };
        let fresh_account = previous_agent.as_ref() != Some(&account.actor_account_id);
        if previous.is_some() {
            // Even a legitimate new Agent cannot recover an erased ledger by
            // treating an existing deployment as its first cold start.
            ledger.native_resume_required_existing().await?;
        }
        if fresh_account {
            let account_state = home.join("gateway").join("arkret-account-state").join(
                crypto
                    .path()
                    .file_name()
                    .context("native account scope path missing")?,
            );
            anyhow::ensure!(
                !crypto.path().exists() && !account_state.exists(),
                "native first configuration has previous account/crypto history"
            );
            // Original unbind must clear old ratchet/inbox first. The ledger
            // retains prior accounts and returns an existing FIRST receipt
            // unchanged on A -> B -> A; no cutoff ever resets.
            if previous.is_some()
                && ledger
                    .native_first_configuration_receipt(
                        account.actor_account_id.clone(),
                        account.controller_account_id.clone(),
                    )
                    .await
                    .is_ok()
            {
                // A -> B -> A reuses the original FIRST receipt. It cannot
                // rebind the old account to a new first intent or reset lanes.
            } else {
                // This transaction itself validates existing owner/digest and
                // rejects corrupt/mismatched records. No load error fallback
                // can turn malformed history into a fresh default ledger.
                ledger
                    .native_initialize_first_config(
                        account.actor_account_id.clone(),
                        account.controller_account_id.clone(),
                        initial_digest.clone(),
                    )
                    .await?;
            }
        } else {
            let first = ledger
                .native_first_configuration_receipt(
                    account.actor_account_id.clone(),
                    account.controller_account_id.clone(),
                )
                .await?;
            anyhow::ensure!(
                first.agent == account.actor_account_id
                    && first.controller == account.controller_account_id,
                "native first configuration owner changed"
            );
        }
        // No installed scope is normal immediately after pairing. It confers
        // no model lane. Native maintenance still supplies KeyPackages/Welcome.
        if !crypto.path().exists() {
            continue;
        }
        let store = open_account_store(home, &parsed.id, &account.id, ACCOUNT_EVENT_DEDUPE_MAX)?;
        let service = parsed
            .service_id
            .as_ref()
            .map(|value| arkret::DidCoreId::new(value.clone()))
            .transpose()?;
        let scopes = receive::native_installed_shared_scopes(
            &crypto.load()?,
            store.cursor_scopes()?,
            account,
            &service,
        )?;
        if scopes.is_empty() {
            continue;
        }
        let shared_digest = arkret::Hash::new(arkret::canonical::canonical_sha256(&(
            "native_shared_configuration_v1",
            &account.actor_account_id,
            &account.controller_account_id,
            &parsed.id,
            &parsed.base_url,
            &parsed.service_id,
            &account.authorized_event_ref,
            &account.verification_method,
            &account.current_signer_evidence,
            &account.signer_resolution_evidence_ref,
            &account.requested_scope,
            &scopes,
        ))?)?;
        // Same explicit intent retry returns its original heads BEFORE reading
        // newer live ones. Existing watermarks never reset to a newer snapshot.
        if ledger
            .native_onboarding_receipt(
                account.actor_account_id.clone(),
                account.controller_account_id.clone(),
                shared_digest.clone(),
            )
            .await?
            .is_some()
        {
            continue;
        }
        let client = ArkretHttpClient::from_provider(&provider, &crypto).await?;
        let heads = receive::native_actual_onboarding_heads(&client, &scopes).await?;
        ledger
            .native_initialize_batch(
                account.actor_account_id.clone(),
                account.controller_account_id.clone(),
                shared_digest,
                heads,
            )
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod native_lifetime_tests {
    use super::*;

    struct NotifyDrop(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for NotifyDrop {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    #[tokio::test]
    async fn native_handle_drop_aborts_all_account_children() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let child_handles = Arc::new(std::sync::Mutex::new((false, Vec::new())));
        let supervisor_handles = Arc::clone(&child_handles);
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let supervisor = tokio::spawn(async move {
            let mut children = NativeAccountTasks(HashMap::new());
            let child = tokio::spawn(async move {
                let _drop = NotifyDrop(Some(dropped_tx));
                let _ = started_tx.send(());
                std::future::pending::<()>().await;
            });
            supervisor_handles
                .lock()
                .unwrap()
                .1
                .push(child.abort_handle());
            children
                .0
                .insert("actual-account".into(), ("same-config".into(), child));
            let _ = stop_rx.await;
            drop(children);
            let _ = closed_tx.send(());
        });
        let handle = NativePublicReplyHandle {
            task: supervisor,
            stop: Some(stop_tx),
            children: child_handles,
        };
        started_rx.await.unwrap();
        drop(handle);
        tokio::time::timeout(Duration::from_secs(2), dropped_rx)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), closed_rx)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn native_account_replacement_aborts_previous_consumer() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let mut children = NativeAccountTasks(HashMap::new());
        let child = tokio::spawn(async move {
            let _drop = NotifyDrop(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        children
            .0
            .insert("actual-account".into(), ("old-config".into(), child));
        started_rx.await.unwrap();
        if let Some((_, old)) = children.0.remove("actual-account") {
            old.abort();
        }
        tokio::time::timeout(Duration::from_secs(2), dropped_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(children.0.is_empty());
    }
    #[tokio::test]
    async fn native_invalid_configuration_drops_previous_consumer_before_return() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let mut children = NativeAccountTasks(HashMap::new());
        let child = tokio::spawn(async move {
            let _drop = NotifyDrop(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        children
            .0
            .insert("actual-account".into(), ("old-config".into(), child));
        started_rx.await.unwrap();
        children.abort_and_wait().await;
        assert!(children.0.is_empty());
        dropped_rx.await.unwrap();
    }
    #[test]
    fn native_unbound_previous_is_distinct_from_partial_or_invalid_pairing() {
        let mut previous: savfox_core::config::channel_store::ChannelConfig =
            serde_json::from_value(serde_json::json!({
                "id":"arkret", "name":"Arkret", "enabled":true,
                "config":{"mode":"agent", "keyRef":null,
                  "verificationMethod":null, "authorizedEventRef":null,
                  "inksonBootstrap":null,"requestedScope":null}
            }))
            .unwrap();
        assert!(native_previous_is_explicitly_unbound(&previous));
        previous.config["keyRef"] = serde_json::json!({"kind":"keyring"});
        assert!(!native_previous_is_explicitly_unbound(&previous));
        assert!(ArkretChannelConfig::from_strict_agent_pairing_config(&previous).is_err());
        previous.config["keyRef"] = serde_json::Value::Null;
        previous.config["mode"] = serde_json::json!("other");
        assert!(!native_previous_is_explicitly_unbound(&previous));
    }
    #[tokio::test]
    async fn native_late_consumer_rechecks_disabled_config_before_crypto_creation() {
        let home = tempfile::tempdir().unwrap();
        let home_path = home.path().to_path_buf();
        let disabled: savfox_core::config::channel_store::ChannelConfig =
            serde_json::from_value(serde_json::json!({
                "id":"arkret", "name":"Arkret", "kind":"arkret", "enabled":false,
                "config":{"mode":"agent", "keyRef":null, "inksonBootstrap":null}
            }))
            .unwrap();
        savfox_core::config::channel_store::save_channel_config(&home_path, &disabled)
            .await
            .unwrap();
        let crypto = FileArkretCryptoStore::for_account(&home_path, "arkret", "old-account");
        let owner = crypto.try_acquire_native_owner().unwrap();
        assert!(
            native_require_current_account_config(
                &home_path,
                "arkret",
                "old-account",
                "previous-captured-config"
            )
            .await
            .is_err()
        );
        assert!(!crypto.path().exists());
        drop(owner);
        let erase_lease = crypto.acquire_native_owner_for_unbind().unwrap();
        drop(erase_lease);
    }
}
