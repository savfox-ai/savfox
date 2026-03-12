//! Session lifecycle: creation, resumption, turn management, and initial context.

use super::*;

impl SessionHandle {
    /// Spawn a new [`SessionHandle`] and initialize the session.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn spawn(
        mut config: Config,
        auth_manager: Arc<AuthManager>,
        models_manager: Arc<ModelsManager>,
        skills_manager: Arc<SkillsManager>,
        conversation_history: InitialHistory,
        session_source: SessionSource,
        agent_control: AgentControl,
        dynamic_tools: Vec<DynamicToolSpec>,
    ) -> SavfoxResult<SessionSpawnResult> {
        let (tx_sub, rx_sub) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);
        let (tx_event, rx_event) = async_channel::unbounded();

        let loaded_skills = skills_manager.skills_for_config(&config);

        for err in &loaded_skills.errors {
            error!(
                "failed to load skill {}: {}",
                err.path.display(),
                err.message
            );
        }

        if let SessionSource::SubAgent(SubAgentSource::SessionSpawn { depth, .. }) = session_source
            && depth >= MAX_THREAD_SPAWN_DEPTH
        {
            config.features.disable(Feature::Collab);
        }

        let enabled_skills = loaded_skills.enabled_skills();
        let user_instructions = get_user_instructions(&config, Some(&enabled_skills)).await;

        let exec_policy = ExecPolicyManager::load(&config.features, &config.config_layer_stack)
            .await
            .map_err(|err| SavfoxError::Fatal(format!("failed to load rules: {err}")))?;

        let config = Arc::new(config);
        let _ = models_manager
            .list_models(
                &config,
                crate::models_manager::manager::RefreshStrategy::OnlineIfUncached,
            )
            .await;
        let model = models_manager
            .get_default_model(
                &config.model,
                &config,
                crate::models_manager::manager::RefreshStrategy::OnlineIfUncached,
            )
            .await;

        // Resolve base instructions for the session. Priority order:
        // 1. config.base_instructions override
        // 2. conversation history => session_meta.base_instructions
        // 3. base_intructions for current model
        let model_info = models_manager.get_model_info(model.as_str(), &config).await;
        let base_instructions = config
            .base_instructions
            .clone()
            .or_else(|| conversation_history.get_base_instructions().map(|s| s.text))
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality));

        // Respect session-start tools. When missing (resumed/forked sessions), read from the db
        // first, then fall back to rollout-file tools.
        let persisted_tools = if dynamic_tools.is_empty()
            && config.features.enabled(Feature::Sqlite)
        {
            let session_id = match &conversation_history {
                InitialHistory::Resumed(resumed) => Some(resumed.conversation_id),
                InitialHistory::Forked(_) => conversation_history.forked_from_id(),
                InitialHistory::New => None,
            };
            match session_id {
                Some(session_id) => {
                    let state_db_ctx = state_db::open_if_present(
                        config.savfox_home.as_path(),
                        config.model_provider_id.as_str(),
                    )
                    .await;
                    state_db::get_dynamic_tools(state_db_ctx.as_deref(), session_id, "savfox_spawn")
                        .await
                }
                None => None,
            }
        } else {
            None
        };
        let dynamic_tools = if dynamic_tools.is_empty() {
            persisted_tools
                .or_else(|| conversation_history.get_dynamic_tools())
                .unwrap_or_default()
        } else {
            dynamic_tools
        };

        // TODO (aibrahim): Consolidate config.model and config.model_reasoning_effort into
        // config.collaboration_mode to avoid extracting these fields separately and
        // constructing CollaborationMode here.
        let collaboration_mode = CollaborationMode {
            mode: ModeKind::Custom,
            settings: Settings {
                model: model.clone(),
                reasoning_effort: config.model_reasoning_effort,
                developer_instructions: None,
            },
        };
        let session_configuration = SessionConfiguration {
            provider: config.model_provider.clone(),
            collaboration_mode,
            model_reasoning_summary: config.model_reasoning_summary,
            developer_instructions: config.developer_instructions.clone(),
            user_instructions,
            personality: config.personality,
            base_instructions,
            compact_prompt: config.compact_prompt.clone(),
            approval_policy: config.approval_policy.clone(),
            sandbox_policy: config.sandbox_policy.clone(),
            windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
            cwd: config.cwd.clone(),
            savfox_home: config.savfox_home.clone(),
            session_name: None,
            original_config_do_not_use: Arc::clone(&config),
            session_source,
            dynamic_tools,
            tool_access_policy: config.tool_access_policy.clone(),
        };

        // Generate a unique ID for the lifetime of this session.
        let session_source_clone = session_configuration.session_source.clone();
        let (agent_status_tx, agent_status_rx) = watch::channel(AgentStatus::PendingInit);

        let session_init_span = info_span!("session_init");
        let session = Session::new(
            session_configuration,
            config.clone(),
            auth_manager.clone(),
            models_manager.clone(),
            exec_policy,
            tx_event.clone(),
            agent_status_tx.clone(),
            conversation_history,
            session_source_clone,
            skills_manager,
            agent_control,
        )
        .instrument(session_init_span)
        .await
        .map_err(|e| {
            error!("Failed to create session: {e:#}");
            map_session_init_error(&e, &config.savfox_home)
        })?;
        let session_id = session.conversation_id;

        // This task will run until Op::Shutdown is received.
        let session_loop_span = info_span!("session_loop", session_id = %session_id);
        tokio::spawn(
            task_coordinator::submission_loop(Arc::clone(&session), config, rx_sub)
                .instrument(session_loop_span),
        );
        let handle = SessionHandle {
            next_id: AtomicU64::new(0),
            tx_sub,
            rx_event,
            agent_status: agent_status_rx,
            session,
        };

        Ok(SessionSpawnResult { handle, session_id })
    }
}

