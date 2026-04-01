use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use async_channel::{Receiver, Sender};
use futures::future::BoxFuture;
use futures::prelude::*;
use futures::stream::FuturesOrdered;
use rmcp::model::{
    ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResult, RequestId,
};
use savfox_async_utils::OrCancelExt;
use savfox_otel::OtelManager;
use savfox_protocol::SessionId;
use savfox_protocol::approvals::ExecPolicyAmendment;
use savfox_protocol::config_types::{
    CollaborationMode, ModeKind, Personality, ReasoningSummary as ReasoningSummaryConfig, Settings,
    WebSearchMode, WindowsSandboxLevel,
};
use savfox_protocol::dynamic_tools::{DynamicToolResponse, DynamicToolSpec};
use savfox_protocol::items::{PlanItem, TurnItem, UserMessageItem};
use savfox_protocol::mcp::CallToolResult;
use savfox_protocol::models::{
    BaseInstructions, ContentItem, DeveloperInstructions, ResponseInputItem, ResponseItem,
    format_allow_prefixes,
};
use savfox_protocol::openai_models::ModelInfo;
use savfox_protocol::protocol::{
    FileChange, HasLegacyEvent, InitialHistory, ItemCompletedEvent, ItemStartedEvent,
    RawResponseItemEvent, ReviewRequest, RolloutItem, SavfoxErrorInfo, SessionSource,
    SubAgentSource, TurnAbortReason, TurnContextItem, TurnStartedEvent,
};
use savfox_protocol::request_user_input::{RequestUserInputArgs, RequestUserInputResponse};
use savfox_protocol::user_input::UserInput;
use savfox_rmcp_client::{ElicitationResponse, OAuthCredentialsStoreMode};
use savfox_utils::readiness::{Readiness, ReadinessFlag};
use serde_json::{self, Value};
use tokio::sync::{Mutex, RwLock, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, field, info, info_span, instrument, trace_span, warn};

use crate::agent::{AgentControl, AgentStatus, MAX_THREAD_SPAWN_DEPTH, agent_status_from_event};
use crate::analytics_client::{AnalyticsEventsClient, build_track_events_context};
use crate::client::{ModelClient, ModelClientSession};
use crate::client_common::{Prompt, ResponseEvent};
use crate::compact::{
    collect_user_messages, run_inline_auto_compact_task, should_use_remote_compact_task,
};
use crate::compact_remote::run_inline_remote_auto_compact_task;
use crate::config::types::{McpServerConfig, ShellEnvironmentPolicy};
use crate::config::{
    Config, Constrained, ConstraintResult, GhostSnapshotConfig, resolve_web_search_mode_for_turn,
};
use crate::context_manager::ContextManager;
use crate::environment_context::EnvironmentContext;
use crate::error::{Result as SavfoxResult, SavfoxError};
#[cfg(test)]
use crate::exec::StreamOutput;
use crate::exec_policy::{ExecPolicyManager, ExecPolicyUpdateError};
use crate::features::{Feature, Features, maybe_push_unstable_features_warning};
use crate::git_info::get_git_repo_root;
use crate::instructions::UserInstructions;
use crate::mcp::auth::compute_auth_statuses;
use crate::mcp::{
    SAVFOX_APPS_MCP_SERVER_NAME, effective_mcp_servers, maybe_prompt_and_install_mcp_dependencies,
    with_savfox_apps_mcp,
};
use crate::mcp_connection_manager::McpConnectionManager;
use crate::mentions::{
    build_connector_slug_counts, build_skill_name_counts, collect_explicit_app_paths,
    collect_tool_mentions_from_messages,
};
use crate::models_manager::manager::ModelsManager;
use crate::parse_command::parse_command;
use crate::project_doc::get_user_instructions;
use crate::proposed_plan_parser::{
    ProposedPlanParser, ProposedPlanSegment, extract_proposed_plan_text,
};
use crate::protocol::{
    AgentMessageContentDeltaEvent, AgentReasoningSectionBreakEvent, ApplyPatchApprovalRequestEvent,
    AskForApproval, BackgroundEventEvent, DeprecationNoticeEvent, ErrorEvent, Event, EventMsg,
    ExecApprovalRequestEvent, McpServerRefreshConfig, Op, PlanDeltaEvent, RateLimitSnapshot,
    ReasoningContentDeltaEvent, ReasoningRawContentDeltaEvent, RequestUserInputEvent,
    ReviewDecision, SandboxPolicy, SessionConfiguredEvent,
    SkillDependencies as ProtocolSkillDependencies, SkillErrorInfo,
    SkillInterface as ProtocolSkillInterface, SkillMetadata as ProtocolSkillMetadata,
    SkillToolDependency as ProtocolSkillToolDependency, StreamErrorEvent, Submission,
    TokenCountEvent, TokenUsage, TokenUsageInfo, TurnDiffEvent, WarningEvent,
};
use crate::rollout::{
    RolloutRecorder, RolloutRecorderParams, map_session_init_error, metadata, session_index,
};
use crate::savfox_session::SessionConfigSnapshot;
use crate::shell_snapshot::ShellSnapshot;
use crate::skills::injection::{ToolMentionKind, app_id_from_path, tool_kind_for_path};
use crate::skills::{
    SkillError, SkillInjections, SkillMetadata, SkillsManager, build_skill_injections,
    collect_env_var_dependencies, collect_explicit_skill_mentions,
    resolve_skill_dependencies_for_turn,
};
use crate::state::{ActiveTurn, SessionServices, SessionState};
use crate::stream_events_utils::{
    HandleOutputCtx, handle_non_tool_response_item, handle_output_item_done,
    last_assistant_message_from_item,
};
use crate::tasks::{GhostSnapshotTask, ReviewTask, SessionTask, SessionTaskContext};
use crate::tools::ToolRouter;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::sandboxing::ApprovalStore;
use crate::tools::spec::{ToolsConfig, ToolsConfigParams};
use crate::transport_manager::TransportManager;
use crate::truncate::TruncationPolicy;
use crate::turn_diff_tracker::TurnDiffTracker;
use crate::unified_exec::UnifiedExecProcessManager;
use crate::user_notification::{UserNotification, UserNotifier};
use crate::util::{backoff, error_or_panic};
use crate::windows_sandbox::WindowsSandboxLevelExt;
use crate::{
    AuthManager, ModelProviderInfo, SandboxState, SavfoxAuth, compact, connectors, feedback_tags,
    parse_turn_item, shell, state_db, terminal,
};

// Sub-modules
mod event_pipeline;
mod mcp_lifecycle;
mod session_lifecycle;
mod skill_injector;
mod task_coordinator;
mod tool_orchestrator;

// Re-export everything that was previously accessible from this module
pub(crate) use task_coordinator::spawn_review_session;
pub(crate) use tool_orchestrator::{get_last_assistant_message_from_turn, run_turn};

/// Handle for interacting with a running agent session.
/// Operates as a queue pair: send submissions and receive events.
pub struct SessionHandle {
    pub(crate) next_id: AtomicU64,
    pub(crate) tx_sub: Sender<Submission>,
    pub(crate) rx_event: Receiver<Event>,
    // Last known status of the agent.
    pub(crate) agent_status: watch::Receiver<AgentStatus>,
    pub(crate) session: Arc<Session>,
}

/// Result of [`SessionHandle::spawn`] containing the spawned handle and
/// the unique session id.
pub struct SessionSpawnResult {
    pub handle: SessionHandle,
    pub session_id: SessionId,
}

pub(crate) const INITIAL_SUBMIT_ID: &str = "";
pub(crate) const SUBMISSION_CHANNEL_CAPACITY: usize = 64;

fn maybe_push_chat_wire_api_deprecation(
    _config: &Config,
    _post_session_configured_events: &mut Vec<Event>,
) {
    // if config.model_provider.wire_api != WireApi::Chat {
    //     return;
    // }

    // if CHAT_WIRE_API_DEPRECATION_EMITTED
    //     .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
    //     .is_err()
    // {
    //     return;
    // }

    // post_session_configured_events.push(Event {
    //     id: INITIAL_SUBMIT_ID.to_owned(),
    //     msg: EventMsg::DeprecationNotice(DeprecationNoticeEvent {
    //         summary: CHAT_WIRE_API_DEPRECATION_SUMMARY.to_string(),
    //         details: None,
    //     }),
    // });
}

impl SessionHandle {
    /// Submit the `op` wrapped in a `Submission` with a unique ID.
    pub async fn submit(&self, op: Op) -> SavfoxResult<String> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .to_string();
        let sub = Submission { id: id.clone(), op };
        self.submit_with_id(sub).await?;
        Ok(id)
    }

    /// Use sparingly: prefer `submit()` so the session handle is responsible for generating
    /// unique IDs for each submission.
    pub async fn submit_with_id(&self, sub: Submission) -> SavfoxResult<()> {
        self.tx_sub
            .send(sub)
            .await
            .map_err(|_| SavfoxError::InternalAgentDied)?;
        Ok(())
    }

    pub async fn next_event(&self) -> SavfoxResult<Event> {
        let event = self
            .rx_event
            .recv()
            .await
            .map_err(|_| SavfoxError::InternalAgentDied)?;
        Ok(event)
    }

    pub async fn maybe_submit_textual_approval(&self, text: &str) -> SavfoxResult<bool> {
        let Some(reply) = parse_textual_approval_reply(text) else {
            return Ok(false);
        };

        let pending_id = {
            let active = self.session.active_turn.lock().await;
            match active.as_ref() {
                Some(at) => {
                    let ts = at.turn_state.lock().await;
                    match reply.target_id.as_deref() {
                        Some(id) if ts.has_pending_approval(id) => Some(id.to_string()),
                        Some(_) => None,
                        None => ts.pending_approval_key_if_unambiguous(),
                    }
                }
                None => None,
            }
        };

        let Some(id) = pending_id else {
            return Ok(false);
        };

        self.submit(Op::ExecApproval {
            id,
            decision: reply.decision,
        })
        .await?;
        Ok(true)
    }

    pub(crate) async fn agent_status(&self) -> AgentStatus {
        self.agent_status.borrow().clone()
    }

    pub(crate) async fn session_config_snapshot(&self) -> SessionConfigSnapshot {
        let state = self.session.state.lock().await;
        state.session_configuration.session_config_snapshot()
    }

    pub(crate) fn state_db(&self) -> Option<state_db::StateDbHandle> {
        self.session.state_db()
    }
}

