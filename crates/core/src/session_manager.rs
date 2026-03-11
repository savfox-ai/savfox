use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use savfox_protocol::SessionId;
use savfox_protocol::config_types::CollaborationModeMask;
use savfox_protocol::openai_models::ModelPreset;
use savfox_protocol::protocol::{
    InitialHistory, McpServerRefreshConfig, Op, RolloutItem, SessionSource,
};
#[cfg(any(test, feature = "test-support"))]
use tempfile::TempDir;
use tokio::sync::{RwLock, broadcast};
use tracing::warn;

use crate::AuthManager;
#[cfg(any(test, feature = "test-support"))]
use crate::ModelProviderInfo;
#[cfg(any(test, feature = "test-support"))]
use crate::SavfoxAuth;
use crate::agent::AgentControl;
use crate::config::Config;
use crate::error::{Result as SavfoxResult, SavfoxError};
use crate::models_manager::manager::ModelsManager;
use crate::protocol::{Event, EventMsg, SessionConfiguredEvent};
use crate::rollout::{RolloutRecorder, truncation};
use crate::savfox::{INITIAL_SUBMIT_ID, Savfox, SavfoxSpawnOk};
use crate::savfox_session::SavfoxSession;
use crate::skills::SkillsManager;

const THREAD_CREATED_CHANNEL_CAPACITY: usize = 1024;

/// Represents a newly created Savfox session (formerly called a session/conversation), including
/// the first event (which is [`EventMsg::SessionConfigured`]).
pub struct NewSession {
    pub session_id: SessionId,
    pub session: Arc<SavfoxSession>,
    pub session_configured: SessionConfiguredEvent,
}

/// [`SessionManager`] is responsible for creating sessions (formerly called sessions) and
/// maintaining them in memory.
pub struct SessionManager {
    state: Arc<SessionManagerState>,
    #[cfg(any(test, feature = "test-support"))]
    _test_savfox_home_guard: Option<TempDir>,
}

/// Shared, `Arc`-owned state for [`SessionManager`]. This `Arc` is required to have a single
/// `Arc` reference that can be downgraded to by `AgentControl` while preventing every single
/// function to require an `Arc<&Self>`.
pub(crate) struct SessionManagerState {
    sessions: Arc<RwLock<HashMap<SessionId, Arc<SavfoxSession>>>>,
    session_created_tx: broadcast::Sender<SessionId>,
    auth_manager: Arc<AuthManager>,
    models_manager: Arc<ModelsManager>,
    skills_manager: Arc<SkillsManager>,
    session_source: SessionSource,
    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    // Captures submitted ops for testing purpose.
    ops_log: Arc<std::sync::Mutex<Vec<(SessionId, Op)>>>,
}

