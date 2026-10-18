//! Durable Agent creator transactions; Sidecar retains its independent handshake.

use arkret::http_client::own_station_results::OwnStationResultClient;
use arkret_models_collaboration::mls_creator_bootstrap::*;

use super::super::client::ArkretHttpClient;
use super::*;

fn scope_key(scope: &ScopeRef) -> anyhow::Result<String> {
    Ok(serde_json::to_string(scope)?)
}

impl FileArkretCryptoStore {
    pub fn pending_creator_bootstrap_scopes(&self) -> anyhow::Result<Vec<ScopeRef>> {
        let state = self.load()?;
        let mut scopes = Vec::new();
        for key in state.creator_bootstrap_records.keys() {
            let scope: ScopeRef = serde_json::from_str(key)?;
            if let Some(record) = self.creator_bootstrap_record(&scope)? {
                if record.quarantine_diagnostic().is_none()
                    && record.superseded_winner().is_none()
                    && record.rejection().is_none()
                    && record.ready_receipt().is_none()
                {
                    scopes.push(scope);
                }
            }
        }
        Ok(scopes)
    }

    /// Commit the explicit choice and exact signed Create before network effects.
    pub fn begin_creator_bootstrap(&self, intent: MlsCreatorBootstrapIntent) -> anyhow::Result<()> {
        intent.validate()?;
        anyhow::ensure!(
            matches!(
                intent.creator_endpoint(),
                arkret::MlsWelcomeRecipientEndpoint::AgentRuntime { .. }
            ),
            "runtime creator requires its own Agent endpoint"
        );
        let _guard = self.acquire_mutation()?;
        let mut state = self.load_unlocked()?;
        let key = scope_key(intent.effective_scope())?;
        if let Some(raw) = state.creator_bootstrap_records.get(&key) {
            let record: MlsCreatorBootstrapRecord = serde_json::from_value(raw.clone())?;
            record.validate()?;
            anyhow::ensure!(
                record.intent() == &intent,
                "creator scope already has another immutable intent"
            );
            return Ok(());
        }
        state.creator_bootstrap_records.insert(
            key,
            serde_json::to_value(MlsCreatorBootstrapRecord::new(intent)?)?,
        );
        self.save(&mut state)
    }

    pub fn creator_bootstrap_record(
        &self,
        scope: &ScopeRef,
    ) -> anyhow::Result<Option<MlsCreatorBootstrapRecord>> {
        let _guard = self.acquire_mutation()?;
        let mut state = self.load_unlocked()?;
        let key = scope_key(scope)?;
        let Some(raw) = state.creator_bootstrap_records.get(&key).cloned() else {
            return Ok(None);
        };
        let decoded =
            serde_json::from_value::<MlsCreatorBootstrapRecord>(raw.clone()).and_then(|record| {
                record.validate().map_err(serde::de::Error::custom)?;
                Ok(record)
            });
        match decoded {
            Ok(record) => {
                anyhow::ensure!(
                    record.intent().effective_scope() == scope,
                    "creator record is stored under another canonical scope"
                );
                Ok(Some(record))
            }
            Err(error) => {
                let record = MlsCreatorBootstrapRecord::quarantine_authenticated_record(
                    raw,
                    state
                        .creator_bootstrap_queue
                        .iter()
                        .map(|(id, bytes)| serde_json::json!({"event_id":id,"bytes":bytes}))
                        .collect(),
                    MlsCreatorBootstrapInvariant::RecordBinding,
                    error.to_string(),
                    None,
                    arkret::canonical::normalize_timestamp_canonical(Utc::now()),
                )?;
                anyhow::ensure!(
                    record.intent().effective_scope() == scope,
                    "corrupt creator has unknown scope"
                );
                state.creator_bootstrap_ready.remove(&key);
                state
                    .creator_bootstrap_records
                    .insert(key, serde_json::to_value(&record)?);
                self.save(&mut state)?;
                Ok(Some(record))
            }
        }
    }

    fn commit_creator_step(
        &self,
        expected: &MlsCreatorBootstrapRecord,
        next: &MlsCreatorBootstrapRecord,
        client: &OwnStationResultClient,
        install: Option<CurrentMlsGroupStateRecord>,
    ) -> anyhow::Result<bool> {
        self.commit_creator_step_at_cut(expected, next, client, install, None)
    }

    fn commit_creator_step_at_cut(
        &self,
        expected: &MlsCreatorBootstrapRecord,
        next: &MlsCreatorBootstrapRecord,
        client: &OwnStationResultClient,
        install: Option<CurrentMlsGroupStateRecord>,
        snapshot: Option<&arkret::RealmStateSnapshot>,
    ) -> anyhow::Result<bool> {
        next.validate()?;
        anyhow::ensure!(
            expected.intent() == next.intent(),
            "creator step changed immutable intent"
        );
        let _guard = self.acquire_mutation()?;
        client.check_session()?;
        anyhow::ensure!(
            expected.intent().owner_actor_id().as_account_id()
                == Some(client.session()?.account_id()),
            "creator step belongs to a replaced Account"
        );
        let mut state = self.load_unlocked()?;
        let key = scope_key(expected.intent().effective_scope())?;
        if state.creator_bootstrap_records.get(&key) != Some(&serde_json::to_value(expected)?) {
            return Ok(false);
        }
        if let Some(snapshot) = snapshot {
            retain_creator_current(&mut state, next, snapshot)?;
        }
        if let Some(queued) = next.queued_genesis() {
            let id = queued.signed_genesis().event_id();
            let bytes = arkret::canonical::canonical_json_bytes(queued.signed_genesis().event())?;
            if let Some(existing) = state.creator_bootstrap_queue.get(id) {
                anyhow::ensure!(
                    *existing == bytes,
                    "creator queue changed frozen signed bytes"
                );
            }
            state.creator_bootstrap_queue.insert(id.clone(), bytes);
        }
        if let Some(private) = install {
            let accepted = next
                .accepted_genesis()
                .context("creator install has no acceptance")?
                .accepted();
            let group_id = private.group_id.to_string();
            if state.mls_group_states.contains_key(&group_id) {
                require_installed_creator_private(&state, next, &private)?;
            } else {
                state.mls_group_states.insert(group_id.clone(), private);
                state
                    .mls_accepted_bases
                    .insert(group_id.clone(), accepted.clone());
                state.bootstrap.insert(
                    group_id.clone(),
                    ArkretBootstrapRecord {
                        group_id,
                        required_epoch: 0,
                        local_epoch: Some(0),
                        group_state_ref: Some(accepted.event.event_id.to_string()),
                        action: MlsRecoveryAction::UseLocalState,
                        updated_at: Utc::now(),
                    },
                );
            }
            if let ScopeRef::Realm { realm_id } = expected.intent().effective_scope() {
                state.realm_policies.insert(
                    realm_id.to_string(),
                    ArkretRealmCryptoPolicy {
                        realm_id: realm_id.to_string(),
                        content_encryption_floor: ArkretContentEncryptionFloor::E2eeRequired,
                        encryption_profile: Some("mls_rfc9420".into()),
                        mls_group_id: Some(expected.intent().mls_group_id().to_string()),
                        source: "accepted_mls_genesis".into(),
                        updated_at: Utc::now(),
                    },
                );
            }
        }
        state.creator_bootstrap_ready.remove(&key);
        if next.quarantine_diagnostic().is_some()
            || next.superseded_winner().is_some()
            || next.rejection().is_some()
        {
            if let Some(queued) = expected.queued_genesis() {
                // Quarantine retains the queue original in its diagnostic, never retries it.
                state
                    .creator_bootstrap_queue
                    .remove(queued.signed_genesis().event_id());
            }
        }
        state
            .creator_bootstrap_records
            .insert(key, serde_json::to_value(next)?);
        client.check_session()?;
        self.save(&mut state)?;
        Ok(true)
    }

