use std::sync::{Arc, Weak};

use savfox_protocol::SessionId;
use savfox_protocol::protocol::Op;
use savfox_protocol::user_input::UserInput;
use tokio::sync::watch;

use crate::agent::AgentStatus;
use crate::agent::guards::Guards;
use crate::error::{SavfoxError, Result as SavfoxResult};
use crate::session_manager::SessionManagerState;

/// Control-plane handle for multi-agent operations.
/// `AgentControl` is held by each session (via `SessionServices`). It provides capability to
/// spawn new agents and the inter-agent communication layer.
/// An `AgentControl` instance is shared per "user session" which means the same `AgentControl`
/// is used for every sub-agent spawned by Savfox. By doing so, we make sure the guards are
/// scoped to a user session.
#[derive(Clone, Default)]
pub(crate) struct AgentControl {
    /// Weak handle back to the global session registry/state.
    /// This is `Weak` to avoid reference cycles and shadow persistence of the form
    /// `SessionManagerState -> SavfoxSession -> Session -> SessionServices -> SessionManagerState`.
    manager: Weak<SessionManagerState>,
    state: Arc<Guards>,
}

impl AgentControl {
    /// Construct a new `AgentControl` that can spawn/message agents via the given manager state.
    pub(crate) fn new(manager: Weak<SessionManagerState>) -> Self {
        Self {
            manager,
            ..Default::default()
        }
    }

    /// Spawn a new agent session and submit the initial prompt.
    pub(crate) async fn spawn_agent(
        &self,
        config: crate::config::Config,
        prompt: String,
        session_source: Option<savfox_protocol::protocol::SessionSource>,
    ) -> SavfoxResult<SessionId> {
        let state = self.upgrade()?;
        let reservation = self.state.reserve_spawn_slot(config.agent_max_sessions)?;

        // The same `AgentControl` is sent to spawn the session.
        let new_session = match session_source {
            Some(session_source) => {
                state
                    .spawn_new_session_with_source(config, self.clone(), session_source)
                    .await?
            }
            None => state.spawn_new_session(config, self.clone()).await?,
        };
        reservation.commit(new_session.session_id);

        // Notify a new session has been created. This notification will be processed by clients
        // to subscribe or drain this newly created session.
        // TODO(jif) add helper for drain
        state.notify_session_created(new_session.session_id);

        self.send_prompt(new_session.session_id, prompt).await?;

        Ok(new_session.session_id)
    }

    /// Send a `user` prompt to an existing agent session.
    pub(crate) async fn send_prompt(
        &self,
        agent_id: SessionId,
        prompt: String,
    ) -> SavfoxResult<String> {
        let state = self.upgrade()?;
        let result = state
            .send_op(
                agent_id,
                Op::UserInput {
                    items: vec![UserInput::Text {
                        text: prompt,
                        // Agent control prompts are plain text with no UI text elements.
                        text_elements: Vec::new(),
                    }],
                    final_output_json_schema: None,
                },
            )
            .await;
        if matches!(result, Err(SavfoxError::InternalAgentDied)) {
            let _ = state.remove_session(&agent_id).await;
            self.state.release_spawned_session(agent_id);
        }
        result
    }

    /// Interrupt the current task for an existing agent session.
    pub(crate) async fn interrupt_agent(&self, agent_id: SessionId) -> SavfoxResult<String> {
        let state = self.upgrade()?;
        state.send_op(agent_id, Op::Interrupt).await
    }

    /// Submit a shutdown request to an existing agent session.
    pub(crate) async fn shutdown_agent(&self, agent_id: SessionId) -> SavfoxResult<String> {
        let state = self.upgrade()?;
        let result = state.send_op(agent_id, Op::Shutdown {}).await;
        let _ = state.remove_session(&agent_id).await;
        self.state.release_spawned_session(agent_id);
        result
    }