impl SessionManager {
    pub fn new(
        savfox_home: PathBuf,
        auth_manager: Arc<AuthManager>,
        session_source: SessionSource,
    ) -> Self {
        let (session_created_tx, _) = broadcast::channel(THREAD_CREATED_CHANNEL_CAPACITY);
        Self {
            state: Arc::new(SessionManagerState {
                sessions: Arc::new(RwLock::new(HashMap::new())),
                session_created_tx,
                models_manager: Arc::new(ModelsManager::new(
                    savfox_home.clone(),
                    auth_manager.clone(),
                )),
                skills_manager: Arc::new(SkillsManager::new(savfox_home)),
                auth_manager,
                session_source,
                #[cfg(any(test, feature = "test-support"))]
                ops_log: Arc::new(std::sync::Mutex::new(Vec::new())),
            }),
            #[cfg(any(test, feature = "test-support"))]
            _test_savfox_home_guard: None,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Construct with a dummy AuthManager containing the provided SavfoxAuth.
    /// Used for integration tests: should not be used by ordinary business logic.
    pub fn with_models_provider(auth: SavfoxAuth, provider: ModelProviderInfo) -> Self {
        let temp_dir = tempfile::tempdir().unwrap_or_else(|err| panic!("temp savfox home: {err}"));
        let savfox_home = temp_dir.path().to_path_buf();
        let mut manager = Self::with_models_provider_and_home(auth, provider, savfox_home);
        manager._test_savfox_home_guard = Some(temp_dir);
        manager
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Construct with a dummy AuthManager containing the provided SavfoxAuth and savfox home.
    /// Used for integration tests: should not be used by ordinary business logic.
    pub fn with_models_provider_and_home(
        auth: SavfoxAuth,
        provider: ModelProviderInfo,
        savfox_home: PathBuf,
    ) -> Self {
        let auth_manager = AuthManager::from_auth_for_testing(auth);
        let (session_created_tx, _) = broadcast::channel(THREAD_CREATED_CHANNEL_CAPACITY);
        Self {
            state: Arc::new(SessionManagerState {
                sessions: Arc::new(RwLock::new(HashMap::new())),
                session_created_tx,
                models_manager: Arc::new(ModelsManager::with_provider(
                    savfox_home.clone(),
                    auth_manager.clone(),
                    provider,
                )),
                skills_manager: Arc::new(SkillsManager::new(savfox_home)),
                auth_manager,
                session_source: SessionSource::Exec,
                #[cfg(any(test, feature = "test-support"))]
                ops_log: Arc::new(std::sync::Mutex::new(Vec::new())),
            }),
            _test_savfox_home_guard: None,
        }
    }

    pub fn session_source(&self) -> SessionSource {
        self.state.session_source.clone()
    }

    pub fn skills_manager(&self) -> Arc<SkillsManager> {
        self.state.skills_manager.clone()
    }

    pub fn get_models_manager(&self) -> Arc<ModelsManager> {
        self.state.models_manager.clone()
    }

    pub async fn list_models(
        &self,
        config: &Config,
        refresh_strategy: crate::models_manager::manager::RefreshStrategy,
    ) -> Vec<ModelPreset> {
        self.state
            .models_manager
            .list_models(config, refresh_strategy)
            .await
    }

    pub fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        self.state.models_manager.list_collaboration_modes()
    }

    pub async fn list_session_ids(&self) -> Vec<SessionId> {
        self.state.sessions.read().await.keys().copied().collect()
    }

    pub async fn refresh_mcp_servers(&self, refresh_config: McpServerRefreshConfig) {
        let sessions = self
            .state
            .sessions
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for session in sessions {
            if let Err(err) = session
                .submit(Op::RefreshMcpServers {
                    config: refresh_config.clone(),
                })
                .await
            {
                warn!("failed to request MCP server refresh: {err}");
            }
        }
    }

    pub fn subscribe_session_created(&self) -> broadcast::Receiver<SessionId> {
        self.state.session_created_tx.subscribe()
    }

    pub async fn get_session(&self, session_id: SessionId) -> SavfoxResult<Arc<SavfoxSession>> {
        self.state.get_session(session_id).await
    }

    pub async fn start_session(&self, config: Config) -> SavfoxResult<NewSession> {
        self.start_session_with_tools(config, Vec::new()).await
    }

    pub async fn start_session_with_tools(
        &self,
        config: Config,
        dynamic_tools: Vec<savfox_protocol::dynamic_tools::DynamicToolSpec>,
    ) -> SavfoxResult<NewSession> {
        self.state
            .spawn_session(
                config,
                InitialHistory::New,
                Arc::clone(&self.state.auth_manager),
                self.agent_control(),
                dynamic_tools,
            )
            .await
    }

    pub async fn resume_session_from_rollout(
        &self,
        config: Config,
        rollout_path: PathBuf,
        auth_manager: Arc<AuthManager>,
    ) -> SavfoxResult<NewSession> {
        let initial_history = RolloutRecorder::get_rollout_history(&rollout_path).await?;
        self.resume_session_with_history(config, initial_history, auth_manager)
            .await
    }

    pub async fn resume_session_with_history(
        &self,
        config: Config,
        initial_history: InitialHistory,
        auth_manager: Arc<AuthManager>,
    ) -> SavfoxResult<NewSession> {
        self.state
            .spawn_session(
                config,
                initial_history,
                auth_manager,
                self.agent_control(),
                Vec::new(),
            )
            .await
    }

    /// Removes the session from the manager's internal map, though the session is stored
    /// as `Arc<SavfoxSession>`, it is possible that other references to it exist elsewhere.
    /// Returns the session if the session was found and removed.
    pub async fn remove_session(&self, session_id: &SessionId) -> Option<Arc<SavfoxSession>> {
        self.state.sessions.write().await.remove(session_id)
    }

    /// Closes all sessions open in this SessionManager
    pub async fn remove_and_close_all_sessions(&self) -> SavfoxResult<()> {
        for session in self.state.sessions.read().await.values() {
            session.submit(Op::Shutdown).await?;
        }
        self.state.sessions.write().await.clear();
        Ok(())
    }

    /// Fork an existing session by taking messages up to the given position (not including
    /// the message at the given position) and starting a new session with identical
    /// configuration (unless overridden by the caller's `config`). The new session will have
    /// a fresh id. Pass `usize::MAX` to keep the full rollout history.
    pub async fn fork_session(
        &self,
        nth_user_message: usize,
        config: Config,
        path: PathBuf,
    ) -> SavfoxResult<NewSession> {
        let history = RolloutRecorder::get_rollout_history(&path).await?;
        let history = truncate_before_nth_user_message(history, nth_user_message);
        self.state
            .spawn_session(
                config,
                history,
                Arc::clone(&self.state.auth_manager),
                self.agent_control(),
                Vec::new(),
            )
            .await
    }

    pub(crate) fn agent_control(&self) -> AgentControl {
        AgentControl::new(Arc::downgrade(&self.state))
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn captured_ops(&self) -> Vec<(SessionId, Op)> {
        self.state
            .ops_log
            .lock()
            .map(|log| log.clone())
            .unwrap_or_default()
    }
}

impl SessionManagerState {
    /// Fetch a session by ID or return SessionNotFound.
    pub(crate) async fn get_session(
        &self,
        session_id: SessionId,
    ) -> SavfoxResult<Arc<SavfoxSession>> {
        let sessions = self.sessions.read().await;
        sessions
            .get(&session_id)
            .cloned()
            .ok_or_else(|| SavfoxError::SessionNotFound(session_id))
    }

    /// Send an operation to a session by ID.
    pub(crate) async fn send_op(&self, session_id: SessionId, op: Op) -> SavfoxResult<String> {
        let session = self.get_session(session_id).await?;
        #[cfg(any(test, feature = "test-support"))]
        {
            if let Ok(mut log) = self.ops_log.lock() {
                log.push((session_id, op.clone()));
            }
        }
        session.submit(op).await
    }

    /// Remove a session from the manager by ID, returning it when present.
    pub(crate) async fn remove_session(
        &self,
        session_id: &SessionId,
    ) -> Option<Arc<SavfoxSession>> {
        self.sessions.write().await.remove(session_id)
    }

    /// Spawn a new session with no history using a provided config.
    pub(crate) async fn spawn_new_session(
        &self,
        config: Config,
        agent_control: AgentControl,
    ) -> SavfoxResult<NewSession> {
        self.spawn_new_session_with_source(config, agent_control, self.session_source.clone())
            .await
    }

    pub(crate) async fn spawn_new_session_with_source(
        &self,
        config: Config,
        agent_control: AgentControl,
        session_source: SessionSource,
    ) -> SavfoxResult<NewSession> {
        self.spawn_session_with_source(
            config,
            InitialHistory::New,
            Arc::clone(&self.auth_manager),
            agent_control,
            session_source,
            Vec::new(),
        )
        .await
    }

    /// Spawn a new session with optional history and register it with the manager.
    pub(crate) async fn spawn_session(
        &self,
        config: Config,
        initial_history: InitialHistory,
        auth_manager: Arc<AuthManager>,
        agent_control: AgentControl,
        dynamic_tools: Vec<savfox_protocol::dynamic_tools::DynamicToolSpec>,
    ) -> SavfoxResult<NewSession> {
        self.spawn_session_with_source(
            config,
            initial_history,
            auth_manager,
            agent_control,
            self.session_source.clone(),
            dynamic_tools,
        )
        .await
    }

    pub(crate) async fn spawn_session_with_source(
        &self,
        config: Config,
        initial_history: InitialHistory,
        auth_manager: Arc<AuthManager>,
        agent_control: AgentControl,
        session_source: SessionSource,
        dynamic_tools: Vec<savfox_protocol::dynamic_tools::DynamicToolSpec>,
    ) -> SavfoxResult<NewSession> {
        let SavfoxSpawnOk {
            savfox, session_id, ..
        } = Savfox::spawn(
            config,
            auth_manager,
            Arc::clone(&self.models_manager),
            Arc::clone(&self.skills_manager),
            initial_history,
            session_source,
            agent_control,
            dynamic_tools,
        )
        .await?;
        self.finalize_session_spawn(savfox, session_id).await
    }

    async fn finalize_session_spawn(
        &self,
        savfox: Savfox,
        session_id: SessionId,
    ) -> SavfoxResult<NewSession> {
        let event = savfox.next_event().await?;
        let session_configured = match event {
            Event {
                id,
                msg: EventMsg::SessionConfigured(session_configured),
            } if id == INITIAL_SUBMIT_ID => session_configured,
            _ => {
                return Err(SavfoxError::SessionConfiguredNotFirstEvent);
            }
        };

        let session = Arc::new(SavfoxSession::new(
            savfox,
            session_configured.rollout_path.clone(),
        ));
        let mut sessions = self.sessions.write().await;
        sessions.insert(session_id, session.clone());

        Ok(NewSession {
            session_id,
            session,
            session_configured,
        })
    }

    pub(crate) fn notify_session_created(&self, session_id: SessionId) {
        let _ = self.session_created_tx.send(session_id);
    }
}

/// Return a prefix of `items` obtained by cutting strictly before the nth user message
/// (0-based) and all items that follow it.
fn truncate_before_nth_user_message(history: InitialHistory, n: usize) -> InitialHistory {
    let items: Vec<RolloutItem> = history.get_rollout_items();
    let rolled = truncation::truncate_rollout_before_nth_user_message_from_start(&items, n);

    if rolled.is_empty() {
        InitialHistory::New
    } else {
        InitialHistory::Forked(rolled)
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use pretty_assertions::assert_eq;
    use savfox_protocol::models::{ContentItem, ReasoningItemReasoningSummary, ResponseItem};

    use super::*;
    use crate::savfox::make_session_and_context;

    fn user_msg(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }
    fn assistant_msg(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    #[test]
    fn drops_from_last_user_only() {
        let items = [
            user_msg("u1"),
            assistant_msg("a1"),
            assistant_msg("a2"),
            user_msg("u2"),
            assistant_msg("a3"),
            ResponseItem::Reasoning {
                id: "r1".to_string(),
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "s".to_string(),
                }],
                content: None,
                encrypted_content: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                call_id: "c1".to_string(),
                name: "tool".to_string(),
                arguments: "{}".to_string(),
            },
            assistant_msg("a4"),
        ];

        let initial: Vec<RolloutItem> = items
            .iter()
            .cloned()
            .map(RolloutItem::ResponseItem)
            .collect();
        let truncated = truncate_before_nth_user_message(InitialHistory::Forked(initial), 1);
        let got_items = truncated.get_rollout_items();
        let expected_items = vec![
            RolloutItem::ResponseItem(items[0].clone()),
            RolloutItem::ResponseItem(items[1].clone()),
            RolloutItem::ResponseItem(items[2].clone()),
        ];
        assert_eq!(
            serde_json::to_value(&got_items).unwrap(),
            serde_json::to_value(&expected_items).unwrap()
        );

        let initial2: Vec<RolloutItem> = items
            .iter()
            .cloned()
            .map(RolloutItem::ResponseItem)
            .collect();
        let truncated2 = truncate_before_nth_user_message(InitialHistory::Forked(initial2), 2);
        assert_matches!(truncated2, InitialHistory::New);
    }

    #[tokio::test]
    async fn ignores_session_prefix_messages_when_truncating() {
        let (session, turn_context) = make_session_and_context().await;
        let mut items = session.build_initial_context(&turn_context).await;
        items.push(user_msg("feature request"));
        items.push(assistant_msg("ack"));
        items.push(user_msg("second question"));
        items.push(assistant_msg("answer"));

        let rollout_items: Vec<RolloutItem> = items
            .iter()
            .cloned()
            .map(RolloutItem::ResponseItem)
            .collect();

        let truncated = truncate_before_nth_user_message(InitialHistory::Forked(rollout_items), 1);
        let got_items = truncated.get_rollout_items();

        let expected: Vec<RolloutItem> = vec![
            RolloutItem::ResponseItem(items[0].clone()),
            RolloutItem::ResponseItem(items[1].clone()),
            RolloutItem::ResponseItem(items[2].clone()),
            RolloutItem::ResponseItem(items[3].clone()),
        ];

        assert_eq!(
            serde_json::to_value(&got_items).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
    }
}