    fn quarantine_creator(
        &self,
        record: &MlsCreatorBootstrapRecord,
        client: &OwnStationResultClient,
        detail: String,
    ) -> anyhow::Result<()> {
        self.quarantine_creator_invariant(
            record,
            client,
            MlsCreatorBootstrapInvariant::PrivateMaterial,
            detail,
            None,
        )
    }

    fn quarantine_creator_invariant(
        &self,
        record: &MlsCreatorBootstrapRecord,
        client: &OwnStationResultClient,
        invariant: MlsCreatorBootstrapInvariant,
        detail: String,
        known: Option<MlsCreatorBootstrapKnownGenesis>,
    ) -> anyhow::Result<()> {
        let state = self.load()?;
        let queue = record
            .queued_genesis()
            .and_then(|queued| {
                state
                    .creator_bootstrap_queue
                    .get(queued.signed_genesis().event_id())
            })
            .map(serde_json::to_value)
            .transpose()?
            .into_iter()
            .collect();
        let next = MlsCreatorBootstrapRecord::quarantine_authenticated_record(
            serde_json::to_value(record)?,
            queue,
            invariant,
            detail,
            known,
            arkret::canonical::normalize_timestamp_canonical(Utc::now()),
        )?;
        self.commit_creator_step(record, &next, client, None)?;
        Ok(())
    }

    fn restore_creator_group(
        &self,
        record: &MlsCreatorBootstrapRecord,
    ) -> anyhow::Result<(
        CurrentMlsGroupStateRecord,
        MlsCreatorBootstrapArtifactChecks,
    )> {
        record.validate()?;
        let unit = record
            .epoch_zero()
            .context("creator has no private recovery unit")?;
        let wrapped: WrappedCryptoStateFile =
            serde_json::from_slice(unit.encrypted_private_state())?;
        let private = zeroize::Zeroizing::new(self.decrypt_wrapped_state(&wrapped)?);
        let private: CurrentMlsGroupStateRecord = serde_json::from_slice(&private)?;
        let group = ArkretMlsGroup::restore_from_state_record(&private)?;
        let checks = creator_checks(&group, record.intent())?;
        let payload = unit.payload()?;
        anyhow::ensure!(
            checks.group_info_bytes == unit.group_info_bytes()
                && checks.ratchet_tree_bytes == unit.ratchet_tree_bytes()
                && checks.creator_leaf_authority == payload.creator_leaf_authority
                && checks.cipher_suite == payload.cipher_suite.as_str(),
            "creator restored private state differs from frozen public material"
        );
        Ok((private, checks))
    }