    /// Fetch the last known status for `agent_id`, returning `NotFound` when unavailable.
    pub(crate) async fn get_status(&self, agent_id: SessionId) -> AgentStatus {
        let Ok(state) = self.upgrade() else {
            // No agent available if upgrade fails.
            return AgentStatus::NotFound;
        };
        let Ok(session) = state.get_session(agent_id).await else {
            return AgentStatus::NotFound;
        };
        session.agent_status().await
    }

    /// Subscribe to status updates for `agent_id`, yielding the latest value and changes.
    pub(crate) async fn subscribe_status(
        &self,
        agent_id: SessionId,
    ) -> SavfoxResult<watch::Receiver<AgentStatus>> {
        let state = self.upgrade()?;
        let session = state.get_session(agent_id).await?;
        Ok(session.subscribe_status())
    }

    fn upgrade(&self) -> SavfoxResult<Arc<SessionManagerState>> {
        self.manager
            .upgrade()
            .ok_or_else(|| SavfoxError::UnsupportedOperation("session manager dropped".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use savfox_protocol::config_types::ModeKind;
    use savfox_protocol::protocol::{
        ErrorEvent, EventMsg, TurnAbortReason, TurnAbortedEvent, TurnCompleteEvent,
        TurnStartedEvent,
    };
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;
    use toml::Value as TomlValue;

    use super::*;
    use crate::agent::agent_status_from_event;
    use crate::config::{Config, ConfigBuilder};
    use crate::{SavfoxAuth, SavfoxSession, SessionManager};

    async fn test_config_with_cli_overrides(
        cli_overrides: Vec<(String, TomlValue)>,
    ) -> (TempDir, Config) {
        let home = TempDir::new().expect("create temp dir");
        let config = ConfigBuilder::default()
            .savfox_home(home.path().to_path_buf())
            .cli_overrides(cli_overrides)
            .build()
            .await
            .expect("load default test config");
        (home, config)
    }

    async fn test_config() -> (TempDir, Config) {
        test_config_with_cli_overrides(Vec::new()).await
    }

    struct AgentControlHarness {
        _home: TempDir,
        config: Config,
        manager: SessionManager,
        control: AgentControl,
    }

    impl AgentControlHarness {
        async fn new() -> Self {
            let (home, config) = test_config().await;
            let manager = SessionManager::with_models_provider_and_home(
                SavfoxAuth::from_api_key("dummy"),
                config.model_provider.clone(),
                config.savfox_home.clone(),
            );
            let control = manager.agent_control();
            Self {
                _home: home,
                config,
                manager,
                control,
            }
        }

        async fn start_session(&self) -> (SessionId, Arc<SavfoxSession>) {
            let new_session = self
                .manager
                .start_session(self.config.clone())
                .await
                .expect("start session");
            (new_session.session_id, new_session.session)
        }
    }

    #[tokio::test]
    async fn send_prompt_errors_when_manager_dropped() {
        let control = AgentControl::default();
        let err = control
            .send_prompt(SessionId::new(), "hello".to_string())
            .await
            .expect_err("send_prompt should fail without a manager");
        assert_eq!(
            err.to_string(),
            "unsupported operation: session manager dropped"
        );
    }

    #[tokio::test]
    async fn get_status_returns_not_found_without_manager() {
        let control = AgentControl::default();
        let got = control.get_status(SessionId::new()).await;
        assert_eq!(got, AgentStatus::NotFound);
    }

    #[tokio::test]
    async fn on_event_updates_status_from_task_started() {
        let status = agent_status_from_event(&EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            collaboration_mode_kind: ModeKind::Custom,
        }));
        assert_eq!(status, Some(AgentStatus::Running));
    }

    #[tokio::test]
    async fn on_event_updates_status_from_task_complete() {
        let status = agent_status_from_event(&EventMsg::TurnComplete(TurnCompleteEvent {
            last_agent_message: Some("done".to_string()),
        }));
        let expected = AgentStatus::Completed(Some("done".to_string()));
        assert_eq!(status, Some(expected));
    }

    #[tokio::test]
    async fn on_event_updates_status_from_error() {
        let status = agent_status_from_event(&EventMsg::Error(ErrorEvent {
            message: "boom".to_string(),
            savfox_error_info: None,
        }));

        let expected = AgentStatus::Errored("boom".to_string());
        assert_eq!(status, Some(expected));
    }

    #[tokio::test]
    async fn on_event_updates_status_from_turn_aborted() {
        let status = agent_status_from_event(&EventMsg::TurnAborted(TurnAbortedEvent {
            reason: TurnAbortReason::Interrupted,
        }));

        let expected = AgentStatus::Errored("Interrupted".to_string());
        assert_eq!(status, Some(expected));
    }

    #[tokio::test]
    async fn on_event_updates_status_from_shutdown_complete() {
        let status = agent_status_from_event(&EventMsg::ShutdownComplete);
        assert_eq!(status, Some(AgentStatus::Shutdown));
    }

    #[tokio::test]
    async fn spawn_agent_errors_when_manager_dropped() {
        let control = AgentControl::default();
        let (_home, config) = test_config().await;
        let err = control
            .spawn_agent(config, "hello".to_string(), None)
            .await
            .expect_err("spawn_agent should fail without a manager");
        assert_eq!(
            err.to_string(),
            "unsupported operation: session manager dropped"
        );
    }

    #[tokio::test]
    async fn send_prompt_errors_when_session_missing() {
        let harness = AgentControlHarness::new().await;
        let session_id = SessionId::new();
        let err = harness
            .control
            .send_prompt(session_id, "hello".to_string())
            .await
            .expect_err("send_prompt should fail for missing session");
        assert_matches!(err, SavfoxError::SessionNotFound(id) if id == session_id);
    }

    #[tokio::test]
    async fn get_status_returns_not_found_for_missing_session() {
        let harness = AgentControlHarness::new().await;
        let status = harness.control.get_status(SessionId::new()).await;
        assert_eq!(status, AgentStatus::NotFound);
    }

    #[tokio::test]
    async fn get_status_returns_pending_init_for_new_session() {
        let harness = AgentControlHarness::new().await;
        let (session_id, _) = harness.start_session().await;
        let status = harness.control.get_status(session_id).await;
        assert_eq!(status, AgentStatus::PendingInit);
    }

    #[tokio::test]
    async fn subscribe_status_errors_for_missing_session() {
        let harness = AgentControlHarness::new().await;
        let session_id = SessionId::new();
        let err = harness
            .control
            .subscribe_status(session_id)
            .await
            .expect_err("subscribe_status should fail for missing session");
        assert_matches!(err, SavfoxError::SessionNotFound(id) if id == session_id);
    }

    #[tokio::test]
    async fn subscribe_status_updates_on_shutdown() {
        let harness = AgentControlHarness::new().await;
        let (session_id, session) = harness.start_session().await;
        let mut status_rx = harness
            .control
            .subscribe_status(session_id)
            .await
            .expect("subscribe_status should succeed");
        assert_eq!(status_rx.borrow().clone(), AgentStatus::PendingInit);

        let _ = session
            .submit(Op::Shutdown {})
            .await
            .expect("shutdown should submit");

        let _ = status_rx.changed().await;
        assert_eq!(status_rx.borrow().clone(), AgentStatus::Shutdown);
    }

    #[tokio::test]
    async fn send_prompt_submits_user_message() {
        let harness = AgentControlHarness::new().await;
        let (session_id, _session) = harness.start_session().await;

        let submission_id = harness
            .control
            .send_prompt(session_id, "hello from tests".to_string())
            .await
            .expect("send_prompt should succeed");
        assert!(!submission_id.is_empty());
        let expected = (
            session_id,
            Op::UserInput {
                items: vec![UserInput::Text {
                    text: "hello from tests".to_string(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            },
        );
        let captured = harness
            .manager
            .captured_ops()
            .into_iter()
            .find(|entry| *entry == expected);
        assert_eq!(captured, Some(expected));
    }

    #[tokio::test]
    async fn spawn_agent_creates_session_and_sends_prompt() {
        let harness = AgentControlHarness::new().await;
        let session_id = harness
            .control
            .spawn_agent(harness.config.clone(), "spawned".to_string(), None)
            .await
            .expect("spawn_agent should succeed");
        let _session = harness
            .manager
            .get_session(session_id)
            .await
            .expect("session should be registered");
        let expected = (
            session_id,
            Op::UserInput {
                items: vec![UserInput::Text {
                    text: "spawned".to_string(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            },
        );
        let captured = harness
            .manager
            .captured_ops()
            .into_iter()
            .find(|entry| *entry == expected);
        assert_eq!(captured, Some(expected));
    }

    #[tokio::test]
    async fn spawn_agent_respects_max_sessions_limit() {
        let max_sessions = 1usize;
        let (_home, config) = test_config_with_cli_overrides(vec![(
            "agents.max_sessions".to_string(),
            TomlValue::Integer(max_sessions as i64),
        )])
        .await;
        let manager = SessionManager::with_models_provider_and_home(
            SavfoxAuth::from_api_key("dummy"),
            config.model_provider.clone(),
            config.savfox_home.clone(),
        );
        let control = manager.agent_control();

        let _ = manager
            .start_session(config.clone())
            .await
            .expect("start session");

        let first_agent_id = control
            .spawn_agent(config.clone(), "hello".to_string(), None)
            .await
            .expect("spawn_agent should succeed");

        let err = control
            .spawn_agent(config, "hello again".to_string(), None)
            .await
            .expect_err("spawn_agent should respect max sessions");
        let SavfoxError::AgentLimitReached {
            max_sessions: seen_max_sessions,
        } = err
        else {
            panic!("expected SavfoxError::AgentLimitReached");
        };
        assert_eq!(seen_max_sessions, max_sessions);

        let _ = control
            .shutdown_agent(first_agent_id)
            .await
            .expect("shutdown agent");
    }

    #[tokio::test]
    async fn spawn_agent_releases_slot_after_shutdown() {
        let max_sessions = 1usize;
        let (_home, config) = test_config_with_cli_overrides(vec![(
            "agents.max_sessions".to_string(),
            TomlValue::Integer(max_sessions as i64),
        )])
        .await;
        let manager = SessionManager::with_models_provider_and_home(
            SavfoxAuth::from_api_key("dummy"),
            config.model_provider.clone(),
            config.savfox_home.clone(),
        );
        let control = manager.agent_control();

        let first_agent_id = control
            .spawn_agent(config.clone(), "hello".to_string(), None)
            .await
            .expect("spawn_agent should succeed");
        let _ = control
            .shutdown_agent(first_agent_id)
            .await
            .expect("shutdown agent");

        let second_agent_id = control
            .spawn_agent(config.clone(), "hello again".to_string(), None)
            .await
            .expect("spawn_agent should succeed after shutdown");
        let _ = control
            .shutdown_agent(second_agent_id)
            .await
            .expect("shutdown agent");
    }

    #[tokio::test]
    async fn spawn_agent_limit_shared_across_clones() {
        let max_sessions = 1usize;
        let (_home, config) = test_config_with_cli_overrides(vec![(
            "agents.max_sessions".to_string(),
            TomlValue::Integer(max_sessions as i64),
        )])
        .await;
        let manager = SessionManager::with_models_provider_and_home(
            SavfoxAuth::from_api_key("dummy"),
            config.model_provider.clone(),
            config.savfox_home.clone(),
        );
        let control = manager.agent_control();
        let cloned = control.clone();

        let first_agent_id = cloned
            .spawn_agent(config.clone(), "hello".to_string(), None)
            .await
            .expect("spawn_agent should succeed");

        let err = control
            .spawn_agent(config, "hello again".to_string(), None)
            .await
            .expect_err("spawn_agent should respect shared guard");
        let SavfoxError::AgentLimitReached { max_sessions } = err else {
            panic!("expected SavfoxError::AgentLimitReached");
        };
        assert_eq!(max_sessions, 1);

        let _ = control
            .shutdown_agent(first_agent_id)
            .await
            .expect("shutdown agent");
    }
}