struct ParsedApprovalReply {
    decision: ReviewDecision,
    target_id: Option<String>,
}

fn parse_approval_command_id<'a>(text: &'a str, command: &str) -> Option<&'a str> {
    let (head, tail) = text.split_once(':')?;
    if head.eq_ignore_ascii_case(command) {
        let id = tail.trim();
        if !id.is_empty() {
            return Some(id);
        }
    }
    None
}

fn parse_textual_approval_reply(text: &str) -> Option<ParsedApprovalReply> {
    let normalized = text.trim();
    match normalized {
        "+" => {
            return Some(ParsedApprovalReply {
                decision: ReviewDecision::Approved,
                target_id: None,
            });
        }
        "-" => {
            return Some(ParsedApprovalReply {
                decision: ReviewDecision::Denied,
                target_id: None,
            });
        }
        _ => {}
    }

    if let Some(id) = parse_approval_command_id(normalized, "approve") {
        return Some(ParsedApprovalReply {
            decision: ReviewDecision::Approved,
            target_id: Some(id.to_string()),
        });
    }

    if let Some(id) = parse_approval_command_id(normalized, "deny") {
        return Some(ParsedApprovalReply {
            decision: ReviewDecision::Denied,
            target_id: Some(id.to_string()),
        });
    }

    None
}

/// Context for an initialized model agent
///
/// A session has at most 1 running task at a time, and can be interrupted by user input.
pub(crate) struct Session {
    pub(crate) conversation_id: SessionId,
    tx_event: Sender<Event>,
    agent_status: watch::Sender<AgentStatus>,
    state: Mutex<SessionState>,
    /// The set of enabled features should be invariant for the lifetime of the
    /// session.
    features: Features,
    pending_mcp_server_refresh_config: Mutex<Option<McpServerRefreshConfig>>,
    pub(crate) active_turn: Mutex<Option<ActiveTurn>>,
    pub(crate) services: SessionServices,
    next_internal_sub_id: AtomicU64,
}

/// The context needed for a single turn of the session.
#[derive(Debug)]
pub(crate) struct TurnContext {
    pub(crate) sub_id: String,
    pub(crate) client: ModelClient,
    /// The session's current working directory. All relative paths provided by
    /// the model as well as sandbox policies are resolved against this path
    /// instead of `std::env::current_dir()`.
    pub(crate) cwd: PathBuf,
    pub(crate) developer_instructions: Option<String>,
    pub(crate) compact_prompt: Option<String>,
    pub(crate) user_instructions: Option<String>,
    pub(crate) collaboration_mode: CollaborationMode,
    pub(crate) personality: Option<Personality>,
    pub(crate) approval_policy: AskForApproval,
    pub(crate) sandbox_policy: SandboxPolicy,
    pub(crate) windows_sandbox_level: WindowsSandboxLevel,
    pub(crate) shell_environment_policy: ShellEnvironmentPolicy,
    pub(crate) tools_config: ToolsConfig,
    pub(crate) ghost_snapshot: GhostSnapshotConfig,
    pub(crate) final_output_json_schema: Option<Value>,
    pub(crate) savfox_linux_sandbox_exe: Option<PathBuf>,
    pub(crate) tool_call_gate: Arc<ReadinessFlag>,
    pub(crate) truncation_policy: TruncationPolicy,
    pub(crate) dynamic_tools: Vec<DynamicToolSpec>,
    /// Tool access policy from the unified permission system.
    pub(crate) tool_access_policy: Option<savfox_protocol::protocol::ToolAccessPolicy>,
}
impl TurnContext {
    pub(crate) fn resolve_path(&self, path: Option<String>) -> PathBuf {
        path.as_ref()
            .map(PathBuf::from)
            .map_or_else(|| self.cwd.clone(), |p| self.cwd.join(p))
    }

    pub(crate) fn compact_prompt(&self) -> &str {
        self.compact_prompt
            .as_deref()
            .unwrap_or(compact::SUMMARIZATION_PROMPT)
    }
}

#[derive(Clone)]
pub(crate) struct SessionConfiguration {
    /// Provider identifier ("openai", "openrouter", ...).
    provider: ModelProviderInfo,

    collaboration_mode: CollaborationMode,
    model_reasoning_summary: ReasoningSummaryConfig,

    /// Developer instructions that supplement the base instructions.
    developer_instructions: Option<String>,

    /// Model instructions that are appended to the base instructions.
    user_instructions: Option<String>,

    /// Personality preference for the model.
    personality: Option<Personality>,

    /// Base instructions for the session.
    base_instructions: String,

    /// Compact prompt override.
    compact_prompt: Option<String>,

    /// When to escalate for approval for execution
    approval_policy: Constrained<AskForApproval>,
    /// How to sandbox commands executed in the system
    sandbox_policy: Constrained<SandboxPolicy>,
    windows_sandbox_level: WindowsSandboxLevel,

    /// Working directory that should be treated as the *root* of the
    /// session. All relative paths supplied by the model as well as the
    /// execution sandbox are resolved against this directory **instead**
    /// of the process-wide current working directory. CLI front-ends are
    /// expected to expand this to an absolute path before sending the
    /// `ConfigureSession` operation so that the business-logic layer can
    /// operate deterministically.
    cwd: PathBuf,
    /// Directory containing all Savfox state for this session.
    savfox_home: PathBuf,
    /// Optional user-facing name for the session, updated during the session.
    session_name: Option<String>,

    //  TODO(pakrym): Remove config from here
    original_config_do_not_use: Arc<Config>,
    /// Source of the session (cli, vscode, exec, mcp, ...)
    session_source: SessionSource,
    dynamic_tools: Vec<DynamicToolSpec>,
    /// Tool access policy from the unified permission system.
    tool_access_policy: Option<savfox_protocol::protocol::ToolAccessPolicy>,
}

impl SessionConfiguration {
    pub(crate) fn savfox_home(&self) -> &PathBuf {
        &self.savfox_home
    }

    fn session_config_snapshot(&self) -> SessionConfigSnapshot {
        SessionConfigSnapshot {
            model: self.collaboration_mode.model().to_string(),
            model_provider_id: self.original_config_do_not_use.model_provider_id.clone(),
            approval_policy: self.approval_policy.value(),
            sandbox_policy: self.sandbox_policy.get().clone(),
            cwd: self.cwd.clone(),
            reasoning_effort: self.collaboration_mode.reasoning_effort(),
            personality: self.personality,
            session_source: self.session_source.clone(),
        }
    }

    pub(crate) fn apply(&self, updates: &SessionSettingsUpdate) -> ConstraintResult<Self> {
        let mut next_configuration = self.clone();
        if let Some(collaboration_mode) = updates.collaboration_mode.clone() {
            next_configuration.collaboration_mode = collaboration_mode;
            next_configuration.sync_provider_from_model_prefix();
        }
        if let Some(summary) = updates.reasoning_summary {
            next_configuration.model_reasoning_summary = summary;
        }
        if let Some(personality) = updates.personality {
            next_configuration.personality = Some(personality);
        }
        if let Some(approval_policy) = updates.approval_policy {
            next_configuration.approval_policy.set(approval_policy)?;
        }
        if let Some(sandbox_policy) = updates.sandbox_policy.clone() {
            next_configuration.sandbox_policy.set(sandbox_policy)?;
        }
        if let Some(windows_sandbox_level) = updates.windows_sandbox_level {
            next_configuration.windows_sandbox_level = windows_sandbox_level;
        }
        if let Some(cwd) = updates.cwd.clone() {
            next_configuration.cwd = cwd;
        }
        if let Some(tool_access_policy) = updates.tool_access_policy.clone() {
            next_configuration.tool_access_policy = Some(tool_access_policy);
        }
        Ok(next_configuration)
    }

    fn sync_provider_from_model_prefix(&mut self) {
        let Some((provider_id, _)) =
            crate::parse_provider_prefixed_model(self.collaboration_mode.model())
        else {
            return;
        };

        let Some((resolved_provider_id, resolved_provider)) = self
            .original_config_do_not_use
            .model_providers
            .iter()
            .find(|(configured_id, _)| configured_id.eq_ignore_ascii_case(provider_id))
        else {
            return;
        };

        if self
            .original_config_do_not_use
            .model_provider_id
            .eq_ignore_ascii_case(resolved_provider_id)
        {
            return;
        }

        self.provider = resolved_provider.clone();
        let mut per_turn_config = (*self.original_config_do_not_use).clone();
        per_turn_config.model_provider_id = resolved_provider_id.clone();
        per_turn_config.model_provider = resolved_provider.clone();
        self.original_config_do_not_use = Arc::new(per_turn_config);
    }
}

#[derive(Default, Clone)]
pub(crate) struct SessionSettingsUpdate {
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) approval_policy: Option<AskForApproval>,
    pub(crate) sandbox_policy: Option<SandboxPolicy>,
    pub(crate) windows_sandbox_level: Option<WindowsSandboxLevel>,
    pub(crate) collaboration_mode: Option<CollaborationMode>,
    pub(crate) reasoning_summary: Option<ReasoningSummaryConfig>,
    pub(crate) final_output_json_schema: Option<Option<Value>>,
    pub(crate) personality: Option<Personality>,
    pub(crate) tool_access_policy: Option<savfox_protocol::protocol::ToolAccessPolicy>,
}