    fn publish_creator_ready(
        &self,
        expected: &MlsCreatorBootstrapRecord,
        client: &OwnStationResultClient,
    ) -> anyhow::Result<bool> {
        let _guard = self.acquire_mutation()?;
        client.check_session()?;
        let mut state = self.load_unlocked()?;
        let key = scope_key(expected.intent().effective_scope())?;
        if state.creator_bootstrap_records.get(&key) != Some(&serde_json::to_value(expected)?) {
            return Ok(false);
        }
        let (private, checks) = self.restore_creator_group(expected)?;
        anyhow::ensure!(
            expected.artifacts() == Some(&MlsCreatorBootstrapArtifacts::new(expected, checks)?),
            "creator durable artifact mismatch"
        );
        require_installed_creator_private(&state, expected, &private)?;
        let receipt = MlsCreatorBootstrapReadyReceipt::new(
            expected,
            state
                .generation
                .checked_add(1)
                .context("creator commit position exhausted")?,
        )?;
        let mut next = expected.clone();
        next.publish_ready(receipt.clone())?;
        state
            .creator_bootstrap_records
            .insert(key.clone(), serde_json::to_value(next)?);
        state.creator_bootstrap_ready.insert(key, receipt);
        client.check_session()?;
        self.save(&mut state)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use arkret_models_collaboration::authority_commit::SelfAuthoritySubmitRequest;

    use super::*;

    fn intent(circle: bool) -> MlsCreatorBootstrapIntent {
        let actor = ActorId::account(AccountId::new(
            DidCoreId::new("ak:did_core:web:creator.example").unwrap(),
            DidCoreId::new("ak:did_core:web:station.example").unwrap(),
        ));
        let payload = if circle {
            serde_json::json!({})
        } else {
            let origin = arkret::RealmGenesis::new(
                arkret::RealmPurpose::Collaboration,
                arkret::GenesisSalt::new(arkret::base64url_encode([5; 32])).unwrap(),
                arkret::TrustDomainId::new("ak:trust_domain:example.net").unwrap(),
                arkret::SecurityClass::Standard,
                DidCoreId::new("ak:did_core:web:station.example").unwrap(),
                arkret::JoinRule::Closed,
                arkret::HistoryAccess::SinceJoin,
                arkret::Discoverability::InviteOnly,
                None,
                None,
            )
            .unwrap();
            serde_json::to_value(arkret::RealmCreatePayload::new(origin)).unwrap()
        };
        let parent =
            RealmId::from_event_id(&EventId::from_digest(arkret::DigestSuite::Sha256, [4; 32]));
        let event = arkret_wire::test_support::raw_event_for_actor_at(
            if circle {
                "ak.circle.create"
            } else {
                "ak.realm.create"
            },
            if circle {
                ScopeRef::Realm {
                    realm_id: parent.clone(),
                }
            } else {
                ScopeRef::RealmGenesis
            },
            actor.clone(),
            payload,
            arkret::canonical::normalize_timestamp_canonical(Utc::now()),
        )
        .unwrap();
        let mut authored =
            arkret::AuthoredEvent::finalize_with_digest_suite(event, arkret::DigestSuite::Sha256)
                .unwrap();
        let method = arkret::DidUrl::new("did:web:creator.example#runtime").unwrap();
        let signer = arkret::signatures::Ed25519DetachedJwsSigner::new(
            ed25519_dalek::SigningKey::from_bytes(&[11; 32]),
            method.as_str(),
        );
        super::super::super::outbound::sign_outbound_event(&mut authored, &signer).unwrap();
        let scope = if circle {
            ScopeRef::Circle {
                realm_id: parent,
                circle_id: arkret::CircleId::from_event_id(authored.event_id()),
            }
        } else {
            ScopeRef::Realm {
                realm_id: authored.realm_id.clone(),
            }
        };
        MlsCreatorBootstrapIntent::new_with_endpoint(
            actor,
            scope.clone(),
            DeviceId::new("ak:device:0198ff00-0000-7000-8000-000000000001").unwrap(),
            method.clone(),
            arkret::MlsWelcomeRecipientEndpoint::AgentRuntime {
                verification_method: method,
            },
            arkret::MlsGovernanceBindingPayload::new(scope, None, 0, 0, 0).unwrap(),
            SelfAuthoritySubmitRequest::Event(arkret::EventAdmissionSubmission::new(
                authored.event().clone(),
            )),
        )
        .unwrap()
    }

    struct LocalSession(
        std::sync::Mutex<arkret::http_client::own_station_results::OwnStationSessionSnapshot>,
    );
    impl arkret::http_client::own_station_results::OwnStationSessionSource for LocalSession {
        fn snapshot(
            &self,
        ) -> arkret::http_client::Result<
            arkret::http_client::own_station_results::OwnStationSessionSnapshot,
        > {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    fn local_session(intent: &MlsCreatorBootstrapIntent) -> OwnStationResultClient {
        let account = intent.owner_actor_id().as_account_id().unwrap().clone();
        let binding = arkret::StationConnectionBinding {
            service_id: account.station_id.clone(),
            base_url: "https://station.example/".into(),
            trust_domain: arkret::TrustDomainId::new("ak:trust_domain:example.net").unwrap(),
            auth_metadata: arkret::AuthMetadata::minimal(),
        };
        let snapshot = arkret::http_client::own_station_results::OwnStationSessionSnapshot::new(
            binding.clone(),
            account.clone(),
            account.station_id,
            arkret_wire::SessionGrantId::from_issuance_digest([1; 32]),
            1,
            "local-transaction-fixture".into(),
        )
        .unwrap();
        let raw =
            arkret::http_client::ClientBuilder::new(url::Url::parse(&binding.base_url).unwrap())
                .auth(arkret::http_client::Auth::Bearer(
                    "local-transaction-fixture".into(),
                ))
                .build()
                .unwrap();
        OwnStationResultClient::new(
            raw,
            binding,
            Arc::new(LocalSession(std::sync::Mutex::new(snapshot))),
        )
        .unwrap()
    }

    fn local_acceptance(
        intent: &MlsCreatorBootstrapIntent,
    ) -> (
        MlsCreatorBootstrapAcceptedCreate,
        arkret::RealmStateSnapshot,
        MlsCreatorBootstrapGovernanceEvidence,
    ) {
        // Synthetic host-verified inputs isolate local CAS/RFC recovery. These
        // detached signatures do not claim an actual Station accepted anything.
        let SelfAuthoritySubmitRequest::Event(create) = intent.signed_scope_create_unit() else {
            unreachable!()
        };
        let now = create.event.created_at;
        let signature = |context| arkret::DetachedObjectSignature {
            context,
            signature_algorithm: arkret::DetachedSignatureAlgorithm::Ed25519,
            verification_method: arkret::DidUrl::new("did:web:station.example#key").unwrap(),
            signed_digest: arkret::Hash::new(arkret::canonical::sha256_digest(b"local-fixture"))
                .unwrap(),
            created_at: now,
            sig: arkret::Base64UrlString::new("AA").unwrap(),
        };
        let commit = arkret::RealmCommit {
            commit_id: arkret::RealmCommitId::from_digest([1; 32]),
            realm_id: create.event.realm_id.clone(),
            stream_ref: arkret::CommitStreamRef::Realm {
                realm_id: create.event.realm_id.clone(),
            },
            stream_position: 0,
            previous_commit_ref: None,
            event_ref: create.event.event_id.clone(),
            governance_generation: 0,
            authority_ref: arkret::RealmCommitAuthorityRef::GenesisOrChangeEvent(
                create.event.event_id.clone(),
            ),
            committed_at: now,
            producer_signer_fact_digest: None,
            signature: signature(arkret::DetachedSignatureContext::RealmCommit),
        };
        let head = arkret::CommitStreamHead {
            stream_ref: commit.stream_ref.clone(),
            stream_position: 0,
            commit_id: commit.commit_id.clone(),
        };
        let root = arkret::RealmAuthorityBundle {
            realm_id: commit.realm_id.clone(),
            genesis_event: create.event.clone(),
            genesis_commit: commit.clone(),
            authority_transitions: Vec::new(),
            current_generation: 0,
            current_service_id: intent
                .owner_actor_id()
                .as_account_id()
                .unwrap()
                .station_id
                .clone(),
            current_route_record: serde_json::json!({}),
            realm_stream_head: head.clone(),
            bundle_issued_at: now,
            current_assertion: arkret::RealmAuthorityCurrentAssertion {
                realm_id: commit.realm_id.clone(),
                current_generation: 0,
                current_service_id: intent
                    .owner_actor_id()
                    .as_account_id()
                    .unwrap()
                    .station_id
                    .clone(),
                last_handoff_ref: None,
                realm_stream_head: head.clone(),
                nonce: arkret::Base64UrlString::new(arkret::base64url_encode([0; 32])).unwrap(),
                expires_at: now + chrono::TimeDelta::minutes(5),
                signature: signature(
                    arkret::DetachedSignatureContext::RealmAuthorityCurrentAssertion,
                ),
            },
        };
        let accepted = MlsCreatorBootstrapAcceptedCreate::new(
            intent,
            create.event.clone(),
            commit.clone(),
            arkret::DigestSuite::Sha256,
            root,
        )
        .unwrap();
        let absence = arkret::RealmStateSnapshot {
            snapshot_id: arkret::RealmSnapshotId::from_digest([3; 32]),
            realm_id: commit.realm_id.clone(),
            governance_generation: 0,
            visible_stream_heads: vec![head],
            current_state_entries: Vec::new(),
            retention_and_history_floor: arkret::RetentionAndHistoryFloor {
                history_access: arkret::HistoryAccess::SinceJoin,
                stream_floors: vec![arkret::StreamHistoryFloor {
                    stream_ref: commit.stream_ref.clone(),
                    oldest_position: 0,
                }],
            },
            created_at: now,
            signature: signature(arkret::DetachedSignatureContext::RealmSnapshot),
        };
        let authority = MlsCreatorBootstrapAgentAuthority::from_current_signer_query(
            intent,
            arkret::SignerKeyQueryOutcome::CurrentResolved {
                selector: arkret::SignerKeyQuerySelector::CurrentAdmission {
                    sender: arkret::CurrentSignerKeyQuerySender::Agent {
                        actor: intent.owner_actor_id().clone(),
                        verification_method: intent.creator_signer_method().clone(),
                    },
                },
                key: arkret::ResolvedSignerKey {
                    public_key_b64u: arkret::Base64UrlString::new(arkret::base64url_encode(
                        ed25519_dalek::SigningKey::from_bytes(&[11; 32])
                            .verifying_key()
                            .to_bytes(),
                    ))
                    .unwrap(),
                    authorization_ref: arkret::CommittedEventRef {
                        event_id: create.event.event_id.clone(),
                        commit_id: commit.commit_id.clone(),
                        stream_ref: commit.stream_ref,
                        stream_position: 0,
                    },
                    revision: arkret::CurrentRevision {
                        commit_id: commit.commit_id,
                        stream_position: 0,
                    },
                    governance_generation: 0,
                },
            },
            now,
        )
        .unwrap();
        let evidence = MlsCreatorBootstrapGovernanceEvidence::new_with_endpoint_authority(
            intent,
            accepted.clone(),
            absence.clone(),
            MlsCreatorBootstrapEndpointAuthority::Agent(authority),
        )
        .unwrap();
        (accepted, absence, evidence)
    }

    #[test]
    fn runtime_creator_atomic_steps_reopen_original_bytes_and_publish_one_ready_cut() {
        let home = tempfile::tempdir().unwrap();
        let store = FileArkretCryptoStore::for_account(home.path(), "creator-cas", "runtime");
        let intent = intent(false);
        let client = local_session(&intent);
        let (create, absence, evidence) = local_acceptance(&intent);
        store.begin_creator_bootstrap(intent.clone()).unwrap();
        let read = || {
            FileArkretCryptoStore::for_account(home.path(), "creator-cas", "runtime")
                .creator_bootstrap_record(intent.effective_scope())
                .unwrap()
                .unwrap()
        };
        let first = read();
        let mut next = first.clone();
        next.accept_realm(create.clone(), absence.clone()).unwrap();
        assert!(
            store
                .commit_creator_step(&first, &next, &client, None)
                .unwrap()
        );
        let accepted = read();
        next.pin_governance(evidence.clone()).unwrap();
        assert!(
            store
                .commit_creator_step(&accepted, &next, &client, None)
                .unwrap()
        );
        assert!(
            !store
                .commit_creator_step(&first, &accepted, &client, None)
                .unwrap()
        );
        let pinned = read();
        let key = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
        let identity = ArkretMlsIdentity::new_agent(
            intent.owner_actor_id().clone(),
            intent.creator_signer_method().clone(),
            evidence
                .creator_authority()
                .authorization_event_ref()
                .clone(),
            ArkretMlsSigner::from_ed25519_signing_key(key.clone()),
        )
        .unwrap();
        let mut group = identity
            .create_group_with_governance_binding(
                intent.effective_scope(),
                evidence.governance_binding(),
            )
            .unwrap();
        group
            .install_local_creator_binding(intent.owner_actor_id().clone(), None)
            .unwrap();
        let checks = creator_checks(&group, &intent).unwrap();
        let payload = arkret::MlsGenesisPayload {
            cipher_suite: arkret::NonEmptyString::new(checks.cipher_suite.clone()).unwrap(),
            group_info_ref: arkret::BlobRef::new(format!(
                "ak:blob:{}",
                arkret::canonical::sha256_digest(&checks.group_info_bytes)
            ))
            .unwrap(),
            ratchet_tree_ref: arkret::BlobRef::new(format!(
                "ak:blob:{}",
                arkret::canonical::sha256_digest(&checks.ratchet_tree_bytes)
            ))
            .unwrap(),
            creator_leaf_authority: checks.creator_leaf_authority.clone(),
            governance_binding: evidence.governance_binding().clone(),
            created_at: create.accepted_event().created_at,
        };
        let unsigned = arkret::TypedEventDraft::<arkret::event_spec::MlsGenesis>::new(
            intent.effective_scope().clone(),
            intent.owner_actor_id().clone(),
            payload,
        )
        .unwrap()
        .into_intent(create.accepted_event().created_at)
        .unwrap()
        .author_with_digest_suite(arkret::DigestSuite::Sha256)
        .unwrap();
        let private = serde_json::to_vec(&group.export_state_record().unwrap()).unwrap();
        let wrapped = serde_json::to_vec(&store.encrypt_wrapped_state(&private).unwrap()).unwrap();
        let unit = MlsCreatorBootstrapEpochZero::new(
            &intent,
            &evidence,
            wrapped,
            checks.group_info_bytes,
            checks.ratchet_tree_bytes,
            unsigned.clone(),
        )
        .unwrap();
        next.persist_epoch_zero(unit).unwrap();
        assert!(
            store
                .commit_creator_step(&pinned, &next, &client, None)
                .unwrap()
        );
        let epoch0 = read();
        let mut signed = unsigned;
        let signer = arkret::signatures::Ed25519DetachedJwsSigner::new(
            key.clone(),
            intent.creator_signer_method().as_str(),
        );
        super::super::super::outbound::sign_outbound_event(&mut signed, &signer).unwrap();
        next.queue_genesis(signed.clone()).unwrap();
        assert!(
            store
                .commit_creator_step(&epoch0, &next, &client, None)
                .unwrap()
        );
        let queued = read();
        verify_frozen_creator_producer(&queued, &key).unwrap();
        let original = arkret::canonical::canonical_json_bytes(signed.event()).unwrap();
        assert_eq!(
            store
                .load()
                .unwrap()
                .creator_bootstrap_queue
                .get(signed.event_id()),
            Some(&original)
        );
        let mut commit = create.covering_commit().clone();
        commit.stream_position += 1;
        commit.previous_commit_ref = Some(commit.commit_id.clone());
        commit.event_ref = signed.event_id().clone();
        next.accept_genesis(
            MlsCreatorBootstrapAcceptedGenesis::new(
                &intent,
                queued.queued_genesis().unwrap(),
                arkret::CommittedEventFullView {
                    event: signed.event().clone(),
                    commit,
                },
                create.authority_root().unwrap().clone(),
            )
            .unwrap(),
        )
        .unwrap();
        let current = arkret::MlsGroupCurrent {
            effective_scope: intent.effective_scope().clone(),
            genesis_event_ref: signed.event_id().clone(),
            cipher_suite: arkret::NonEmptyString::new(
                group.group_ciphersuite_canonical_id().unwrap(),
            )
            .unwrap(),
            current_mls_commit_event_ref: signed.event_id().clone(),
            epoch: 0,
            current_key_access_revision: 0,
            covered_key_access_revision: 0,
            public_tree_ref: arkret::BlobRef::new(format!(
                "ak:blob:{}",
                arkret::canonical::sha256_digest(
                    &queued.epoch_zero().unwrap().ratchet_tree_bytes()
                )
            ))
            .unwrap(),
        };
        let mut accepted_cut = absence;
        accepted_cut
            .current_state_entries
            .push(TypedCurrentRow::Value {
                selector: arkret::CurrentSelector::MlsGroup {
                    scope_ref: intent.effective_scope().clone(),
                },
                source_stream_ref: arkret::CommitStreamRef::from_scope(
                    intent.effective_scope(),
                    None,
                )
                .unwrap(),
                revision: arkret::CurrentRevision {
                    commit_id: next
                        .accepted_genesis()
                        .unwrap()
                        .accepted()
                        .commit
                        .commit_id
                        .clone(),
                    stream_position: next
                        .accepted_genesis()
                        .unwrap()
                        .accepted()
                        .commit
                        .stream_position,
                },
                value: serde_json::to_value(&current).unwrap(),
            });
        assert!(
            store
                .commit_creator_step_at_cut(&queued, &next, &client, None, Some(&accepted_cut))
                .unwrap()
        );
        assert_eq!(
            store.load().unwrap().mls_station_currents[intent.mls_group_id().as_str()][&0],
            current
        );
        let accepted = read();
        let (private, checks) = store.restore_creator_group(&accepted).unwrap();
        next.converge_artifacts(MlsCreatorBootstrapArtifacts::new(&accepted, checks).unwrap())
            .unwrap();
        assert!(
            store
                .commit_creator_step(&accepted, &next, &client, Some(private))
                .unwrap()
        );
        let converged = read();
        assert!(
            store
                .require_creator_ready(intent.effective_scope())
                .is_err()
        );
        assert!(store.publish_creator_ready(&converged, &client).unwrap());
        let ready = read();
        assert_eq!(
            ready.ready_receipt().unwrap().ready_commit_position(),
            store.load().unwrap().generation
        );
        store
            .require_creator_ready(intent.effective_scope())
            .unwrap();
        let before = std::fs::read(store.path()).unwrap();
        assert!(!store.publish_creator_ready(&converged, &client).unwrap());
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
        assert!(matches!(
            store
                .encrypt_content_block_for_scope(
                    intent.effective_scope(),
                    &serde_json::to_value(ContentBlock::text("actual encrypted write")).unwrap()
                )
                .unwrap(),
            ArkretEncryptOutcome::Encrypted(_)
        ));
        store
            .require_creator_ready(intent.effective_scope())
            .unwrap();
        assert_eq!(read(), ready);
        let healthy = store.load().unwrap();
        for failure in 0..4 {
            let mut state = healthy.clone();
            let mut invalid = current.clone();
            match failure {
                0 => invalid.current_key_access_revision = 1,
                1 => invalid.epoch = 1,
                2 => {
                    invalid.public_tree_ref = arkret::BlobRef::new(format!(
                        "ak:blob:{}",
                        arkret::canonical::sha256_digest(b"another tree")
                    ))
                    .unwrap()
                }
                _ => {
                    invalid.current_mls_commit_event_ref = create.accepted_event().event_id.clone()
                }
            }
            state
                .mls_station_currents
                .get_mut(intent.mls_group_id().as_str())
                .unwrap()
                .insert(invalid.epoch, invalid);
            // Represents a later accepted current arriving through the native reader.
            // Ready receipt alone cannot authorize the now stale private state.
            let mut live = store.load().unwrap();
            live.mls_station_currents = state.mls_station_currents;
            store.save(&mut live).unwrap();
            let before = std::fs::read(store.path()).unwrap();
            assert!(
                store
                    .require_creator_ready(intent.effective_scope())
                    .is_err(),
                "failure {failure}"
            );
            assert!(
                store
                    .encrypt_content_block_for_scope(
                        intent.effective_scope(),
                        &serde_json::to_value(ContentBlock::text("must stay closed")).unwrap()
                    )
                    .is_err()
            );
            assert_eq!(std::fs::read(store.path()).unwrap(), before);
        }
    }

    #[test]
    fn runtime_creator_reopens_the_same_intent_and_blocks_plaintext_in_both_scopes() {
        for circle in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let store = FileArkretCryptoStore::for_account(home.path(), "creator", "runtime");
            let intent = intent(circle);
            store.begin_creator_bootstrap(intent.clone()).unwrap();
            let before = std::fs::read(store.path()).unwrap();
            store.begin_creator_bootstrap(intent.clone()).unwrap();
            assert_eq!(std::fs::read(store.path()).unwrap(), before);
            let reopened = FileArkretCryptoStore::for_account(home.path(), "creator", "runtime");
            let record = reopened
                .creator_bootstrap_record(intent.effective_scope())
                .unwrap()
                .unwrap();
            assert_eq!(record.intent(), &intent);
            assert!(
                reopened
                    .require_creator_ready(intent.effective_scope())
                    .is_err()
            );
            assert!(
                reopened
                    .encrypt_content_block_for_scope(
                        intent.effective_scope(),
                        &serde_json::to_value(ContentBlock::text("private")).unwrap()
                    )
                    .is_err()
            );
            assert_eq!(
                reopened.pending_creator_bootstrap_scopes().unwrap(),
                vec![intent.effective_scope().clone()]
            );
            let mut other = serde_json::to_value(&intent).unwrap();
            other["creator_device_id"] =
                serde_json::json!("ak:device:0198ff00-0000-7000-8000-000000000002");
            let other = serde_json::from_value(other).unwrap();
            assert!(reopened.begin_creator_bootstrap(other).is_err());
            assert_eq!(std::fs::read(store.path()).unwrap(), before);
        }
    }

    #[test]
    fn runtime_creator_restores_real_agent_private_state_and_binds_public_scope_and_leaf() {
        for circle in [false, true] {
            let intent = intent(circle);
            let identity = ArkretMlsIdentity::new_agent(
                intent.owner_actor_id().clone(),
                intent.creator_signer_method().clone(),
                intent.scope_create_event_id().clone(),
                ArkretMlsSigner::from_ed25519_signing_key(ed25519_dalek::SigningKey::from_bytes(
                    &[11; 32],
                )),
            )
            .unwrap();
            let mut group = identity
                .create_group_with_governance_binding(
                    intent.effective_scope(),
                    intent.proposal().proposed_group_genesis_binding(),
                )
                .unwrap();
            group
                .install_local_creator_binding(intent.owner_actor_id().clone(), None)
                .unwrap();
            let before = creator_checks(&group, &intent).unwrap();
            let home = tempfile::tempdir().unwrap();
            let store =
                FileArkretCryptoStore::for_account(home.path(), "creator-private", "runtime");
            let bytes = serde_json::to_vec(&group.export_state_record().unwrap()).unwrap();
            let protected = store.encrypt_wrapped_state(&bytes).unwrap();
            let reopened =
                FileArkretCryptoStore::for_account(home.path(), "creator-private", "runtime");
            let record: CurrentMlsGroupStateRecord =
                serde_json::from_slice(&reopened.decrypt_wrapped_state(&protected).unwrap())
                    .unwrap();
            let restored = ArkretMlsGroup::restore_from_state_record(&record).unwrap();
            assert_eq!(creator_checks(&restored, &intent).unwrap(), before);
            assert_eq!(
                before.creator_leaf_authority.endpoint,
                *intent.creator_endpoint()
            );
            let mut tampered = protected;
            tampered.ciphertext.push('A');
            assert!(reopened.decrypt_wrapped_state(&tampered).is_err());
        }
    }

    #[test]
    fn runtime_creator_quarantines_authenticated_damage_without_erasing_the_original() {
        let home = tempfile::tempdir().unwrap();
        let store = FileArkretCryptoStore::for_account(home.path(), "creator-damage", "runtime");
        let intent = intent(false);
        store.begin_creator_bootstrap(intent.clone()).unwrap();
        let mut state = store.load().unwrap();
        let key = scope_key(intent.effective_scope()).unwrap();
        state.creator_bootstrap_records.get_mut(&key).unwrap()["unexpected"] =
            serde_json::json!("retained-diagnostic");
        store.save(&mut state).unwrap();
        let record = store
            .creator_bootstrap_record(intent.effective_scope())
            .unwrap()
            .unwrap();
        assert_eq!(
            record.quarantine_diagnostic().unwrap().recovery_record()["unexpected"],
            "retained-diagnostic"
        );
        assert!(store.pending_creator_bootstrap_scopes().unwrap().is_empty());
        assert!(
            store
                .require_creator_ready(intent.effective_scope())
                .is_err()
        );
        let before = std::fs::read(store.path()).unwrap();
        assert_eq!(
            store
                .creator_bootstrap_record(intent.effective_scope())
                .unwrap()
                .unwrap(),
            record
        );
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
    }

    #[test]
    fn runtime_creator_does_not_reconstruct_material_from_an_empty_existing_vault() {
        let home = tempfile::tempdir().unwrap();
        let store = FileArkretCryptoStore::for_account(home.path(), "creator-empty", "runtime");
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), []).unwrap();
        assert!(store.begin_creator_bootstrap(intent(false)).is_err());
        assert!(std::fs::read(store.path()).unwrap().is_empty());
    }
}