impl Session {
    /// Don't expand the number of mutated arguments on config. We are in the process of getting rid
    /// of it.
    pub(crate) fn build_per_turn_config(session_configuration: &SessionConfiguration) -> Config {
        // todo(aibrahim): store this state somewhere else so we don't need to mut config
        let config = session_configuration.original_config_do_not_use.clone();
        let mut per_turn_config = (*config).clone();
        per_turn_config.model_reasoning_effort =
            session_configuration.collaboration_mode.reasoning_effort();
        per_turn_config.model_reasoning_summary = session_configuration.model_reasoning_summary;
        per_turn_config.personality = session_configuration.personality;
        per_turn_config.web_search_mode = Some(resolve_web_search_mode_for_turn(
            per_turn_config.web_search_mode,
            session_configuration.provider.is_azure_responses_endpoint(),
            session_configuration.sandbox_policy.get(),
        ));
        per_turn_config.features = config.features.clone();
        per_turn_config
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn make_turn_context(
        auth_manager: Option<Arc<AuthManager>>,
        otel_manager: &OtelManager,
        provider: ModelProviderInfo,
        session_configuration: &SessionConfiguration,
        per_turn_config: Config,
        model_info: ModelInfo,
        conversation_id: SessionId,
        sub_id: String,
        transport_manager: TransportManager,
    ) -> TurnContext {
        let otel_manager = otel_manager.clone().with_model(
            session_configuration.collaboration_mode.model(),
            model_info.slug.as_str(),
        );
        let per_turn_config = Arc::new(per_turn_config);
        let client = ModelClient::new(
            per_turn_config.clone(),
            auth_manager,
            model_info.clone(),
            otel_manager,
            provider,
            session_configuration.collaboration_mode.reasoning_effort(),
            session_configuration.model_reasoning_summary,
            conversation_id,
            session_configuration.session_source.clone(),
            transport_manager,
        );

        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &per_turn_config.features,
            web_search_mode: per_turn_config.web_search_mode,
        });

