use std::path::PathBuf;

use savfox_protocol::config_types::Personality;
use savfox_protocol::openai_models::ReasoningEffort;
use savfox_protocol::protocol::{AskForApproval, SandboxPolicy, SessionSource};
use tokio::sync::watch;

use crate::agent::AgentStatus;
use crate::error::Result as SavfoxResult;
use crate::protocol::{Event, Op, Submission};
use crate::savfox::Savfox;
use crate::state_db::StateDbHandle;

#[derive(Clone, Debug)]
pub struct SessionConfigSnapshot {
    pub model: String,
    pub model_provider_id: String,
    pub approval_policy: AskForApproval,
    pub sandbox_policy: SandboxPolicy,
    pub cwd: PathBuf,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub personality: Option<Personality>,
    pub session_source: SessionSource,
}

pub struct SavfoxSession {
    savfox: Savfox,
    rollout_path: Option<PathBuf>,
}

/// Conduit for the bidirectional stream of messages that compose a session
/// (formerly called a conversation) in Savfox.
impl SavfoxSession {
    pub(crate) fn new(savfox: Savfox, rollout_path: Option<PathBuf>) -> Self {
        Self {
            savfox,
            rollout_path,
        }
    }

    pub async fn submit(&self, op: Op) -> SavfoxResult<String> {
        self.savfox.submit(op).await
    }

    /// Use sparingly: this is intended to be removed soon.
    pub async fn submit_with_id(&self, sub: Submission) -> SavfoxResult<()> {
        self.savfox.submit_with_id(sub).await
    }

    pub async fn next_event(&self) -> SavfoxResult<Event> {
        self.savfox.next_event().await
    }

    pub async fn agent_status(&self) -> AgentStatus {
        self.savfox.agent_status().await
    }

    pub(crate) fn subscribe_status(&self) -> watch::Receiver<AgentStatus> {
        self.savfox.agent_status.clone()
    }

    pub fn rollout_path(&self) -> Option<PathBuf> {
        self.rollout_path.clone()
    }

    pub fn state_db(&self) -> Option<StateDbHandle> {
        self.savfox.state_db()
    }

    pub async fn config_snapshot(&self) -> SessionConfigSnapshot {
        self.savfox.session_config_snapshot().await
    }
}