// The accepted Genesis and the latest current at that same verified cut are
// one local commit. A later native current must never be rolled back by a retry.
fn retain_creator_current(
    state: &mut ArkretCryptoStateFile,
    record: &MlsCreatorBootstrapRecord,
    snapshot: &arkret::RealmStateSnapshot,
) -> anyhow::Result<()> {
    let scope = record.intent().effective_scope();
    let genesis = record
        .accepted_genesis()
        .map(|accepted| &accepted.accepted().event.event_id)
        .or_else(|| {
            record
                .superseded_winner()
                .map(|winner| &winner.accepted().event.event_id)
        })
        .context("creator accepted cut has no authenticated Genesis")?;
    let mut matching = snapshot
        .current_state_entries
        .iter()
        .filter_map(|row| match row {
            TypedCurrentRow::Value {
                selector: arkret::CurrentSelector::MlsGroup { scope_ref },
                source_stream_ref,
                value,
                ..
            } if scope_ref == scope => Some((source_stream_ref, value)),
            _ => None,
        });
    let (stream, value) = matching
        .next()
        .context("creator accepted cut omits MLS current")?;
    anyhow::ensure!(matching.next().is_none(), "creator cut repeats MLS current");
    anyhow::ensure!(
        stream == &arkret::CommitStreamRef::from_scope(scope, None)?,
        "creator current has another source stream"
    );
    let current: arkret::MlsGroupCurrent = serde_json::from_value(value.clone())?;
    anyhow::ensure!(
        current.effective_scope == *scope && current.genesis_event_ref == *genesis,
        "creator current differs from its accepted scope or Genesis"
    );
    let epochs = state
        .mls_station_currents
        .entry(record.intent().mls_group_id().to_string())
        .or_default();
    for previous in epochs.values() {
        anyhow::ensure!(
            previous.genesis_event_ref == *genesis,
            "creator accepted cut conflicts with retained Genesis"
        );
    }
    if let Some(previous) = epochs.get(&current.epoch) {
        anyhow::ensure!(
            previous.current_mls_commit_event_ref == current.current_mls_commit_event_ref
                && previous.public_tree_ref == current.public_tree_ref,
            "creator cut conflicts with retained epoch Commit or tree"
        );
        if previous.current_key_access_revision > current.current_key_access_revision
            || (previous.current_key_access_revision == current.current_key_access_revision
                && previous.covered_key_access_revision > current.covered_key_access_revision)
        {
            return Ok(());
        }
    }
    epochs.insert(current.epoch, current);
    Ok(())
}

