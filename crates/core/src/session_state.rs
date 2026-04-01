use std::path::PathBuf;

use savfox_protocol::config_types::Personality;
use savfox_protocol::openai_models::ReasoningEffort;
use savfox_protocol::protocol::{AskForApproval, SandboxPolicy, SessionSource};
use tokio::sync::watch;

use crate::agent::AgentStatus;
use crate::error::Result as SavfoxResult;
use crate::protocol::{Event, Op, Submission};
use crate::savfox::SessionHandle;
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
    handle: SessionHandle,
    rollout_path: Option<PathBuf>,
}

/// Conduit for the bidirectional stream of messages that compose a session
/// (formerly called a conversation) in Savfox.
impl SavfoxSession {
    pub(crate) fn new(handle: SessionHandle, rollout_path: Option<PathBuf>) -> Self {
        Self {
            handle,
            rollout_path,
        }
    }

    pub async fn submit(&self, op: Op) -> SavfoxResult<String> {
        self.handle.submit(op).await
    }

    /// Use sparingly: this is intended to be removed soon.
    pub async fn submit_with_id(&self, sub: Submission) -> SavfoxResult<()> {
        self.handle.submit_with_id(sub).await
    }

    pub async fn next_event(&self) -> SavfoxResult<Event> {
        self.handle.next_event().await
    }

    pub async fn maybe_submit_textual_approval(&self, text: &str) -> SavfoxResult<bool> {
        self.handle.maybe_submit_textual_approval(text).await
    }

    pub async fn agent_status(&self) -> AgentStatus {
        self.handle.agent_status().await
    }

    pub(crate) fn subscribe_status(&self) -> watch::Receiver<AgentStatus> {
        self.handle.agent_status.clone()
    }

    pub fn rollout_path(&self) -> Option<PathBuf> {
        self.rollout_path.clone()
    }

    pub fn state_db(&self) -> Option<StateDbHandle> {
        self.handle.state_db()
    }

    pub async fn config_snapshot(&self) -> SessionConfigSnapshot {
        self.handle.session_config_snapshot().await
    }
}