// Shared Session methods that don't fit a specific sub-module
impl Session {
    pub(crate) fn get_tx_event(&self) -> Sender<Event> {
        self.tx_event.clone()
    }

    pub(crate) fn state_db(&self) -> Option<state_db::StateDbHandle> {
        self.services.state_db.clone()
    }

    /// Ensure all rollout writes are durably flushed.
    pub(crate) async fn flush_rollout(&self) {
        let recorder = {
            let guard = self.services.rollout.lock().await;
            guard.clone()
        };
        if let Some(rec) = recorder
            && let Err(e) = rec.flush().await
        {
            warn!("failed to flush rollout recorder: {e}");
        }
    }

    fn next_internal_sub_id(&self) -> String {
        let id = self
            .next_internal_sub_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("auto-compact-{id}")
    }

    async fn get_total_token_usage(&self) -> i64 {
        let state = self.state.lock().await;
        state.get_total_token_usage(state.server_reasoning_included())
    }

    async fn get_estimated_token_count(&self, turn_context: &TurnContext) -> Option<i64> {
        let state = self.state.lock().await;
        state.history.estimate_token_count(turn_context)
    }

    pub(crate) async fn get_base_instructions(&self) -> BaseInstructions {
        let state = self.state.lock().await;
        BaseInstructions {
            text: state.session_configuration.base_instructions.clone(),
        }
    }

    pub fn enabled(&self, feature: Feature) -> bool {
        self.features.enabled(feature)
    }

    pub(crate) fn features(&self) -> Features {
        self.features.clone()
    }

    pub(crate) async fn collaboration_mode(&self) -> CollaborationMode {
        let state = self.state.lock().await;
        state.session_configuration.collaboration_mode.clone()
    }

    pub(crate) async fn set_server_reasoning_included(&self, included: bool) {
        let mut state = self.state.lock().await;
        state.set_server_reasoning_included(included);
    }

    pub(crate) async fn current_collaboration_mode(&self) -> CollaborationMode {
        let state = self.state.lock().await;
        state.session_configuration.collaboration_mode.clone()
    }

    async fn get_config(&self) -> std::sync::Arc<Config> {
        let state = self.state.lock().await;
        state
            .session_configuration
            .original_config_do_not_use
            .clone()
    }

    pub(crate) async fn savfox_home(&self) -> PathBuf {
        let state = self.state.lock().await;
        state.session_configuration.savfox_home().clone()
    }

    async fn turn_context_for_sub_id(&self, sub_id: &str) -> Option<Arc<TurnContext>> {
        let active = self.active_turn.lock().await;
        active
            .as_ref()
            .and_then(|turn| turn.tasks.get(sub_id))
            .map(|task| Arc::clone(&task.turn_context))
    }

    /// Adds an execpolicy amendment to both the in-memory and on-disk policies so future
    /// commands can use the newly approved prefix.
    pub(crate) async fn persist_execpolicy_amendment(
        &self,
        amendment: &ExecPolicyAmendment,
    ) -> Result<(), ExecPolicyUpdateError> {
        let features = self.features.clone();
        let savfox_home = self
            .state
            .lock()
            .await
            .session_configuration
            .savfox_home()
            .clone();

        if !features.enabled(Feature::ExecPolicy) {
            error!("attempted to append execpolicy rule while execpolicy feature is disabled");
            return Err(ExecPolicyUpdateError::FeatureDisabled);
        }

        self.services
            .exec_policy
            .append_amendment_and_update(&savfox_home, amendment)
            .await?;

        Ok(())
    }

    pub(crate) async fn record_execpolicy_amendment_message(
        &self,
        sub_id: &str,
        amendment: &ExecPolicyAmendment,
    ) {
        let Some(prefixes) = format_allow_prefixes(vec![amendment.command.clone()]) else {
            warn!("execpolicy amendment for {sub_id} had no command prefix");
            return;
        };
        let text = format!("Approved command prefix saved:\n{prefixes}");
        let message: ResponseItem = DeveloperInstructions::new(text.clone()).into();

        if let Some(turn_context) = self.turn_context_for_sub_id(sub_id).await {
            self.record_conversation_items(&turn_context, std::slice::from_ref(&message))
                .await;
            return;
        }

        if self
            .inject_response_items(vec![ResponseInputItem::Message {
                role: "developer".to_string(),
                content: vec![ContentItem::InputText { text }],
            }])
            .await
            .is_err()
        {
            warn!("no active turn found to record execpolicy amendment message for {sub_id}");
        }
    }

    /// Emit an exec approval request event and await the user's decision.
    ///
    /// The request is keyed by `sub_id`/`call_id` so matching responses are delivered
    /// to the correct in-flight turn. If the task is aborted, this returns the
    /// default `ReviewDecision` (`Denied`).
    #[allow(clippy::too_many_arguments)]
    pub async fn request_command_approval(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        command: Vec<String>,
        cwd: PathBuf,
        reason: Option<String>,
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    ) -> ReviewDecision {
        let sub_id = turn_context.sub_id.clone();
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
        let event_id = sub_id.clone();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_approval(sub_id, tx_approve)
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending approval for sub_id: {event_id}");
        }

        let parsed_cmd = parse_command(&command);
        let event = EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            command,
            cwd,
            reason,
            proposed_execpolicy_amendment,
            parsed_cmd,
        });
        self.send_event(turn_context, event).await;
        rx_approve.await.unwrap_or_default()
    }

    pub async fn request_patch_approval(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        changes: HashMap<PathBuf, FileChange>,
        reason: Option<String>,
        grant_root: Option<PathBuf>,
    ) -> oneshot::Receiver<ReviewDecision> {
        let sub_id = turn_context.sub_id.clone();
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
        let event_id = sub_id.clone();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_approval(sub_id, tx_approve)
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending approval for sub_id: {event_id}");
        }

        let event = EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            changes,
            reason,
            grant_root,
        });
        self.send_event(turn_context, event).await;
        rx_approve
    }

    pub async fn request_user_input(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        args: RequestUserInputArgs,
    ) -> Option<RequestUserInputResponse> {
        let sub_id = turn_context.sub_id.clone();
        let (tx_response, rx_response) = oneshot::channel();
        let event_id = sub_id.clone();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_user_input(sub_id, tx_response)
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending user input for sub_id: {event_id}");
        }

        let event = EventMsg::RequestUserInput(RequestUserInputEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            questions: args.questions,
        });
        self.send_event(turn_context, event).await;
        rx_response.await.ok()
    }

    pub async fn notify_user_input_response(
        &self,
        sub_id: &str,
        response: RequestUserInputResponse,
    ) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_user_input(sub_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_response) => {
                tx_response.send(response).ok();
            }
            None => {
                warn!("No pending user input found for sub_id: {sub_id}");
            }
        }
    }

    pub async fn notify_dynamic_tool_response(&self, call_id: &str, response: DynamicToolResponse) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_dynamic_tool(call_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_response) => {
                tx_response.send(response).ok();
            }
            None => {
                warn!("No pending dynamic tool call found for call_id: {call_id}");
            }
        }
    }

    pub async fn notify_approval(&self, sub_id: &str, decision: ReviewDecision) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_approval(sub_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_approve) => {
                tx_approve.send(decision).ok();
            }
            None => {
                warn!("No pending approval found for sub_id: {sub_id}");
            }
        }
    }

    pub async fn interrupt_task(self: &Arc<Self>) {
        info!("interrupt received: abort current task, if any");
        let has_active_turn = { self.active_turn.lock().await.is_some() };
        if has_active_turn {
            self.abort_all_tasks(TurnAbortReason::Interrupted).await;
        } else {
            self.cancel_mcp_startup().await;
        }
    }

    pub(crate) fn notifier(&self) -> &UserNotifier {
        &self.services.notifier
    }

    pub(crate) fn user_shell(&self) -> Arc<shell::Shell> {
        Arc::clone(&self.services.user_shell)
    }

    fn show_raw_agent_reasoning(&self) -> bool {
        self.services.show_raw_agent_reasoning
    }

    /// Returns the input if there was no task running to inject into
    pub async fn inject_input(&self, input: Vec<UserInput>) -> Result<(), Vec<UserInput>> {
        let mut active = self.active_turn.lock().await;
        match active.as_mut() {
            Some(at) => {
                let mut ts = at.turn_state.lock().await;
                ts.push_pending_input(input.into());
                Ok(())
            }
            None => Err(input),
        }
    }

    /// Returns the input if there was no task running to inject into
    pub async fn inject_response_items(
        &self,
        input: Vec<ResponseInputItem>,
    ) -> Result<(), Vec<ResponseInputItem>> {
        let mut active = self.active_turn.lock().await;
        match active.as_mut() {
            Some(at) => {
                let mut ts = at.turn_state.lock().await;
                for item in input {
                    ts.push_pending_input(item);
                }
                Ok(())
            }
            None => Err(input),
        }
    }

    pub async fn get_pending_input(&self) -> Vec<ResponseInputItem> {
        let mut active = self.active_turn.lock().await;
        match active.as_mut() {
            Some(at) => {
                let mut ts = at.turn_state.lock().await;
                ts.take_pending_input()
            }
            None => Vec::with_capacity(0),
        }
    }

    pub async fn has_pending_input(&self) -> bool {
        let active = self.active_turn.lock().await;
        match active.as_ref() {
            Some(at) => {
                let ts = at.turn_state.lock().await;
                ts.has_pending_input()
            }
            None => false,
        }
    }

    async fn maybe_start_ghost_snapshot(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        cancellation_token: CancellationToken,
    ) {
        if !self.enabled(Feature::GhostCommit) {
            return;
        }
        let token = match turn_context.tool_call_gate.subscribe().await {
            Ok(token) => token,
            Err(err) => {
                warn!("failed to subscribe to ghost snapshot readiness: {err}");
                return;
            }
        };

        info!("spawning ghost snapshot task");
        let task = GhostSnapshotTask::new(token);
        Arc::new(task)
            .run(
                Arc::new(SessionTaskContext::new(self.clone())),
                turn_context.clone(),
                Vec::new(),
                cancellation_token,
            )
            .await;
    }
}