fn require_creator_current_private(
    state: &ArkretCryptoStateFile,
    record: &MlsCreatorBootstrapRecord,
    group: &ArkretMlsGroup,
    current: &arkret::MlsGroupCurrent,
    genesis: &arkret::EventId,
) -> anyhow::Result<()> {
    let (_, tree) = group.public_group_state_bytes()?;
    let digest = arkret_models_collaboration::mls_group_state_material::material_digest_from_ref(
        &current.public_tree_ref,
    )?;
    let expected_tree = format!(
        "ak:blob:{}",
        arkret::canonical::digest(digest.digest_suite()?, &tree)
    );
    anyhow::ensure!(
        current.genesis_event_ref == *genesis
            && current.effective_scope == *record.intent().effective_scope()
            && group.scope() == record.intent().effective_scope()
            && group.group_id() == *record.intent().mls_group_id()
            && group.epoch() == current.epoch
            && group.local_actor_id() == record.intent().owner_actor_id()
            && group.group_ciphersuite_canonical_id()? == current.cipher_suite.as_str()
            && current.current_key_access_revision == current.covered_key_access_revision
            && current.public_tree_ref.as_str() == expected_tree
            && group_state_ref_for_epoch(
                state,
                record.intent().mls_group_id().as_str(),
                group.epoch()
            )
            .as_deref()
                == Some(current.current_mls_commit_event_ref.as_str()),
        "creator private state does not match the latest accepted current; native recovery required"
    );
    Ok(())
}