        TurnContext {
            sub_id,
            client,
            cwd: session_configuration.cwd.clone(),
            developer_instructions: session_configuration.developer_instructions.clone(),
            compact_prompt: session_configuration.compact_prompt.clone(),
            user_instructions: session_configuration.user_instructions.clone(),
            collaboration_mode: session_configuration.collaboration_mode.clone(),
            personality: session_configuration.personality,
            approval_policy: session_configuration.approval_policy.value(),
            sandbox_policy: session_configuration.sandbox_policy.get().clone(),
            windows_sandbox_level: session_configuration.windows_sandbox_level,
            shell_environment_policy: per_turn_config.shell_environment_policy.clone(),
            tools_config,
            ghost_snapshot: per_turn_config.ghost_snapshot.clone(),
            final_output_json_schema: None,
            savfox_linux_sandbox_exe: per_turn_config.savfox_linux_sandbox_exe.clone(),
            tool_call_gate: Arc::new(ReadinessFlag::new()),
            truncation_policy: model_info.truncation_policy.into(),
            dynamic_tools: session_configuration.dynamic_tools.clone(),
            tool_access_policy: session_configuration.tool_access_policy.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn new(
        mut session_configuration: SessionConfiguration,
        config: Arc<Config>,
        auth_manager: Arc<AuthManager>,
        models_manager: Arc<ModelsManager>,
        exec_policy: ExecPolicyManager,
        tx_event: Sender<Event>,
        agent_status: watch::Sender<AgentStatus>,
        initial_history: InitialHistory,
        session_source: SessionSource,
        skills_manager: Arc<SkillsManager>,
        agent_control: AgentControl,
    ) -> anyhow::Result<Arc<Self>> {
        debug!(
            "Configuring session: model={}; provider={:?}",
            session_configuration.collaboration_mode.model(),
            session_configuration.provider
        );
        if !session_configuration.cwd.is_absolute() {
            return Err(anyhow::anyhow!(
                "cwd is not absolute: {:?}",
                session_configuration.cwd
            ));
        }

        let forked_from_id = initial_history.forked_from_id();

        let (conversation_id, rollout_params) = match &initial_history {
            InitialHistory::New | InitialHistory::Forked(_) => {
                let conversation_id = SessionId::default();
                (
                    conversation_id,
                    RolloutRecorderParams::new(
                        conversation_id,
                        forked_from_id,
                        session_source,
                        BaseInstructions {
                            text: session_configuration.base_instructions.clone(),
                        },
                        session_configuration.dynamic_tools.clone(),
                    ),
                )
            }
            InitialHistory::Resumed(resumed_history) => (
                resumed_history.conversation_id,
                RolloutRecorderParams::resume(resumed_history.rollout_path.clone()),
            ),
        };
        let state_builder = match &initial_history {
            InitialHistory::Resumed(resumed) => metadata::builder_from_items(
                resumed.history.as_slice(),
                resumed.rollout_path.as_path(),
            ),
            InitialHistory::New | InitialHistory::Forked(_) => None,
        };

        // Kick off independent async setup tasks in parallel to reduce startup latency.
        let rollout_fut = async {
            if config.ephemeral {
                Ok::<_, anyhow::Error>((None, None))
            } else {
                let state_db_ctx = state_db::init_if_enabled(&config, None).await;
                let rollout_recorder = RolloutRecorder::new(
                    &config,
                    rollout_params,
                    state_db_ctx.clone(),
                    state_builder.clone(),
                )
                .await?;
                Ok((Some(rollout_recorder), state_db_ctx))
            }
        };

        let history_meta_fut = crate::message_history::history_metadata(&config);
        let auth_manager_clone = Arc::clone(&auth_manager);
        let config_for_mcp = Arc::clone(&config);
        let auth_and_mcp_fut = async move {
            let auth = auth_manager_clone.auth().await;
            let mcp_servers = effective_mcp_servers(&config_for_mcp, auth.as_ref());
            let auth_statuses = compute_auth_statuses(
                mcp_servers.iter(),
                config_for_mcp.mcp_oauth_credentials_store_mode,
            )
            .await;
            (auth, mcp_servers, auth_statuses)
        };

        // Join all independent futures.
        let (
            rollout_recorder_and_state_db,
            (history_log_id, history_entry_count),
            (auth, mcp_servers, auth_statuses),
        ) = tokio::join!(rollout_fut, history_meta_fut, auth_and_mcp_fut);

        let (rollout_recorder, state_db_ctx) = rollout_recorder_and_state_db.map_err(|e| {
            error!("failed to initialize rollout recorder: {e:#}");
            e
        })?;
        let rollout_path = rollout_recorder
            .as_ref()
            .map(|rec| rec.rollout_path.clone());

        let mut post_session_configured_events = Vec::<Event>::new();

        for usage in config.features.legacy_feature_usages() {
            post_session_configured_events.push(Event {
                id: INITIAL_SUBMIT_ID.to_owned(),
                msg: EventMsg::DeprecationNotice(DeprecationNoticeEvent {
                    summary: usage.summary.clone(),
                    details: usage.details.clone(),
                }),
            });
        }
        maybe_push_chat_wire_api_deprecation(&config, &mut post_session_configured_events);
        maybe_push_unstable_features_warning(&config, &mut post_session_configured_events);

        let auth = auth.as_ref();
        let otel_manager = OtelManager::new(
            conversation_id,
            session_configuration.collaboration_mode.model(),
            session_configuration.collaboration_mode.model(),
            auth.and_then(SavfoxAuth::get_account_id),
            auth.and_then(SavfoxAuth::get_account_email),
            auth.map(SavfoxAuth::api_auth_mode),
            config.otel.log_user_prompt,
            terminal::user_agent(),
            session_configuration.session_source.clone(),
        );
        config.features.emit_metrics(&otel_manager);
        otel_manager.counter(
            "savfox.session.started",
            1,
            &[(
                "is_git",
                if get_git_repo_root(&session_configuration.cwd).is_some() {
                    "true"
                } else {
                    "false"
                },
            )],
        );

        otel_manager.conversation_starts(
            config.model_provider.id.as_str(),
            session_configuration.collaboration_mode.reasoning_effort(),
            config.model_reasoning_summary,
            config.model_context_window,
            config.model_auto_compact_token_limit,
            config.approval_policy.value(),
            config.sandbox_policy.get().clone(),
            mcp_servers.keys().map(String::as_str).collect(),
        );

        let mut default_shell = shell::default_user_shell();
        // Create the mutable state for the Session.
        if config.features.enabled(Feature::ShellSnapshot) {
            ShellSnapshot::start_snapshotting(
                config.savfox_home.clone(),
                conversation_id,
                &mut default_shell,
                otel_manager.clone(),
            );
        }
        let session_name =
            match session_index::find_session_name_by_id(&config.savfox_home, &conversation_id)
                .await
            {
                Ok(name) => name,
                Err(err) => {
                    warn!("Failed to read session index for session name: {err}");
                    None
                }
            };
        session_configuration.session_name = session_name.clone();
        let state = SessionState::new(session_configuration.clone());

        let services = SessionServices {
            mcp_connection_manager: Arc::new(RwLock::new(McpConnectionManager::default())),
            mcp_startup_cancellation_token: Mutex::new(CancellationToken::new()),
            unified_exec_manager: UnifiedExecProcessManager::default(),
            analytics_events_client: AnalyticsEventsClient::new(
                Arc::clone(&config),
                Arc::clone(&auth_manager),
            ),
            notifier: UserNotifier::new(config.notify.clone()),
            rollout: Mutex::new(rollout_recorder),
            user_shell: Arc::new(default_shell),
            show_raw_agent_reasoning: config.show_raw_agent_reasoning,
            exec_policy,
            auth_manager: Arc::clone(&auth_manager),
            otel_manager,
            models_manager: Arc::clone(&models_manager),
            tool_approvals: Mutex::new(ApprovalStore::default()),
            skills_manager,
            agent_control,
            state_db: state_db_ctx.clone(),
            transport_manager: TransportManager::new(),
        };

        let sess = Arc::new(Session {
            conversation_id,
            tx_event: tx_event.clone(),
            agent_status,
            state: Mutex::new(state),
            features: config.features.clone(),
            pending_mcp_server_refresh_config: Mutex::new(None),
            active_turn: Mutex::new(None),
            services,
            next_internal_sub_id: AtomicU64::new(0),
        });

        // Dispatch the SessionConfiguredEvent first and then report any errors.
        let initial_messages = initial_history.get_event_msgs();
        let events = std::iter::once(Event {
            id: INITIAL_SUBMIT_ID.to_owned(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: conversation_id,
                forked_from_id,
                session_name: session_configuration.session_name.clone(),
                model: session_configuration.collaboration_mode.model().to_string(),
                model_provider_id: config.model_provider_id.clone(),
                approval_policy: session_configuration.approval_policy.value(),
                sandbox_policy: session_configuration.sandbox_policy.get().clone(),
                cwd: session_configuration.cwd.clone(),
                reasoning_effort: session_configuration.collaboration_mode.reasoning_effort(),
                history_log_id,
                history_entry_count,
                initial_messages,
                rollout_path,
            }),
        })
        .chain(post_session_configured_events.into_iter());
        for event in events {
            sess.send_event_raw(event).await;
        }

        // Construct sandbox_state before initialize() so it can be sent to each
        // MCP server immediately after it becomes ready (avoiding blocking).
        let sandbox_state = SandboxState {
            sandbox_policy: session_configuration.sandbox_policy.get().clone(),
            savfox_linux_sandbox_exe: config.savfox_linux_sandbox_exe.clone(),
            sandbox_cwd: session_configuration.cwd.clone(),
        };
        let cancel_token = sess.mcp_startup_cancellation_token().await;

        sess.services
            .mcp_connection_manager
            .write()
            .await
            .initialize(
                &mcp_servers,
                config.mcp_oauth_credentials_store_mode,
                auth_statuses.clone(),
                tx_event.clone(),
                cancel_token,
                sandbox_state,
            )
            .await;

        // record_initial_history can emit events. We record only after the SessionConfiguredEvent
        // is emitted.
        sess.record_initial_history(initial_history).await;

        Ok(sess)
    }