#[cfg(test)]
pub(crate) use tests::make_session_and_context;
#[cfg(test)]
pub(crate) use tests::make_session_and_context_with_rx;

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Duration as StdDuration};

    use pretty_assertions::assert_eq;
    use savfox_app_server_protocol::{AppInfo, AuthMode};
    use savfox_protocol::SessionId;
    use savfox_protocol::mcp::CallToolResult as McpCallToolResult;
    use savfox_protocol::models::{ContentItem, FunctionCallOutputPayload, ResponseItem};
    use serde::Deserialize;
    use serde_json::json;
    use tokio::time::sleep;

    use super::*;
    use crate::SavfoxAuth;
    use crate::config::{ConfigBuilder, test_config};
    use crate::exec::ExecToolCallOutput;
    use crate::function_tool::FunctionCallError;
    use crate::protocol::{
        CompactedItem, CreditsSnapshot, InitialHistory, RateLimitSnapshot, RateLimitWindow,
        ResumedHistory, TokenCountEvent, TokenUsage, TokenUsageInfo,
    };
    use crate::shell::default_user_shell;
    use crate::state::TaskKind;
    use crate::tasks::{SessionTask, SessionTaskContext};
    use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
    use crate::tools::handlers::{ShellHandler, UnifiedExecHandler};
    use crate::tools::registry::ToolHandler;
    use crate::tools::{ToolRouter, format_exec_output_str};
    use crate::turn_diff_tracker::TurnDiffTracker;

    struct InstructionsTestCase {
        slug: &'static str,
        expects_apply_patch_instructions: bool,
    }

    fn user_message(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    fn make_connector(id: &str, name: &str) -> AppInfo {
        AppInfo {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            logo_url: None,
            logo_url_dark: None,
            distribution_channel: None,
            install_url: None,
            is_accessible: true,
        }
    }

    #[tokio::test]
    async fn get_base_instructions_no_user_content() {
        let test_cases = vec![
            InstructionsTestCase {
                slug: "gpt-3.5",
                expects_apply_patch_instructions: true,
            },
            InstructionsTestCase {
                slug: "gpt-4.1",
                expects_apply_patch_instructions: true,
            },
            InstructionsTestCase {
                slug: "gpt-4o",
                expects_apply_patch_instructions: true,
            },
            InstructionsTestCase {
                slug: "gpt-5",
                expects_apply_patch_instructions: true,
            },
            InstructionsTestCase {
                slug: "gpt-5.1",
                expects_apply_patch_instructions: false,
            },
            InstructionsTestCase {
                slug: "savfox-mini-latest",
                expects_apply_patch_instructions: true,
            },
            InstructionsTestCase {
                slug: "gpt-oss:120b",
                expects_apply_patch_instructions: false,
            },
            InstructionsTestCase {
                slug: "gpt-5.1-savfox",
                expects_apply_patch_instructions: false,
            },
            InstructionsTestCase {
                slug: "gpt-5.1-savfox-max",
                expects_apply_patch_instructions: false,
            },
        ];

        let (session, _turn_context) = make_session_and_context().await;

        for test_case in test_cases {
            let config = test_config();
            let model_info = ModelsManager::construct_model_info_offline(test_case.slug, &config);
            if test_case.expects_apply_patch_instructions {
                assert!(
                    model_info.base_instructions.contains("apply_patch"),
                    "expected apply_patch instructions for model {}",
                    test_case.slug
                );
            }

            {
                let mut state = session.state.lock().await;
                state.session_configuration.base_instructions =
                    model_info.base_instructions.clone();
            }

            let base_instructions = session.get_base_instructions().await;
            assert_eq!(base_instructions.text, model_info.base_instructions);
        }
    }

    #[test]
    fn filter_connectors_for_input_skips_duplicate_slug_mentions() {
        let connectors = vec![
            make_connector("one", "Foo Bar"),
            make_connector("two", "Foo-Bar"),
        ];
        let input = vec![user_message("use $foo-bar")];
        let explicit_app_paths = Vec::new();
        let skill_name_counts_lower = HashMap::new();

        let selected = tool_orchestrator::filter_connectors_for_input(
            connectors,
            &input,
            &explicit_app_paths,
            &skill_name_counts_lower,
        );

        assert_eq!(selected, Vec::new());
    }

    #[test]
    fn filter_connectors_for_input_skips_when_skill_name_conflicts() {
        let connectors = vec![make_connector("one", "Todoist")];
        let input = vec![user_message("use $todoist")];
        let explicit_app_paths = Vec::new();
        let skill_name_counts_lower = HashMap::from([("todoist".to_string(), 1)]);

        let selected = tool_orchestrator::filter_connectors_for_input(
            connectors,
            &input,
            &explicit_app_paths,
            &skill_name_counts_lower,
        );

        assert_eq!(selected, Vec::new());
    }

    #[tokio::test]
    async fn reconstruct_history_matches_live_compactions() {
        let (session, turn_context) = make_session_and_context().await;
        let (rollout_items, expected) = sample_rollout(&session, &turn_context).await;

        let reconstructed = session
            .reconstruct_history_from_rollout(&turn_context, &rollout_items)
            .await;

        assert_eq!(expected, reconstructed);
    }

    #[tokio::test]
    async fn record_initial_history_reconstructs_resumed_transcript() {
        let (session, turn_context) = make_session_and_context().await;
        let (rollout_items, expected) = sample_rollout(&session, &turn_context).await;

        session
            .record_initial_history(InitialHistory::Resumed(ResumedHistory {
                conversation_id: SessionId::default(),
                history: rollout_items,
                rollout_path: PathBuf::from("/tmp/resume.jsonl"),
            }))
            .await;

        let history = session.state.lock().await.clone_history();
        assert_eq!(expected, history.raw_items());
    }

    #[tokio::test]
    async fn resumed_history_seeds_initial_context_on_first_turn_only() {
        let (session, turn_context) = make_session_and_context().await;
        let (rollout_items, mut expected) = sample_rollout(&session, &turn_context).await;

        session
            .record_initial_history(InitialHistory::Resumed(ResumedHistory {
                conversation_id: SessionId::default(),
                history: rollout_items,
                rollout_path: PathBuf::from("/tmp/resume.jsonl"),
            }))
            .await;

        let history_before_seed = session.state.lock().await.clone_history();
        assert_eq!(expected, history_before_seed.raw_items());

        session.seed_initial_context_if_needed(&turn_context).await;
        expected.extend(session.build_initial_context(&turn_context).await);
        let history_after_seed = session.clone_history().await;
        assert_eq!(expected, history_after_seed.raw_items());

        session.seed_initial_context_if_needed(&turn_context).await;
        let history_after_second_seed = session.clone_history().await;
        assert_eq!(expected, history_after_second_seed.raw_items());
    }

    #[tokio::test]
    async fn record_initial_history_seeds_token_info_from_rollout() {
        let (session, turn_context) = make_session_and_context().await;
        let (mut rollout_items, _expected) = sample_rollout(&session, &turn_context).await;

        let info1 = TokenUsageInfo {
            total_token_usage: TokenUsage {
                input_tokens: 10,
                cached_input_tokens: 0,
                output_tokens: 20,
                reasoning_output_tokens: 0,
                total_tokens: 30,
            },
            last_token_usage: TokenUsage {
                input_tokens: 3,
                cached_input_tokens: 0,
                output_tokens: 4,
                reasoning_output_tokens: 0,
                total_tokens: 7,
            },
            model_context_window: Some(1_000),
        };
        let info2 = TokenUsageInfo {
            total_token_usage: TokenUsage {
                input_tokens: 100,
                cached_input_tokens: 50,
                output_tokens: 200,
                reasoning_output_tokens: 25,
                total_tokens: 375,
            },
            last_token_usage: TokenUsage {
                input_tokens: 10,
                cached_input_tokens: 0,
                output_tokens: 20,
                reasoning_output_tokens: 5,
                total_tokens: 35,
            },
            model_context_window: Some(2_000),
        };

        rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
            TokenCountEvent {
                info: Some(info1),
                rate_limits: None,
            },
        )));
        rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
            TokenCountEvent {
                info: None,
                rate_limits: None,
            },
        )));
        rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
            TokenCountEvent {
                info: Some(info2.clone()),
                rate_limits: None,
            },
        )));
        rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
            TokenCountEvent {
                info: None,
                rate_limits: None,
            },
        )));

        session
            .record_initial_history(InitialHistory::Resumed(ResumedHistory {
                conversation_id: SessionId::default(),
                history: rollout_items,
                rollout_path: PathBuf::from("/tmp/resume.jsonl"),
            }))
            .await;

        let actual = session.state.lock().await.token_info();
        assert_eq!(actual, Some(info2));
    }

    #[tokio::test]
    async fn record_initial_history_reconstructs_forked_transcript() {
        let (session, turn_context) = make_session_and_context().await;
        let (rollout_items, mut expected) = sample_rollout(&session, &turn_context).await;

        session
            .record_initial_history(InitialHistory::Forked(rollout_items))
            .await;

        expected.extend(session.build_initial_context(&turn_context).await);
        let history = session.state.lock().await.clone_history();
        assert_eq!(expected, history.raw_items());
    }

    #[tokio::test]
    async fn session_rollback_drops_last_turn_from_history() {
        let (sess, tc, rx) = make_session_and_context_with_rx().await;

        let initial_context = sess.build_initial_context(tc.as_ref()).await;
        sess.record_into_history(&initial_context, tc.as_ref())
            .await;

        let turn_1 = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "turn 1 user".to_string(),
                }],
                end_turn: None,
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "turn 1 assistant".to_string(),
                }],
                end_turn: None,
                phase: None,
            },
        ];
        sess.record_into_history(&turn_1, tc.as_ref()).await;

        let turn_2 = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "turn 2 user".to_string(),
                }],
                end_turn: None,
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "turn 2 assistant".to_string(),
                }],
                end_turn: None,
                phase: None,
            },
        ];
        sess.record_into_history(&turn_2, tc.as_ref()).await;

        task_coordinator::handlers::session_rollback(&sess, "sub-1".to_string(), 1).await;

        let rollback_event = wait_for_session_rolled_back(&rx).await;
        assert_eq!(rollback_event.num_turns, 1);

        let mut expected = Vec::new();
        expected.extend(initial_context);
        expected.extend(turn_1);

        let history = sess.clone_history().await;
        assert_eq!(expected, history.raw_items());
    }

    #[tokio::test]
    async fn session_rollback_clears_history_when_num_turns_exceeds_existing_turns() {
        let (sess, tc, rx) = make_session_and_context_with_rx().await;

        let initial_context = sess.build_initial_context(tc.as_ref()).await;
        sess.record_into_history(&initial_context, tc.as_ref())
            .await;

        let turn_1 = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "turn 1 user".to_string(),
            }],
            end_turn: None,
            phase: None,
        }];
        sess.record_into_history(&turn_1, tc.as_ref()).await;

        task_coordinator::handlers::session_rollback(&sess, "sub-1".to_string(), 99).await;

        let rollback_event = wait_for_session_rolled_back(&rx).await;
        assert_eq!(rollback_event.num_turns, 99);

        let history = sess.clone_history().await;
        assert_eq!(initial_context, history.raw_items());
    }

    #[tokio::test]
    async fn session_rollback_fails_when_turn_in_progress() {
        let (sess, tc, rx) = make_session_and_context_with_rx().await;

        let initial_context = sess.build_initial_context(tc.as_ref()).await;
        sess.record_into_history(&initial_context, tc.as_ref())
            .await;

        *sess.active_turn.lock().await = Some(crate::state::ActiveTurn::default());
        task_coordinator::handlers::session_rollback(&sess, "sub-1".to_string(), 1).await;

        let error_event = wait_for_session_rollback_failed(&rx).await;
        assert_eq!(
            error_event.savfox_error_info,
            Some(SavfoxErrorInfo::SessionRollbackFailed)
        );

        let history = sess.clone_history().await;
        assert_eq!(initial_context, history.raw_items());
    }

    #[tokio::test]
    async fn session_rollback_fails_when_num_turns_is_zero() {
        let (sess, tc, rx) = make_session_and_context_with_rx().await;

        let initial_context = sess.build_initial_context(tc.as_ref()).await;
        sess.record_into_history(&initial_context, tc.as_ref())
            .await;

        task_coordinator::handlers::session_rollback(&sess, "sub-1".to_string(), 0).await;

        let error_event = wait_for_session_rollback_failed(&rx).await;
        assert_eq!(error_event.message, "num_turns must be >= 1");
        assert_eq!(
            error_event.savfox_error_info,
            Some(SavfoxErrorInfo::SessionRollbackFailed)
        );

        let history = sess.clone_history().await;
        assert_eq!(initial_context, history.raw_items());
    }

    #[tokio::test]
    async fn override_turn_context_prefixed_model_switches_provider() {
        let (sess, _tc) = make_session_and_context().await;
        let before_provider = {
            let state = sess.state.lock().await;
            state
                .session_configuration
                .original_config_do_not_use
                .model_provider_id
                .clone()
        };
        assert_eq!(before_provider, "openai");

        let current_mode = sess.current_collaboration_mode().await;
        let updated_mode =
            current_mode.with_updates(Some("zhipuai-coding-plan/glm-5".to_string()), None, None);
        sess.update_settings(SessionSettingsUpdate {
            collaboration_mode: Some(updated_mode),
            ..Default::default()
        })
        .await
        .expect("update settings should succeed");

        let after_provider = {
            let state = sess.state.lock().await;
            state
                .session_configuration
                .original_config_do_not_use
                .model_provider_id
                .clone()
        };
        assert_eq!(after_provider, "openai");

        let after_mode = sess.current_collaboration_mode().await;
        assert_eq!(after_mode.model(), "zhipuai-coding-plan/glm-5");
    }

    #[tokio::test]
    async fn set_rate_limits_retains_previous_credits() {
        let savfox_home = tempfile::tempdir().expect("create temp dir");
        let config = build_test_config(savfox_home.path()).await;
        let config = Arc::new(config);
        let model = ModelsManager::get_model_offline(config.model.as_deref());
        let model_info = ModelsManager::construct_model_info_offline(model.as_str(), &config);
        let reasoning_effort = config.model_reasoning_effort;
        let collaboration_mode = CollaborationMode {
            mode: ModeKind::Custom,
            settings: Settings {
                model,
                reasoning_effort,
                developer_instructions: None,
            },
        };
        let session_configuration = SessionConfiguration {
            provider: config.model_provider.clone(),
            collaboration_mode,
            model_reasoning_summary: config.model_reasoning_summary,
            developer_instructions: config.developer_instructions.clone(),
            user_instructions: config.user_instructions.clone(),
            personality: config.personality,
            base_instructions: config
                .base_instructions
                .clone()
                .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
            compact_prompt: config.compact_prompt.clone(),
            approval_policy: config.approval_policy.clone(),
            sandbox_policy: config.sandbox_policy.clone(),
            windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
            cwd: config.cwd.clone(),
            savfox_home: config.savfox_home.clone(),
            session_name: None,
            original_config_do_not_use: Arc::clone(&config),
            session_source: SessionSource::Exec,
            dynamic_tools: Vec::new(),
            tool_access_policy: config.tool_access_policy.clone(),
        };

        let mut state = SessionState::new(session_configuration);
        let initial = RateLimitSnapshot {
            primary: Some(RateLimitWindow {
                used_percent: 10.0,
                window_minutes: Some(15),
                resets_at: Some(1_700),
            }),
            secondary: None,
            credits: Some(CreditsSnapshot {
                has_credits: true,
                unlimited: false,
                balance: Some("10.00".to_string()),
            }),
            plan_type: Some(savfox_protocol::account::PlanType::Plus),
        };
        state.set_rate_limits(initial.clone());

        let update = RateLimitSnapshot {
            primary: Some(RateLimitWindow {
                used_percent: 40.0,
                window_minutes: Some(30),
                resets_at: Some(1_800),
            }),
            secondary: Some(RateLimitWindow {
                used_percent: 5.0,
                window_minutes: Some(60),
                resets_at: Some(1_900),
            }),
            credits: None,
            plan_type: None,
        };
        state.set_rate_limits(update.clone());

        assert_eq!(
            state.latest_rate_limits,
            Some(RateLimitSnapshot {
                primary: update.primary.clone(),
                secondary: update.secondary,
                credits: initial.credits,
                plan_type: initial.plan_type,
            })
        );
    }

    #[tokio::test]
    async fn set_rate_limits_updates_plan_type_when_present() {
        let savfox_home = tempfile::tempdir().expect("create temp dir");
        let config = build_test_config(savfox_home.path()).await;
        let config = Arc::new(config);
        let model = ModelsManager::get_model_offline(config.model.as_deref());
        let model_info = ModelsManager::construct_model_info_offline(model.as_str(), &config);
        let reasoning_effort = config.model_reasoning_effort;
        let collaboration_mode = CollaborationMode {
            mode: ModeKind::Custom,
            settings: Settings {
                model,
                reasoning_effort,
                developer_instructions: None,
            },
        };
        let session_configuration = SessionConfiguration {
            provider: config.model_provider.clone(),
            collaboration_mode,
            model_reasoning_summary: config.model_reasoning_summary,
            developer_instructions: config.developer_instructions.clone(),
            user_instructions: config.user_instructions.clone(),
            personality: config.personality,
            base_instructions: config
                .base_instructions
                .clone()
                .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
            compact_prompt: config.compact_prompt.clone(),
            approval_policy: config.approval_policy.clone(),
            sandbox_policy: config.sandbox_policy.clone(),
            windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
            cwd: config.cwd.clone(),
            savfox_home: config.savfox_home.clone(),
            session_name: None,
            original_config_do_not_use: Arc::clone(&config),
            session_source: SessionSource::Exec,
            dynamic_tools: Vec::new(),
            tool_access_policy: config.tool_access_policy.clone(),
        };

        let mut state = SessionState::new(session_configuration);
        let initial = RateLimitSnapshot {
            primary: Some(RateLimitWindow {
                used_percent: 15.0,
                window_minutes: Some(20),
                resets_at: Some(1_600),
            }),
            secondary: Some(RateLimitWindow {
                used_percent: 5.0,
                window_minutes: Some(45),
                resets_at: Some(1_650),
            }),
            credits: Some(CreditsSnapshot {
                has_credits: true,
                unlimited: false,
                balance: Some("15.00".to_string()),
            }),
            plan_type: Some(savfox_protocol::account::PlanType::Plus),
        };
        state.set_rate_limits(initial.clone());

        let update = RateLimitSnapshot {
            primary: Some(RateLimitWindow {
                used_percent: 35.0,
                window_minutes: Some(25),
                resets_at: Some(1_700),
            }),
            secondary: None,
            credits: None,
            plan_type: Some(savfox_protocol::account::PlanType::Pro),
        };
        state.set_rate_limits(update.clone());

        assert_eq!(
            state.latest_rate_limits,
            Some(RateLimitSnapshot {
                primary: update.primary,
                secondary: update.secondary,
                credits: initial.credits,
                plan_type: update.plan_type,
            })
        );
    }

    #[test]
    fn prefers_structured_content_when_present() {
        let ctr = McpCallToolResult {
            content: vec![text_block("ignored")],
            is_error: None,
            structured_content: Some(json!({
                "ok": true,
                "value": 42
            })),
            meta: None,
        };

        let got = FunctionCallOutputPayload::from(&ctr);
        let expected = FunctionCallOutputPayload {
            content: serde_json::to_string(&json!({
                "ok": true,
                "value": 42
            }))
            .unwrap(),
            success: Some(true),
            ..Default::default()
        };

        assert_eq!(expected, got);
    }

    #[tokio::test]
    async fn includes_timed_out_message() {
        let exec = ExecToolCallOutput {
            exit_code: 0,
            stdout: StreamOutput::new(String::new()),
            stderr: StreamOutput::new(String::new()),
            aggregated_output: StreamOutput::new("Command output".to_string()),
            duration: StdDuration::from_secs(1),
            timed_out: true,
        };
        let (_, turn_context) = make_session_and_context().await;

        let out = format_exec_output_str(&exec, turn_context.truncation_policy);

        assert_eq!(
            out,
            "command timed out after 1000 milliseconds\nCommand output"
        );
    }

    #[test]
    fn falls_back_to_content_when_structured_is_null() {
        let ctr = McpCallToolResult {
            content: vec![text_block("hello"), text_block("world")],
            is_error: None,
            structured_content: Some(serde_json::Value::Null),
            meta: None,
        };

        let got = FunctionCallOutputPayload::from(&ctr);
        let expected = FunctionCallOutputPayload {
            content: serde_json::to_string(&vec![text_block("hello"), text_block("world")])
                .unwrap(),
            success: Some(true),
            ..Default::default()
        };

        assert_eq!(expected, got);
    }

    #[test]
    fn success_flag_reflects_is_error_true() {
        let ctr = McpCallToolResult {
            content: vec![text_block("unused")],
            is_error: Some(true),
            structured_content: Some(json!({ "message": "bad" })),
            meta: None,
        };

        let got = FunctionCallOutputPayload::from(&ctr);
        let expected = FunctionCallOutputPayload {
            content: serde_json::to_string(&json!({ "message": "bad" })).unwrap(),
            success: Some(false),
            ..Default::default()
        };

        assert_eq!(expected, got);
    }

    #[test]
    fn success_flag_true_with_no_error_and_content_used() {
        let ctr = McpCallToolResult {
            content: vec![text_block("alpha")],
            is_error: Some(false),
            structured_content: None,
            meta: None,
        };

        let got = FunctionCallOutputPayload::from(&ctr);
        let expected = FunctionCallOutputPayload {
            content: serde_json::to_string(&vec![text_block("alpha")]).unwrap(),
            success: Some(true),
            ..Default::default()
        };

        assert_eq!(expected, got);
    }

    async fn wait_for_session_rolled_back(
        rx: &async_channel::Receiver<Event>,
    ) -> crate::protocol::SessionRolledBackEvent {
        let deadline = StdDuration::from_secs(2);
        let start = std::time::Instant::now();
        loop {
            let remaining = deadline.saturating_sub(start.elapsed());
            let evt = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("timeout waiting for event")
                .expect("event");
            match evt.msg {
                EventMsg::SessionRolledBack(payload) => return payload,
                _ => continue,
            }
        }
    }

    async fn wait_for_session_rollback_failed(rx: &async_channel::Receiver<Event>) -> ErrorEvent {
        let deadline = StdDuration::from_secs(2);
        let start = std::time::Instant::now();
        loop {
            let remaining = deadline.saturating_sub(start.elapsed());
            let evt = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("timeout waiting for event")
                .expect("event");
            match evt.msg {
                EventMsg::Error(payload)
                    if payload.savfox_error_info
                        == Some(SavfoxErrorInfo::SessionRollbackFailed) =>
                {
                    return payload;
                }
                _ => continue,
            }
        }
    }

    fn text_block(s: &str) -> serde_json::Value {
        json!({
            "type": "text",
            "text": s,
        })
    }

    async fn build_test_config(savfox_home: &Path) -> Config {
        ConfigBuilder::default()
            .savfox_home(savfox_home.to_path_buf())
            .build()
            .await
            .expect("load default test config")
    }

    fn otel_manager(
        conversation_id: SessionId,
        config: &Config,
        model_info: &ModelInfo,
        session_source: SessionSource,
    ) -> OtelManager {
        OtelManager::new(
            conversation_id,
            ModelsManager::get_model_offline(config.model.as_deref()).as_str(),
            model_info.slug.as_str(),
            None,
            Some("test@test.com".to_string()),
            Some(AuthMode::Chatgpt),
            false,
            "test".to_string(),
            session_source,
        )
    }

    pub(crate) async fn make_session_and_context() -> (Session, TurnContext) {
        let (tx_event, _rx_event) = async_channel::unbounded();
        let savfox_home = tempfile::tempdir().expect("create temp dir");
        let config = build_test_config(savfox_home.path()).await;
        let config = Arc::new(config);
        let conversation_id = SessionId::default();
        let auth_manager =
            AuthManager::from_auth_for_testing(SavfoxAuth::from_api_key("Test API Key"));
        let models_manager = Arc::new(ModelsManager::new(
            config.savfox_home.clone(),
            auth_manager.clone(),
        ));
        let agent_control = AgentControl::default();
        let exec_policy = ExecPolicyManager::default();
        let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
        let model = ModelsManager::get_model_offline(config.model.as_deref());
        let model_info = ModelsManager::construct_model_info_offline(model.as_str(), &config);
        let reasoning_effort = config.model_reasoning_effort;
        let collaboration_mode = CollaborationMode {
            mode: ModeKind::Custom,
            settings: Settings {
                model,
                reasoning_effort,
                developer_instructions: None,
            },
        };
        let session_configuration = SessionConfiguration {
            provider: config.model_provider.clone(),
            collaboration_mode,
            model_reasoning_summary: config.model_reasoning_summary,
            developer_instructions: config.developer_instructions.clone(),
            user_instructions: config.user_instructions.clone(),
            personality: config.personality,
            base_instructions: config
                .base_instructions
                .clone()
                .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
            compact_prompt: config.compact_prompt.clone(),
            approval_policy: config.approval_policy.clone(),
            sandbox_policy: config.sandbox_policy.clone(),
            windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
            cwd: config.cwd.clone(),
            savfox_home: config.savfox_home.clone(),
            session_name: None,
            original_config_do_not_use: Arc::clone(&config),
            session_source: SessionSource::Exec,
            dynamic_tools: Vec::new(),
            tool_access_policy: config.tool_access_policy.clone(),
        };
        let per_turn_config = Session::build_per_turn_config(&session_configuration);
        let model_info = ModelsManager::construct_model_info_offline(
            session_configuration.collaboration_mode.model(),
            &per_turn_config,
        );
        let otel_manager = otel_manager(
            conversation_id,
            config.as_ref(),
            &model_info,
            session_configuration.session_source.clone(),
        );

        let mut state = SessionState::new(session_configuration.clone());
        mark_state_initial_context_seeded(&mut state);
        let skills_manager = Arc::new(SkillsManager::new(
            config.savfox_home.clone(),
            config.registry.as_ref(),
        ));

        let services = SessionServices {
            mcp_connection_manager: Arc::new(RwLock::new(McpConnectionManager::default())),
            mcp_startup_cancellation_token: Mutex::new(CancellationToken::new()),
            unified_exec_manager: UnifiedExecProcessManager::default(),
            analytics_events_client: AnalyticsEventsClient::new(
                Arc::clone(&config),
                Arc::clone(&auth_manager),
            ),
            notifier: UserNotifier::new(None),
            rollout: Mutex::new(None),
            user_shell: Arc::new(default_user_shell()),
            show_raw_agent_reasoning: config.show_raw_agent_reasoning,
            exec_policy,
            auth_manager: auth_manager.clone(),
            otel_manager: otel_manager.clone(),
            models_manager: Arc::clone(&models_manager),
            tool_approvals: Mutex::new(ApprovalStore::default()),
            skills_manager,
            agent_control,
            state_db: None,
            transport_manager: TransportManager::new(),
        };

        let turn_context = Session::make_turn_context(
            Some(Arc::clone(&auth_manager)),
            &otel_manager,
            session_configuration.provider.clone(),
            &session_configuration,
            per_turn_config,
            model_info,
            conversation_id,
            "turn_id".to_string(),
            services.transport_manager.clone(),
        );

        let session = Session {
            conversation_id,
            tx_event,
            agent_status: agent_status_tx,
            state: Mutex::new(state),
            features: config.features.clone(),
            pending_mcp_server_refresh_config: Mutex::new(None),
            active_turn: Mutex::new(None),
            services,
            next_internal_sub_id: AtomicU64::new(0),
        };

        (session, turn_context)
    }

    // Like make_session_and_context, but returns Arc<Session> and the event receiver
    // so tests can assert on emitted events.
    pub(crate) async fn make_session_and_context_with_rx() -> (
        Arc<Session>,
        Arc<TurnContext>,
        async_channel::Receiver<Event>,
    ) {
        let (tx_event, rx_event) = async_channel::unbounded();
        let savfox_home = tempfile::tempdir().expect("create temp dir");
        let config = build_test_config(savfox_home.path()).await;
        let config = Arc::new(config);
        let conversation_id = SessionId::default();
        let auth_manager =
            AuthManager::from_auth_for_testing(SavfoxAuth::from_api_key("Test API Key"));
        let models_manager = Arc::new(ModelsManager::new(
            config.savfox_home.clone(),
            auth_manager.clone(),
        ));
        let agent_control = AgentControl::default();
        let exec_policy = ExecPolicyManager::default();
        let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
        let model = ModelsManager::get_model_offline(config.model.as_deref());
        let model_info = ModelsManager::construct_model_info_offline(model.as_str(), &config);
        let reasoning_effort = config.model_reasoning_effort;
        let collaboration_mode = CollaborationMode {
            mode: ModeKind::Custom,
            settings: Settings {
                model,
                reasoning_effort,
                developer_instructions: None,
            },
        };
        let session_configuration = SessionConfiguration {
            provider: config.model_provider.clone(),
            collaboration_mode,
            model_reasoning_summary: config.model_reasoning_summary,
            developer_instructions: config.developer_instructions.clone(),
            user_instructions: config.user_instructions.clone(),
            personality: config.personality,
            base_instructions: config
                .base_instructions
                .clone()
                .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
            compact_prompt: config.compact_prompt.clone(),
            approval_policy: config.approval_policy.clone(),
            sandbox_policy: config.sandbox_policy.clone(),
            windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
            cwd: config.cwd.clone(),
            savfox_home: config.savfox_home.clone(),
            session_name: None,
            original_config_do_not_use: Arc::clone(&config),
            session_source: SessionSource::Exec,
            dynamic_tools: Vec::new(),
            tool_access_policy: config.tool_access_policy.clone(),
        };
        let per_turn_config = Session::build_per_turn_config(&session_configuration);
        let model_info = ModelsManager::construct_model_info_offline(
            session_configuration.collaboration_mode.model(),
            &per_turn_config,
        );
        let otel_manager = otel_manager(
            conversation_id,
            config.as_ref(),
            &model_info,
            session_configuration.session_source.clone(),
        );

        let mut state = SessionState::new(session_configuration.clone());
        mark_state_initial_context_seeded(&mut state);
        let skills_manager = Arc::new(SkillsManager::new(
            config.savfox_home.clone(),
            config.registry.as_ref(),
        ));

        let services = SessionServices {
            mcp_connection_manager: Arc::new(RwLock::new(McpConnectionManager::default())),
            mcp_startup_cancellation_token: Mutex::new(CancellationToken::new()),
            unified_exec_manager: UnifiedExecProcessManager::default(),
            analytics_events_client: AnalyticsEventsClient::new(
                Arc::clone(&config),
                Arc::clone(&auth_manager),
            ),
            notifier: UserNotifier::new(None),
            rollout: Mutex::new(None),
            user_shell: Arc::new(default_user_shell()),
            show_raw_agent_reasoning: config.show_raw_agent_reasoning,
            exec_policy,
            auth_manager: Arc::clone(&auth_manager),
            otel_manager: otel_manager.clone(),
            models_manager: Arc::clone(&models_manager),
            tool_approvals: Mutex::new(ApprovalStore::default()),
            skills_manager,
            agent_control,
            state_db: None,
            transport_manager: TransportManager::new(),
        };

        let turn_context = Arc::new(Session::make_turn_context(
            Some(Arc::clone(&auth_manager)),
            &otel_manager,
            session_configuration.provider.clone(),
            &session_configuration,
            per_turn_config,
            model_info,
            conversation_id,
            "turn_id".to_string(),
            services.transport_manager.clone(),
        ));

        let session = Arc::new(Session {
            conversation_id,
            tx_event,
            agent_status: agent_status_tx,
            state: Mutex::new(state),
            features: config.features.clone(),
            pending_mcp_server_refresh_config: Mutex::new(None),
            active_turn: Mutex::new(None),
            services,
            next_internal_sub_id: AtomicU64::new(0),
        });

        (session, turn_context, rx_event)
    }

    fn mark_state_initial_context_seeded(state: &mut SessionState) {
        state.initial_context_seeded = true;
    }

    #[tokio::test]
    async fn refresh_mcp_servers_is_deferred_until_next_turn() {
        let (session, turn_context) = make_session_and_context().await;
        let old_token = session.mcp_startup_cancellation_token().await;
        assert!(!old_token.is_cancelled());

        let mcp_oauth_credentials_store_mode =
            serde_json::to_value(OAuthCredentialsStoreMode::Auto).expect("serialize store mode");
        let refresh_config = McpServerRefreshConfig {
            mcp_servers: json!({}),
            mcp_oauth_credentials_store_mode,
        };
        {
            let mut guard = session.pending_mcp_server_refresh_config.lock().await;
            *guard = Some(refresh_config);
        }

        assert!(!old_token.is_cancelled());
        assert!(
            session
                .pending_mcp_server_refresh_config
                .lock()
                .await
                .is_some()
        );

        session
            .refresh_mcp_servers_if_requested(&turn_context)
            .await;

        assert!(old_token.is_cancelled());
        assert!(
            session
                .pending_mcp_server_refresh_config
                .lock()
                .await
                .is_none()
        );
        let new_token = session.mcp_startup_cancellation_token().await;
        assert!(!new_token.is_cancelled());
    }

    #[tokio::test]
    async fn record_model_warning_appends_user_message() {
        let (mut session, turn_context) = make_session_and_context().await;
        let features = Features::with_defaults();
        session.features = features;

        session
            .record_model_warning("too many unified exec processes", &turn_context)
            .await;

        let history = session.clone_history().await;
        let history_items = history.raw_items();
        let last = history_items.last().expect("warning recorded");

        match last {
            ResponseItem::Message { role, content, .. } => {
                assert_eq!(role, "user");
                assert_eq!(
                    content,
                    &vec![ContentItem::InputText {
                        text: "Warning: too many unified exec processes".to_string(),
                    }]
                );
            }
            other => panic!("expected user message, got {other:?}"),
        }
    }

    #[derive(Clone, Copy)]
    struct NeverEndingTask {
        kind: TaskKind,
        listen_to_cancellation_token: bool,
    }

    #[async_trait::async_trait]
    impl SessionTask for NeverEndingTask {
        fn kind(&self) -> TaskKind {
            self.kind
        }

        async fn run(
            self: Arc<Self>,
            _session: Arc<SessionTaskContext>,
            _ctx: Arc<TurnContext>,
            _input: Vec<UserInput>,
            cancellation_token: CancellationToken,
        ) -> Option<String> {
            if self.listen_to_cancellation_token {
                cancellation_token.cancelled().await;
                return None;
            }
            loop {
                sleep(Duration::from_secs(60)).await;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[test_log::test]
    async fn abort_regular_task_emits_turn_aborted_only() {
        let (sess, tc, rx) = make_session_and_context_with_rx().await;
        let input = vec![UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        }];
        sess.spawn_task(
            Arc::clone(&tc),
            input,
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: false,
            },
        )
        .await;

        sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

        // Interrupts persist a model-visible `<turn_aborted>` marker into history, but there is no
        // separate client-visible event for that marker (only `EventMsg::TurnAborted`).
        let evt = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("event");
        match evt.msg {
            EventMsg::TurnAborted(e) => assert_eq!(TurnAbortReason::Interrupted, e.reason),
            other => panic!("unexpected event: {other:?}"),
        }
        // No extra events should be emitted after an abort.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn abort_gracefuly_emits_turn_aborted_only() {
        let (sess, tc, rx) = make_session_and_context_with_rx().await;
        let input = vec![UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        }];
        sess.spawn_task(
            Arc::clone(&tc),
            input,
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;

        sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

        // Even if tasks handle cancellation gracefully, interrupts still result in `TurnAborted`
        // being the only client-visible signal.
        let evt = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("event");
        match evt.msg {
            EventMsg::TurnAborted(e) => assert_eq!(TurnAbortReason::Interrupted, e.reason),
            other => panic!("unexpected event: {other:?}"),
        }
        // No extra events should be emitted after an abort.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abort_review_task_emits_exited_then_aborted_and_records_history() {
        let (sess, tc, rx) = make_session_and_context_with_rx().await;
        let input = vec![UserInput::Text {
            text: "start review".to_string(),
            text_elements: Vec::new(),
        }];
        sess.spawn_task(Arc::clone(&tc), input, ReviewTask::new())
            .await;

        sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

        // Aborting a review task should exit review mode before surfacing the abort to the client.
        // We scan for these events (rather than relying on fixed ordering) since unrelated events
        // may interleave.
        let mut exited_review_mode_idx = None;
        let mut turn_aborted_idx = None;
        let mut idx = 0usize;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let evt = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("timeout waiting for event")
                .expect("event");
            let event_idx = idx;
            idx = idx.saturating_add(1);
            match evt.msg {
                EventMsg::ExitedReviewMode(ev) => {
                    assert!(ev.review_output.is_none());
                    exited_review_mode_idx = Some(event_idx);
                }
                EventMsg::TurnAborted(ev) => {
                    assert_eq!(TurnAbortReason::Interrupted, ev.reason);
                    turn_aborted_idx = Some(event_idx);
                    break;
                }
                _ => {}
            }
        }
        assert!(
            exited_review_mode_idx.is_some(),
            "expected ExitedReviewMode after abort"
        );
        assert!(
            turn_aborted_idx.is_some(),
            "expected TurnAborted after abort"
        );
        assert!(
            exited_review_mode_idx.unwrap() < turn_aborted_idx.unwrap(),
            "expected ExitedReviewMode before TurnAborted"
        );

        let history = sess.clone_history().await;
        // The `<turn_aborted>` marker is silent in the event stream, so verify it is still
        // recorded in history for the model.
        assert!(
            history.raw_items().iter().any(|item| {
                let ResponseItem::Message { role, content, .. } = item else {
                    return false;
                };
                if role != "user" {
                    return false;
                }
                content.iter().any(|content_item| {
                    let ContentItem::InputText { text } = content_item else {
                        return false;
                    };
                    text.contains(crate::session_prefix::TURN_ABORTED_OPEN_TAG)
                })
            }),
            "expected a model-visible turn aborted marker in history after interrupt"
        );
    }

    #[tokio::test]
    async fn fatal_tool_error_stops_turn_and_reports_error() {
        let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
        let tools = {
            session
                .services
                .mcp_connection_manager
                .read()
                .await
                .list_all_tools()
                .await
        };
        let mut router = ToolRouter::from_config(
            &turn_context.tools_config,
            Some(
                tools
                    .into_iter()
                    .map(|(name, tool)| (name, tool.tool))
                    .collect(),
            ),
            turn_context.dynamic_tools.as_slice(),
        );
        // Apply tool access filtering from the unified permission system.
        if let Some(ref policy) = turn_context.tool_access_policy {
            router.apply_tool_access_policy(policy);
        }
        let item = ResponseItem::CustomToolCall {
            id: None,
            status: None,
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            input: "{}".to_string(),
        };

        let call = ToolRouter::build_tool_call(session.as_ref(), item.clone())
            .await
            .expect("build tool call")
            .expect("tool call present");
        let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
        let err = router
            .dispatch_tool_call(
                Arc::clone(&session),
                Arc::clone(&turn_context),
                tracker,
                call,
            )
            .await
            .expect_err("expected fatal error");

        match err {
            FunctionCallError::Fatal(message) => {
                assert_eq!(message, "tool shell invoked with incompatible payload");
            }
            other => panic!("expected FunctionCallError::Fatal, got {other:?}"),
        }
    }

    async fn sample_rollout(
        session: &Session,
        turn_context: &TurnContext,
    ) -> (Vec<RolloutItem>, Vec<ResponseItem>) {
        let mut rollout_items = Vec::new();
        let mut live_history = ContextManager::new();

        let initial_context = session.build_initial_context(turn_context).await;
        for item in &initial_context {
            rollout_items.push(RolloutItem::ResponseItem(item.clone()));
        }
        live_history.record_items(initial_context.iter(), turn_context.truncation_policy);

        let user1 = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "first user".to_string(),
            }],
            end_turn: None,
            phase: None,
        };
        live_history.record_items(std::iter::once(&user1), turn_context.truncation_policy);
        rollout_items.push(RolloutItem::ResponseItem(user1.clone()));

        let assistant1 = ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "assistant reply one".to_string(),
            }],
            end_turn: None,
            phase: None,
        };
        live_history.record_items(std::iter::once(&assistant1), turn_context.truncation_policy);
        rollout_items.push(RolloutItem::ResponseItem(assistant1.clone()));

        let summary1 = "summary one";
        let snapshot1 = live_history.clone().for_prompt();
        let user_messages1 = collect_user_messages(&snapshot1);
        let rebuilt1 = compact::build_compacted_history(
            session.build_initial_context(turn_context).await,
            &user_messages1,
            summary1,
        );
        live_history.replace(rebuilt1);
        rollout_items.push(RolloutItem::Compacted(CompactedItem {
            message: summary1.to_string(),
            replacement_history: None,
        }));

        let user2 = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "second user".to_string(),
            }],
            end_turn: None,
            phase: None,
        };
        live_history.record_items(std::iter::once(&user2), turn_context.truncation_policy);
        rollout_items.push(RolloutItem::ResponseItem(user2.clone()));

        let assistant2 = ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "assistant reply two".to_string(),
            }],
            end_turn: None,
            phase: None,
        };
        live_history.record_items(std::iter::once(&assistant2), turn_context.truncation_policy);
        rollout_items.push(RolloutItem::ResponseItem(assistant2.clone()));

        let summary2 = "summary two";
        let snapshot2 = live_history.clone().for_prompt();
        let user_messages2 = collect_user_messages(&snapshot2);
        let rebuilt2 = compact::build_compacted_history(
            session.build_initial_context(turn_context).await,
            &user_messages2,
            summary2,
        );
        live_history.replace(rebuilt2);
        rollout_items.push(RolloutItem::Compacted(CompactedItem {
            message: summary2.to_string(),
            replacement_history: None,
        }));

        let user3 = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "third user".to_string(),
            }],
            end_turn: None,
            phase: None,
        };
        live_history.record_items(std::iter::once(&user3), turn_context.truncation_policy);
        rollout_items.push(RolloutItem::ResponseItem(user3));

        let assistant3 = ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "assistant reply three".to_string(),
            }],
            end_turn: None,
            phase: None,
        };
        live_history.record_items(std::iter::once(&assistant3), turn_context.truncation_policy);
        rollout_items.push(RolloutItem::ResponseItem(assistant3));

        (rollout_items, live_history.for_prompt())
    }

    #[tokio::test]
    async fn rejects_escalated_permissions_when_policy_not_on_request() {
        use std::collections::HashMap;

        use crate::exec::ExecParams;
        use crate::protocol::{AskForApproval, SandboxPolicy};
        use crate::sandboxing::SandboxPermissions;
        use crate::turn_diff_tracker::TurnDiffTracker;

        let (session, mut turn_context_raw) = make_session_and_context().await;
        // Ensure policy is NOT OnRequest so the early rejection path triggers
        turn_context_raw.approval_policy = AskForApproval::OnFailure;
        let session = Arc::new(session);
        let mut turn_context = Arc::new(turn_context_raw);

        let timeout_ms = 1000;
        let sandbox_permissions = SandboxPermissions::RequireEscalated;
        let params = ExecParams {
            command: if cfg!(windows) {
                vec![
                    "cmd.exe".to_string(),
                    "/C".to_string(),
                    "echo hi".to_string(),
                ]
            } else {
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "echo hi".to_string(),
                ]
            },
            cwd: turn_context.cwd.clone(),
            expiration: timeout_ms.into(),
            env: HashMap::new(),
            sandbox_permissions,
            windows_sandbox_level: turn_context.windows_sandbox_level,
            justification: Some("test".to_string()),
            arg0: None,
        };

        let params2 = ExecParams {
            sandbox_permissions: SandboxPermissions::UseDefault,
            command: params.command.clone(),
            cwd: params.cwd.clone(),
            expiration: timeout_ms.into(),
            env: HashMap::new(),
            windows_sandbox_level: turn_context.windows_sandbox_level,
            justification: params.justification.clone(),
            arg0: None,
        };

        let turn_diff_tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));

        let tool_name = "shell";
        let call_id = "test-call".to_string();

        let handler = ShellHandler;
        let resp = handler
            .handle(ToolInvocation {
                session: Arc::clone(&session),
                turn: Arc::clone(&turn_context),
                tracker: Arc::clone(&turn_diff_tracker),
                call_id,
                tool_name: tool_name.to_string(),
                payload: ToolPayload::Function {
                    arguments: serde_json::json!({
                        "command": params.command.clone(),
                        "workdir": Some(turn_context.cwd.to_string_lossy().to_string()),
                        "timeout_ms": params.expiration.timeout_ms(),
                        "sandbox_permissions": params.sandbox_permissions,
                        "justification": params.justification.clone(),
                    })
                    .to_string(),
                },
            })
            .await;

        let Err(FunctionCallError::RespondToModel(output)) = resp else {
            panic!("expected error result");
        };

        let expected = format!(
            "approval policy is {policy:?}; reject command — you should not ask for escalated permissions if the approval policy is {policy:?}",
            policy = turn_context.approval_policy
        );

        pretty_assertions::assert_eq!(output, expected);

        // Now retry the same command WITHOUT escalated permissions; should succeed.
        // Force DangerFullAccess to avoid platform sandbox dependencies in tests.
        Arc::get_mut(&mut turn_context)
            .expect("unique turn context Arc")
            .sandbox_policy = SandboxPolicy::DangerFullAccess;

        let resp2 = handler
            .handle(ToolInvocation {
                session: Arc::clone(&session),
                turn: Arc::clone(&turn_context),
                tracker: Arc::clone(&turn_diff_tracker),
                call_id: "test-call-2".to_string(),
                tool_name: tool_name.to_string(),
                payload: ToolPayload::Function {
                    arguments: serde_json::json!({
                        "command": params2.command.clone(),
                        "workdir": Some(turn_context.cwd.to_string_lossy().to_string()),
                        "timeout_ms": params2.expiration.timeout_ms(),
                        "sandbox_permissions": params2.sandbox_permissions,
                        "justification": params2.justification.clone(),
                    })
                    .to_string(),
                },
            })
            .await;

        let output = match resp2.expect("expected Ok result") {
            ToolOutput::Function { content, .. } => content,
            _ => panic!("unexpected tool output"),
        };

        #[derive(Deserialize, PartialEq, Eq, Debug)]
        struct ResponseExecMetadata {
            exit_code: i32,
        }

        #[derive(Deserialize)]
        struct ResponseExecOutput {
            output: String,
            metadata: ResponseExecMetadata,
        }

        let exec_output: ResponseExecOutput =
            serde_json::from_str(&output).expect("valid exec output json");

        pretty_assertions::assert_eq!(exec_output.metadata, ResponseExecMetadata { exit_code: 0 });
        assert!(exec_output.output.contains("hi"));
    }
    #[tokio::test]
    async fn unified_exec_rejects_escalated_permissions_when_policy_not_on_request() {
        use crate::protocol::AskForApproval;
        use crate::sandboxing::SandboxPermissions;
        use crate::turn_diff_tracker::TurnDiffTracker;

        let (session, mut turn_context_raw) = make_session_and_context().await;
        turn_context_raw.approval_policy = AskForApproval::OnFailure;
        let session = Arc::new(session);
        let turn_context = Arc::new(turn_context_raw);
        let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));

        let handler = UnifiedExecHandler;
        let resp = handler
            .handle(ToolInvocation {
                session: Arc::clone(&session),
                turn: Arc::clone(&turn_context),
                tracker: Arc::clone(&tracker),
                call_id: "exec-call".to_string(),
                tool_name: "exec_command".to_string(),
                payload: ToolPayload::Function {
                    arguments: serde_json::json!({
                        "cmd": "echo hi",
                        "sandbox_permissions": SandboxPermissions::RequireEscalated,
                        "justification": "need unsandboxed execution",
                    })
                    .to_string(),
                },
            })
            .await;

        let Err(FunctionCallError::RespondToModel(output)) = resp else {
            panic!("expected error result");
        };

        let expected = format!(
            "approval policy is {policy:?}; reject command — you cannot ask for escalated permissions if the approval policy is {policy:?}",
            policy = turn_context.approval_policy
        );

        pretty_assertions::assert_eq!(output, expected);
    }
}