fn require_installed_creator_private(
    state: &ArkretCryptoStateFile,
    record: &MlsCreatorBootstrapRecord,
    original: &CurrentMlsGroupStateRecord,
) -> anyhow::Result<()> {
    let installed = state
        .mls_group_states
        .get(original.group_id.as_str())
        .context("creator private install missing")?;
    let group = ArkretMlsGroup::restore_from_state_record(installed)?;
    anyhow::ensure!(
        installed.actor_id == original.actor_id
            && installed.endpoint == original.endpoint
            && group.scope() == record.intent().effective_scope(),
        "creator private install changed its original identity"
    );
    let accepted = record
        .accepted_genesis()
        .context("creator private install has no acceptance")?
        .accepted();
    if let Some(current) = state
        .mls_station_currents
        .get(original.group_id.as_str())
        .and_then(|epochs| epochs.last_key_value())
        .map(|(_, current)| current)
    {
        require_creator_current_private(state, record, &group, current, &accepted.event.event_id)?;
    }
    if installed.epoch == 0 {
        let checks = creator_checks(&group, record.intent())?;
        let unit = record
            .epoch_zero()
            .context("creator install lost original unit")?;
        anyhow::ensure!(
            checks.group_info_bytes == unit.group_info_bytes()
                && checks.ratchet_tree_bytes == unit.ratchet_tree_bytes()
                && state.mls_accepted_bases.get(original.group_id.as_str()) == Some(accepted),
            "creator installed epoch zero differs from the accepted original"
        );
    } else {
        let current = state
            .mls_station_currents
            .get(original.group_id.as_str())
            .and_then(|epochs| epochs.get(&installed.epoch))
            .context("advanced creator private state has no accepted current")?;
        anyhow::ensure!(
            current.genesis_event_ref == accepted.event.event_id
                && group_state_ref_for_epoch(state, original.group_id.as_str(), installed.epoch)
                    .as_deref()
                    == Some(current.current_mls_commit_event_ref.as_str()),
            "advanced creator private state does not descend from its exact accepted Genesis"
        );
    }
    Ok(())
}

fn creator_checks(
    group: &ArkretMlsGroup,
    intent: &MlsCreatorBootstrapIntent,
) -> anyhow::Result<MlsCreatorBootstrapArtifactChecks> {
    let bindings = group.verified_leaf_bindings()?;
    let [leaf] = bindings.as_slice() else {
        anyhow::bail!("creator epoch zero must have exactly one leaf");
    };
    let MlsEndpointIdentity::AgentRuntime {
        verification_method,
        agent_key_authorize_event_id,
        ..
    } = &leaf.endpoint
    else {
        anyhow::bail!("creator private group is not an Agent endpoint");
    };
    anyhow::ensure!(
        group.scope() == intent.effective_scope()
            && group.group_id() == *intent.mls_group_id()
            && group.epoch() == 0
            && leaf.leaf_index == 0
            && leaf.actor_id == *intent.owner_actor_id()
            && verification_method == intent.creator_signer_method(),
        "creator private group has another owner, scope or endpoint"
    );
    let (group_info_bytes, ratchet_tree_bytes) = group.public_group_state_bytes()?;
    Ok(MlsCreatorBootstrapArtifactChecks {
        effective_scope: group.scope().clone(),
        mls_group_id: group.group_id(),
        epoch: group.epoch(),
        cipher_suite: group.group_ciphersuite_canonical_id()?.to_owned(),
        creator_leaf_authority: arkret::MlsGenesisCreatorLeafAuthority {
            endpoint: intent.creator_endpoint().clone(),
            leaf_signature_key_b64u: leaf.signature_key.clone(),
            authorization_event_ref: agent_key_authorize_event_id.clone(),
        },
        group_info_bytes,
        ratchet_tree_bytes,
    })
}

fn original_runtime_key(
    record: &MlsCreatorBootstrapRecord,
    key_ref: &ArkretKeyRef,
) -> anyhow::Result<ed25519_dalek::SigningKey> {
    let key = load_ed25519_signing_key(key_ref)?;
    if let Some(evidence) = record.governance_evidence() {
        let MlsCreatorBootstrapEndpointAuthority::Agent(authority) = evidence.creator_authority()
        else {
            anyhow::bail!("runtime cannot take over a Device creator");
        };
        anyhow::ensure!(
            arkret::base64url_encode(key.verifying_key().to_bytes())
                == authority.key().public_key_b64u.as_str(),
            "creator recovery requires its original runtime key"
        );
    }
    Ok(key)
}

fn verify_frozen_creator_producer(
    record: &MlsCreatorBootstrapRecord,
    key: &ed25519_dalek::SigningKey,
) -> anyhow::Result<()> {
    if let Some(queued) = record.queued_genesis() {
        let event = queued.signed_genesis().event();
        let proof = event
            .producer_proof
            .as_ref()
            .context("creator original has no producer proof")?;
        let input = arkret::canonical::canonical_json_bytes(&event.digest_payload()?)?;
        arkret::signatures::verify_ed25519_detached_jws_proof_with_digest_suite(
            proof,
            &input,
            record.intent().owner_actor_id(),
            &arkret::signatures::PublicKeyMaterial::Ed25519Raw {
                bytes: key.verifying_key().to_bytes().to_vec(),
            },
            queued.signed_genesis().digest_suite(),
        )?;
    }
    Ok(())
}