    pub(super) async fn record_initial_history(&self, conversation_history: InitialHistory) {
        let turn_context = self.new_default_turn().await;
        match conversation_history {
            InitialHistory::New => {
                // Build and record initial items (user instructions + environment context)
                let items = self.build_initial_context(&turn_context).await;
                self.record_conversation_items(&turn_context, &items).await;
                {
                    let mut state = self.state.lock().await;
                    state.initial_context_seeded = true;
                }
                // Ensure initial items are visible to immediate readers (e.g., tests, forks).
                self.flush_rollout().await;
            }
            InitialHistory::Resumed(resumed_history) => {
                let rollout_items = resumed_history.history;
                {
                    let mut state = self.state.lock().await;
                    state.initial_context_seeded = false;
                }

                // If resuming, warn when the last recorded model differs from the current one.
                if let Some(prev) = rollout_items.iter().rev().find_map(|it| {
                    if let RolloutItem::TurnContext(ctx) = it {
                        Some(ctx.model.as_str())
                    } else {
                        None
                    }
                }) {
                    let curr = turn_context.client.get_model();
                    if prev != curr {
                        warn!(
                            "resuming session with different model: previous={prev}, current={curr}"
                        );
                        self.send_event(
                            &turn_context,
                            EventMsg::Warning(WarningEvent {
                                message: format!(
                                    "This session was recorded with model `{prev}` but is resuming with `{curr}`. \
                         Consider switching back to `{prev}` as it may affect Savfox performance."
                                ),
                            }),
                        )
                            .await;
                    }
                }

                // Always add response items to conversation history
                let reconstructed_history = self
                    .reconstruct_history_from_rollout(&turn_context, &rollout_items)
                    .await;
                if !reconstructed_history.is_empty() {
                    self.record_into_history(&reconstructed_history, &turn_context)
                        .await;
                }

                // Seed usage info from the recorded rollout so UIs can show token counts
                // immediately on resume/fork.
                if let Some(info) = Self::last_token_info_from_rollout(&rollout_items) {
                    let mut state = self.state.lock().await;
                    state.set_token_info(Some(info));
                }

                // Defer seeding the session's initial context until the first turn starts so
                // turn/start overrides can be merged before we write to the rollout.
                self.flush_rollout().await;
            }
            InitialHistory::Forked(rollout_items) => {
                // Always add response items to conversation history
                let reconstructed_history = self
                    .reconstruct_history_from_rollout(&turn_context, &rollout_items)
                    .await;
                if !reconstructed_history.is_empty() {
                    self.record_into_history(&reconstructed_history, &turn_context)
                        .await;
                }

                // Seed usage info from the recorded rollout so UIs can show token counts
                // immediately on resume/fork.
                if let Some(info) = Self::last_token_info_from_rollout(&rollout_items) {
                    let mut state = self.state.lock().await;
                    state.set_token_info(Some(info));
                }

                // If persisting, persist all rollout items as-is (recorder filters)
                if !rollout_items.is_empty() {
                    self.persist_rollout_items(&rollout_items).await;
                }

                // Append the current session's initial context after the reconstructed history.
                let initial_context = self.build_initial_context(&turn_context).await;
                self.record_conversation_items(&turn_context, &initial_context)
                    .await;
                {
                    let mut state = self.state.lock().await;
                    state.initial_context_seeded = true;
                }
                // Flush after seeding history and any persisted rollout copy.
                self.flush_rollout().await;
            }
        }
    }

    fn last_token_info_from_rollout(rollout_items: &[RolloutItem]) -> Option<TokenUsageInfo> {
        rollout_items.iter().rev().find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::TokenCount(ev)) => ev.info.clone(),
            _ => None,
        })
    }

    pub(crate) async fn update_settings(
        &self,
        updates: SessionSettingsUpdate,
    ) -> ConstraintResult<()> {
        let mut state = self.state.lock().await;

        match state.session_configuration.apply(&updates) {
            Ok(updated) => {
                state.session_configuration = updated;
                Ok(())
            }
            Err(err) => {
                warn!("rejected session settings update: {err}");
                Err(err)
            }
        }
    }

    pub(crate) async fn new_turn_with_sub_id(
        &self,
        sub_id: String,
        updates: SessionSettingsUpdate,
    ) -> ConstraintResult<Arc<TurnContext>> {
        let (session_configuration, sandbox_policy_changed) = {
            let mut state = self.state.lock().await;
            match state.session_configuration.clone().apply(&updates) {
                Ok(next) => {
                    let sandbox_policy_changed =
                        state.session_configuration.sandbox_policy != next.sandbox_policy;
                    state.session_configuration = next.clone();
                    (next, sandbox_policy_changed)
                }
                Err(err) => {
                    drop(state);
                    self.send_event_raw(Event {
                        id: sub_id.clone(),
                        msg: EventMsg::Error(ErrorEvent {
                            message: err.to_string(),
                            savfox_error_info: Some(SavfoxErrorInfo::BadRequest),
                        }),
                    })
                    .await;
                    return Err(err);
                }
            }
        };

        Ok(self
            .new_turn_from_configuration(
                sub_id,
                session_configuration,
                updates.final_output_json_schema,
                sandbox_policy_changed,
            )
            .await)
    }

    async fn new_turn_from_configuration(
        &self,
        sub_id: String,
        session_configuration: SessionConfiguration,
        final_output_json_schema: Option<Option<Value>>,
        sandbox_policy_changed: bool,
    ) -> Arc<TurnContext> {
        let per_turn_config = Self::build_per_turn_config(&session_configuration);

        if sandbox_policy_changed {
            let sandbox_state = SandboxState {
                sandbox_policy: per_turn_config.sandbox_policy.get().clone(),
                savfox_linux_sandbox_exe: per_turn_config.savfox_linux_sandbox_exe.clone(),
                sandbox_cwd: per_turn_config.cwd.clone(),
            };
            if let Err(e) = self
                .services
                .mcp_connection_manager
                .read()
                .await
                .notify_sandbox_state_change(&sandbox_state)
                .await
            {
                warn!("Failed to notify sandbox state change to MCP servers: {e:#}");
            }
        }

        let model_info = self
            .services
            .models_manager
            .get_model_info(
                session_configuration.collaboration_mode.model(),
                &per_turn_config,
            )
            .await;
        let mut turn_context: TurnContext = Self::make_turn_context(
            Some(Arc::clone(&self.services.auth_manager)),
            &self.services.otel_manager,
            session_configuration.provider.clone(),
            &session_configuration,
            per_turn_config,
            model_info,
            self.conversation_id,
            sub_id,
            self.services.transport_manager.clone(),
        );
        if let Some(final_schema) = final_output_json_schema {
            turn_context.final_output_json_schema = final_schema;
        }
        Arc::new(turn_context)
    }

    pub(crate) async fn new_default_turn(&self) -> Arc<TurnContext> {
        self.new_default_turn_with_sub_id(self.next_internal_sub_id())
            .await
    }

    pub(crate) async fn new_default_turn_with_sub_id(&self, sub_id: String) -> Arc<TurnContext> {
        let session_configuration = {
            let state = self.state.lock().await;
            state.session_configuration.clone()
        };
        self.new_turn_from_configuration(sub_id, session_configuration, None, false)
            .await
    }

    fn build_environment_update_item(
        &self,
        previous: Option<&Arc<TurnContext>>,
        next: &TurnContext,
    ) -> Option<ResponseItem> {
        let prev = previous?;

        let shell = self.user_shell();
        let prev_context = EnvironmentContext::from_turn_context(prev.as_ref(), shell.as_ref());
        let next_context = EnvironmentContext::from_turn_context(next, shell.as_ref());
        if prev_context.equals_except_shell(&next_context) {
            return None;
        }
        Some(ResponseItem::from(EnvironmentContext::diff(
            prev.as_ref(),
            next,
            shell.as_ref(),
        )))
    }

    fn build_permissions_update_item(
        &self,
        previous: Option<&Arc<TurnContext>>,
        next: &TurnContext,
    ) -> Option<ResponseItem> {
        let prev = previous?;
        if prev.sandbox_policy == next.sandbox_policy
            && prev.approval_policy == next.approval_policy
        {
            return None;
        }

        Some(
            DeveloperInstructions::from_policy(
                &next.sandbox_policy,
                next.approval_policy,
                self.services.exec_policy.current().as_ref(),
                self.features.enabled(Feature::RequestRule),
                &next.cwd,
            )
            .into(),
        )
    }

    fn build_personality_update_item(
        &self,
        previous: Option<&Arc<TurnContext>>,
        next: &TurnContext,
    ) -> Option<ResponseItem> {
        if !self.features.enabled(Feature::Personality) {
            return None;
        }
        let previous = previous?;

        // if a personality is specified and it's different from the previous one, build a
        // personality update item
        if let Some(personality) = next.personality
            && next.personality != previous.personality
        {
            let model_info = next.client.get_model_info();
            let personality_message = Self::personality_message_for(&model_info, personality);
            personality_message.map(|personality_message| {
                DeveloperInstructions::personality_spec_message(personality_message).into()
            })
        } else {
            None
        }
    }

    fn personality_message_for(model_info: &ModelInfo, personality: Personality) -> Option<String> {
        model_info
            .model_messages
            .as_ref()
            .and_then(|spec| spec.get_personality_message(Some(personality)))
            .filter(|message| !message.is_empty())
    }

    fn build_collaboration_mode_update_item(
        &self,
        previous: Option<&Arc<TurnContext>>,
        next: &TurnContext,
    ) -> Option<ResponseItem> {
        let prev = previous?;
        if prev.collaboration_mode != next.collaboration_mode {
            // If the next mode has empty developer instructions, this returns None and we emit no
            // update, so prior collaboration instructions remain in the prompt history.
            Some(DeveloperInstructions::from_collaboration_mode(&next.collaboration_mode)?.into())
        } else {
            None
        }
    }

    pub(super) fn build_settings_update_items(
        &self,
        previous_context: Option<&Arc<TurnContext>>,
        current_context: &TurnContext,
    ) -> Vec<ResponseItem> {
        let mut update_items = Vec::new();
        if let Some(env_item) =
            self.build_environment_update_item(previous_context, current_context)
        {
            update_items.push(env_item);
        }
        if let Some(permissions_item) =
            self.build_permissions_update_item(previous_context, current_context)
        {
            update_items.push(permissions_item);
        }
        if let Some(collaboration_mode_item) =
            self.build_collaboration_mode_update_item(previous_context, current_context)
        {
            update_items.push(collaboration_mode_item);
        }
        if let Some(personality_item) =
            self.build_personality_update_item(previous_context, current_context)
        {
            update_items.push(personality_item);
        }
        update_items
    }

    pub(crate) async fn build_initial_context(
        &self,
        turn_context: &TurnContext,
    ) -> Vec<ResponseItem> {
        let mut items = Vec::<ResponseItem>::with_capacity(4);
        let shell = self.user_shell();
        items.push(
            DeveloperInstructions::from_policy(
                &turn_context.sandbox_policy,
                turn_context.approval_policy,
                self.services.exec_policy.current().as_ref(),
                self.features.enabled(Feature::RequestRule),
                &turn_context.cwd,
            )
            .into(),
        );
        if let Some(developer_instructions) = turn_context.developer_instructions.as_deref() {
            items.push(DeveloperInstructions::new(developer_instructions.to_string()).into());
        }
        // Add developer instructions from collaboration_mode if they exist and are non-empty
        let (collaboration_mode, base_instructions) = {
            let state = self.state.lock().await;
            (
                state.session_configuration.collaboration_mode.clone(),
                state.session_configuration.base_instructions.clone(),
            )
        };
        if let Some(collab_instructions) =
            DeveloperInstructions::from_collaboration_mode(&collaboration_mode)
        {
            items.push(collab_instructions.into());
        }
        if self.features.enabled(Feature::Personality)
            && let Some(personality) = turn_context.personality
        {
            let model_info = turn_context.client.get_model_info();
            let has_baked_personality = model_info.supports_personality()
                && base_instructions == model_info.get_model_instructions(Some(personality));
            if !has_baked_personality
                && let Some(personality_message) =
                    Self::personality_message_for(&model_info, personality)
            {
                items.push(
                    DeveloperInstructions::personality_spec_message(personality_message).into(),
                );
            }
        }
        if let Some(user_instructions) = turn_context.user_instructions.as_deref() {
            items.push(
                UserInstructions {
                    text: user_instructions.to_string(),
                    directory: turn_context.cwd.to_string_lossy().into_owned(),
                }
                .into(),
            );
        }
        // Inject Markdown memory context (4-layer system).
        {
            let savfox_home = self.savfox_home().await;
            let project_root = get_git_repo_root(&turn_context.cwd);
            let memory_entries = crate::md_memory::discover_md_memories(
                &savfox_home,
                project_root.as_deref(),
                "default",
            )
            .await;
            let memory_prompt = crate::md_memory::assemble_memory_prompt(
                &memory_entries,
                crate::md_memory::DEFAULT_MD_MEMORY_MAX_BYTES,
            );
            if !memory_prompt.is_empty() {
                items.push(
                    crate::instructions::MemoryInstructions {
                        text: memory_prompt,
                    }
                    .into(),
                );
            }
        }
        items.push(ResponseItem::from(EnvironmentContext::new(
            Some(turn_context.cwd.clone()),
            shell.as_ref().clone(),
        )));
        items
    }

    pub(crate) async fn seed_initial_context_if_needed(&self, turn_context: &TurnContext) {
        {
            let mut state = self.state.lock().await;
            if state.initial_context_seeded {
                return;
            }
            state.initial_context_seeded = true;
        }

        let initial_context = self.build_initial_context(turn_context).await;
        self.record_conversation_items(turn_context, &initial_context)
            .await;
        self.flush_rollout().await;
    }
}