impl ArkretHttpClient {
    /// Resume only the committed explicit intent. A caller may drop this future
    /// at any await; the next invocation re-reads the authoritative durable step.
    pub async fn resume_creator_bootstrap(
        &self,
        store: &FileArkretCryptoStore,
        scope: &ScopeRef,
        key_ref: &ArkretKeyRef,
    ) -> anyhow::Result<MlsCreatorBootstrapRecord> {
        use arkret_wire::MlsCreatorBootstrapState as State;
        let mut submitted = false;
        for _ in 0..16 {
            let record = store
                .creator_bootstrap_record(scope)?
                .context("creator has no explicit durable intent")?;
            let client = self.own_station()?;
            client.check_session()?;
            anyhow::ensure!(
                record.intent().owner_actor_id().as_account_id()
                    == Some(client.session()?.account_id()),
                "creator recovery belongs to another Account"
            );
            anyhow::ensure!(
                record.quarantine_diagnostic().is_none()
                    && record.superseded_winner().is_none()
                    && record.rejection().is_none(),
                "creator terminal attempt requires explicit recovery or retry"
            );
            let key = original_runtime_key(&record, key_ref)?;
            if let Err(error) = verify_frozen_creator_producer(&record, &key) {
                store.quarantine_creator_invariant(
                    &record,
                    client,
                    MlsCreatorBootstrapInvariant::SignedBytes,
                    error.to_string(),
                    None,
                )?;
                return Err(error);
            }
            let producer_context = arkret::ProducerVerificationContext {
                actor: record.intent().owner_actor_id().clone(),
                verification_method: record.intent().creator_signer_method().clone(),
                public_key: arkret::signatures::PublicKeyMaterial::Ed25519Raw {
                    bytes: key.verifying_key().as_bytes().to_vec(),
                },
                digest_suite: arkret::DigestSuite::Sha256,
            };
            if record.epoch_zero().is_some() {
                if let Err(error) = store.restore_creator_group(&record) {
                    store.quarantine_creator(&record, client, error.to_string())?;
                    anyhow::bail!(
                        "creator private recovery quarantined; original material retained"
                    );
                }
            }
            if record.state() == State::Ready {
                store.require_creator_ready(scope)?;
                return Ok(record);
            }
            let mut next = record.clone();
            let mut accepted_cut = None;
            if matches!(
                record.state(),
                State::GovernanceResultPinned | State::Epoch0StatePersisted
            ) {
                let read = garth::mls::creator::read_creator_scope(client, record.intent()).await?;
                if let Some(winner) = read.genesis {
                    next.supersede(MlsCreatorBootstrapWinner::new(
                        &record,
                        winner,
                        read.authority,
                    )?)?;
                    store.commit_creator_step_at_cut(
                        &record,
                        &next,
                        client,
                        None,
                        Some(&read.snapshot),
                    )?;
                    continue;
                }
            }
            match record.state() {
                State::GenesisIntentPersisted => {
                    client
                        .submit_to_realm_authority(
                            &arkret::VerifiedSubmission::verify(
                                record.intent().signed_scope_create_unit().clone(),
                                |_| Ok(producer_context.clone()),
                            )?,
                            &Default::default(),
                        )
                        .await?
                        .value()?;
                    let read =
                        garth::mls::creator::read_creator_scope(client, record.intent()).await?;
                    if let Some(winner) = read.genesis {
                        accepted_cut = Some(read.snapshot);
                        next.supersede(MlsCreatorBootstrapWinner::from_accepted(
                            record.intent(),
                            read.accepted_create,
                            winner,
                            read.authority,
                        )?)?;
                    } else {
                        next.accept_realm(read.accepted_create, read.snapshot)?;
                    }
                }
                State::RealmAccepted => {
                    let request = arkret::SignerKeysQueryRequestBody {
                        request_id: arkret::RequestId::new(arkret::base64url_encode(
                            rand::random::<[u8; 32]>(),
                        ))?,
                        realm_id: scope.realm_id().clone(),
                        recipient_account_id: client.session()?.account_id().clone(),
                        queries: vec![arkret::SignerKeyQuerySelector::CurrentAdmission {
                            sender: arkret::CurrentSignerKeyQuerySender::Agent {
                                actor: record.intent().owner_actor_id().clone(),
                                verification_method: record
                                    .intent()
                                    .creator_signer_method()
                                    .clone(),
                            },
                        }],
                    };
                    let response = client.signer_keys_query(&request).await?;
                    response.value()?.validate_for_request(&request)?;
                    let outcome = response
                        .value()?
                        .results
                        .first()
                        .context("creator Agent key unavailable")?
                        .clone();
                    let authority = MlsCreatorBootstrapAgentAuthority::from_current_signer_query(
                        record.intent(),
                        outcome.clone(),
                        Utc::now(),
                    )?;
                    anyhow::ensure!(
                        authority.key().public_key_b64u.as_str()
                            == arkret::base64url_encode(key.verifying_key().to_bytes()),
                        "current creator Agent key differs from local original"
                    );
                    let read =
                        garth::mls::creator::read_creator_scope(client, record.intent()).await?;
                    if let Some(winner) = read.genesis {
                        accepted_cut = Some(read.snapshot);
                        next.supersede(MlsCreatorBootstrapWinner::new(
                            &record,
                            winner,
                            read.authority,
                        )?)?;
                    } else {
                        let after = client.signer_keys_query(&request).await?;
                        after.value()?.validate_for_request(&request)?;
                        anyhow::ensure!(
                            after.value()?.results == response.value()?.results,
                            "creator Agent authority changed while reading its scope cut"
                        );
                        let authority =
                            MlsCreatorBootstrapAgentAuthority::from_current_signer_query(
                                record.intent(),
                                outcome,
                                Utc::now(),
                            )?;
                        next.pin_governance(
                            MlsCreatorBootstrapGovernanceEvidence::new_with_endpoint_authority(
                                record.intent(),
                                read.accepted_create,
                                read.snapshot,
                                MlsCreatorBootstrapEndpointAuthority::Agent(authority),
                            )?,
                        )?;
                    }
                    response.value()?;
                }
                State::GovernanceResultPinned => {
                    let evidence = record.governance_evidence().context("creator lost pin")?;
                    let identity = ArkretMlsIdentity::new_agent(
                        record.intent().owner_actor_id().clone(),
                        record.intent().creator_signer_method().clone(),
                        evidence
                            .creator_authority()
                            .authorization_event_ref()
                            .clone(),
                        ArkretMlsSigner::from_ed25519_signing_key(key),
                    )?;
                    let mut group = identity.create_group_with_governance_binding(
                        scope,
                        evidence.governance_binding(),
                    )?;
                    group.install_local_creator_binding(
                        record.intent().owner_actor_id().clone(),
                        None,
                    )?;
                    let checks = creator_checks(&group, record.intent())?;
                    let private =
                        zeroize::Zeroizing::new(serde_json::to_vec(&group.export_state_record()?)?);
                    let encrypted = serde_json::to_vec(&store.encrypt_wrapped_state(&private)?)?;
                    let blob_ref = |bytes: &[u8]| {
                        arkret::BlobRef::new(format!(
                            "ak:blob:{}",
                            arkret::canonical::sha256_digest(bytes)
                        ))
                    };
                    let now = arkret::canonical::normalize_timestamp_canonical(Utc::now());
                    let payload = arkret::MlsGenesisPayload {
                        cipher_suite: arkret::NonEmptyString::new(checks.cipher_suite)
                            .map_err(anyhow::Error::msg)?,
                        group_info_ref: blob_ref(&checks.group_info_bytes)?,
                        ratchet_tree_ref: blob_ref(&checks.ratchet_tree_bytes)?,
                        creator_leaf_authority: checks.creator_leaf_authority,
                        governance_binding: evidence.governance_binding().clone(),
                        created_at: now,
                    };
                    let unsigned = arkret::TypedEventDraft::<arkret::event_spec::MlsGenesis>::new(
                        scope.clone(),
                        record.intent().owner_actor_id().clone(),
                        payload,
                    )?
                    .into_intent(now)?
                    .author_with_digest_suite(
                        scope.realm_id().digest_suite_code().digest_suite(),
                    )?;
                    next.persist_epoch_zero(MlsCreatorBootstrapEpochZero::new(
                        record.intent(),
                        evidence,
                        encrypted,
                        checks.group_info_bytes,
                        checks.ratchet_tree_bytes,
                        unsigned,
                    )?)?;
                }
                State::Epoch0StatePersisted => {
                    let unit = record.epoch_zero().context("creator lost private unit")?;
                    let payload = unit.payload()?;
                    for (bytes, reference) in [
                        (unit.group_info_bytes(), payload.group_info_ref),
                        (unit.ratchet_tree_bytes(), payload.ratchet_tree_ref),
                    ] {
                        let metadata = arkret::BlobUploadMetadata {
                            realm_id: Some(scope.realm_id().clone()),
                            content_digest: Some(arkret::Hash::new(
                                arkret::canonical::sha256_digest(bytes),
                            )?),
                            size_bytes: bytes.len() as u64,
                            media_type: Some("application/octet-stream".into()),
                            filename: None,
                            encryption: None,
                        };
                        let uploaded = self
                            .inner
                            .blob_upload_bytes(&metadata, bytes.to_vec())
                            .await?;
                        anyhow::ensure!(
                            uploaded.blob_ref == reference
                                && self.inner.blob_download(&reference, None).await? == bytes,
                            "creator public upload does not match original material"
                        );
                    }
                    let mut signed = unit.unsigned_genesis().clone();
                    super::super::outbound::sign_outbound_event_with_key(
                        &mut signed,
                        key_ref,
                        record.intent().creator_signer_method().as_str(),
                    )?;
                    next.queue_genesis(signed)?;
                }
                State::GenesisQueued => {
                    let read =
                        garth::mls::creator::read_creator_scope(client, record.intent()).await?;
                    if let Some(accepted) = read.genesis {
                        accepted_cut = Some(read.snapshot);
                        let queued = record
                            .queued_genesis()
                            .context("creator queue lost original")?;
                        if accepted.event.event_id == *queued.signed_genesis().event_id() {
                            next.accept_genesis(MlsCreatorBootstrapAcceptedGenesis::new(
                                record.intent(),
                                queued,
                                accepted,
                                read.authority,
                            )?)?;
                        } else {
                            next.supersede(MlsCreatorBootstrapWinner::new(
                                &record,
                                accepted,
                                read.authority,
                            )?)?;
                        }
                    } else {
                        anyhow::ensure!(
                            !submitted,
                            "creator submission is awaiting exact acceptance; retry the same durable transaction"
                        );
                        let queued = record
                            .queued_genesis()
                            .context("creator lost signed original")?;
                        let state = store.load()?;
                        let original = arkret::canonical::canonical_json_bytes(
                            queued.signed_genesis().event(),
                        )?;
                        if state
                            .creator_bootstrap_queue
                            .get(queued.signed_genesis().event_id())
                            != Some(&original)
                        {
                            store.quarantine_creator_invariant(
                                &record,
                                client,
                                MlsCreatorBootstrapInvariant::QueueBinding,
                                "creator queue lost its exact frozen bytes".into(),
                                None,
                            )?;
                            anyhow::bail!(
                                "creator queue is quarantined; original recovery material retained"
                            );
                        }
                        client.check_session()?;
                        if store.creator_bootstrap_record(scope)?.as_ref() != Some(&record) {
                            continue;
                        }
                        let request = arkret_models_collaboration::authority_commit::SelfAuthoritySubmitRequest::Event(
                            arkret::EventAdmissionSubmission::new(queued.signed_genesis().event().clone()));
                        let request = arkret::VerifiedSubmission::verify(request, |_| {
                            Ok(producer_context.clone())
                        })?;
                        if let Err(error) = client
                            .submit_to_realm_authority(&request, &Default::default())
                            .await
                        {
                            let error = anyhow::Error::new(error);
                            let problem = error.chain().find_map(|cause| {
                                match cause.downcast_ref::<arkret::http_client::Error>() {
                                    Some(arkret::http_client::Error::Api { error, .. }) => {
                                        Some(*error.clone())
                                    }
                                    _ => None,
                                }
                            });
                            if let Some(problem) = problem.filter(|problem| {
                                MlsCreatorBootstrapRejection::terminal_problem_reason(problem)
                                    .is_some()
                            }) {
                                let absence = MlsCreatorBootstrapVerifiedAbsence::new(
                                    record.intent(),
                                    read.accepted_create,
                                    read.snapshot,
                                )?;
                                next.reject(MlsCreatorBootstrapRejection::from_problem(
                                    &record, problem, absence,
                                )?)?;
                                store.commit_creator_step(&record, &next, client, None)?;
                            }
                            return Err(error);
                        }
                        submitted = true;
                        // A submit response never advances the logical transaction.
                        continue;
                    }
                }
                State::GenesisAccepted => {
                    let (private, checks) = store.restore_creator_group(&record)?;
                    next.converge_artifacts(MlsCreatorBootstrapArtifacts::new(&record, checks)?)?;
                    store.commit_creator_step(&record, &next, client, Some(private))?;
                    continue;
                }
                State::ArtifactsConverged => {
                    store.publish_creator_ready(&record, client)?;
                    continue;
                }
                _ => anyhow::bail!("creator is at a terminal state"),
            }
            store.commit_creator_step_at_cut(
                &record,
                &next,
                client,
                None,
                accepted_cut.as_ref(),
            )?;
        }
        anyhow::bail!("creator cut changed repeatedly; retry the same durable transaction")
    }
}

impl ArkretHttpClient {
    /// Explicitly close a rejected attempt using a new verified slot decision.
    /// The original selector and identity remain immutable across generations.
    pub async fn retry_creator_bootstrap(
        &self,
        store: &FileArkretCryptoStore,
        scope: &ScopeRef,
        key_ref: &ArkretKeyRef,
    ) -> anyhow::Result<MlsCreatorBootstrapRecord> {
        let record = store
            .creator_bootstrap_record(scope)?
            .context("creator retry lost original intent")?;
        anyhow::ensure!(
            record.rejection().is_some(),
            "creator explicit retry requires a rejected attempt"
        );
        let client = self.own_station()?;
        let read = garth::mls::creator::read_creator_scope(client, record.intent()).await?;
        let mut next = record.clone();
        let mut accepted_cut = None;
        if let Some(winner) = read.genesis {
            accepted_cut = Some(read.snapshot);
            if record
                .rejection()
                .is_some_and(|rejection| rejection.event_id() == &winner.event.event_id)
                || record
                    .closed_attempts()
                    .iter()
                    .any(|closed| closed.event_id() == &winner.event.event_id)
            {
                let known = MlsCreatorBootstrapKnownGenesis::authenticated_winner(
                    &record,
                    winner,
                    read.authority,
                )?;
                store.quarantine_creator_invariant(
                    &record,
                    client,
                    MlsCreatorBootstrapInvariant::AcceptedResult,
                    "previously rejected creator original was independently accepted".into(),
                    Some(known),
                )?;
                anyhow::bail!(
                    "creator accepted result contradicts its rejected attempt; original retained in quarantine"
                );
            }
            next.supersede(MlsCreatorBootstrapWinner::new(
                &record,
                winner,
                read.authority,
            )?)?;
        } else {
            next.reopen_rejected(MlsCreatorBootstrapVerifiedAbsence::new(
                record.intent(),
                read.accepted_create,
                read.snapshot,
            )?)?;
        }
        store.commit_creator_step_at_cut(&record, &next, client, None, accepted_cut.as_ref())?;
        self.resume_creator_bootstrap(store, scope, key_ref).await
    }
}

impl FileArkretCryptoStore {
    pub fn require_creator_ready(&self, scope: &ScopeRef) -> anyhow::Result<()> {
        let state = self.load()?;
        let key = scope_key(scope)?;
        let Some(raw) = state.creator_bootstrap_records.get(&key) else {
            return Ok(());
        };
        let record: MlsCreatorBootstrapRecord = serde_json::from_value(raw.clone())?;
        record.validate()?;
        let group_id = record.intent().mls_group_id().as_str();
        if let Some(winner) = record.superseded_winner() {
            let current = state
                .mls_station_currents
                .get(group_id)
                .and_then(|epochs| epochs.last_key_value())
                .map(|(_, current)| current)
                .context("superseded creator awaits independently installed winner state")?;
            let private = state
                .mls_group_states
                .get(group_id)
                .context("superseded creator has no winning private state")?;
            let group = ArkretMlsGroup::restore_from_state_record(private)?;
            require_creator_current_private(
                &state,
                &record,
                &group,
                current,
                &winner.accepted().event.event_id,
            )?;
            return Ok(());
        }
        let receipt = record
            .ready_receipt()
            .context("creator explicit encryption intent is not write-ready")?;
        let (private, _) = self.restore_creator_group(&record)?;
        require_installed_creator_private(&state, &record, &private)?;
        let current_matches = state
            .mls_station_currents
            .get(group_id)
            .and_then(|epochs| epochs.last_key_value())
            .map_or_else(
                || {
                    state.mls_accepted_bases.get(group_id).is_some_and(|base| {
                        base.event.event_id == *receipt.accepted_genesis_event_id()
                    })
                },
                |(_, current)| current.genesis_event_ref == *receipt.accepted_genesis_event_id(),
            );
        anyhow::ensure!(
            state.creator_bootstrap_ready.get(&key) == Some(receipt)
                && receipt.ready_commit_position() <= state.generation
                && state.mls_group_states.contains_key(group_id)
                && current_matches,
            "creator ready receipt, artifact and send gate disagree"
        );
        Ok(())
    }
}
