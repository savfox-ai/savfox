mod auth_handler;
mod mcp_handler;
mod misc_handler;
mod session_handler;
mod skill_handler;
mod turn_handler;

use std::collections::{HashMap, HashSet};
use std::io::Error as IoError;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use savfox_app_server_protocol::{
    ClientRequest, ConversationGitInfo, ConversationSummary, DynamicToolSpec as ApiDynamicToolSpec,
    JSONRPCErrorError, RequestId, ReviewTarget as ApiReviewTarget, SandboxMode, Session, TurnError,
};
use savfox_core::config::{Config, ConfigOverrides};
use savfox_core::config_loader::CloudRequirementsLoader;
use savfox_core::protocol::{ReviewRequest, ReviewTarget as CoreReviewTarget};
use savfox_core::{
    AuthManager, InitialHistory, RolloutRecorder, SavfoxSession, SessionConfigSnapshot,
    SessionManager, SessionMeta, read_head_for_summary,
};
use savfox_feedback::SavfoxFeedback;
use savfox_login_oauth::ShutdownHandle;
use savfox_protocol::SessionId;
use savfox_protocol::config_types::Personality;
use savfox_protocol::items::TurnItem;
use savfox_protocol::models::ResponseItem;
use savfox_protocol::protocol::{
    GitInfo as CoreGitInfo, RolloutItem, SessionMetaLine, USER_MESSAGE_BEGIN,
};
use savfox_utils::json_to_toml::json_to_toml;
use tokio::sync::{Mutex, oneshot};
use toml::Value as TomlValue;
use tracing::warn;
use uuid::Uuid;

use crate::error_code::{INTERNAL_ERROR_CODE, INVALID_REQUEST_ERROR_CODE};
use crate::outgoing_message::OutgoingMessageSender;

type PendingInterruptQueue = Vec<RequestId>;
pub(crate) type PendingInterrupts = Arc<Mutex<HashMap<SessionId, PendingInterruptQueue>>>;

pub(crate) type PendingRollbacks = Arc<Mutex<HashMap<SessionId, RequestId>>>;

/// Per-conversation accumulation of the latest states e.g. error message while a turn runs.
#[derive(Default, Clone)]
pub(crate) struct TurnSummary {
    pub(crate) file_change_started: HashSet<String>,
    pub(crate) last_error: Option<TurnError>,
}

pub(crate) type TurnSummaryStore = Arc<Mutex<HashMap<SessionId, TurnSummary>>>;

const SESSION_LIST_DEFAULT_LIMIT: usize = 25;
const SESSION_LIST_MAX_LIMIT: usize = 100;

// Duration before a ChatGPT login attempt is abandoned.
const LOGIN_CHATGPT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
struct ActiveLogin {
    shutdown_handle: ShutdownHandle,
    login_id: Uuid,
}

#[derive(Clone, Copy, Debug)]
enum CancelLoginError {
    NotFound,
}

impl Drop for ActiveLogin {
    fn drop(&mut self) {
        self.shutdown_handle.shutdown();
    }
}

/// Handles JSON-RPC messages for Savfox sessions (and legacy conversation APIs).
pub(crate) struct SavfoxMessageProcessor {
    auth_manager: Arc<AuthManager>,
    session_manager: Arc<SessionManager>,
    outgoing: Arc<OutgoingMessageSender>,
    savfox_linux_sandbox_exe: Option<PathBuf>,
    config: Arc<Config>,
    cli_overrides: Vec<(String, TomlValue)>,
    cloud_requirements: CloudRequirementsLoader,
    conversation_listeners: HashMap<Uuid, oneshot::Sender<()>>,
    listener_session_ids_by_subscription: HashMap<Uuid, SessionId>,
    active_login: Arc<Mutex<Option<ActiveLogin>>>,
    // Queue of pending interrupt requests per conversation. We reply when TurnAborted arrives.
    pending_interrupts: PendingInterrupts,
    // Queue of pending rollback requests per conversation. We reply when SessionRollback arrives.
    pending_rollbacks: PendingRollbacks,
    turn_summary_store: TurnSummaryStore,
    pending_fuzzy_searches: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    feedback: SavfoxFeedback,
}

pub(crate) struct SavfoxMessageProcessorArgs {
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) session_manager: Arc<SessionManager>,
    pub(crate) outgoing: Arc<OutgoingMessageSender>,
    pub(crate) savfox_linux_sandbox_exe: Option<PathBuf>,
    pub(crate) config: Arc<Config>,
    pub(crate) cli_overrides: Vec<(String, TomlValue)>,
    pub(crate) cloud_requirements: CloudRequirementsLoader,
    pub(crate) feedback: SavfoxFeedback,
}

impl SavfoxMessageProcessor {
    async fn load_session(
        &self,
        session_id: &str,
    ) -> Result<(SessionId, Arc<SavfoxSession>), JSONRPCErrorError> {
        // Resolve the core conversation handle from a v2 session id string.
        let session_id = SessionId::from_string(session_id).map_err(|err| JSONRPCErrorError {
            code: INVALID_REQUEST_ERROR_CODE,
            message: format!("invalid session id: {err}"),
            data: None,
        })?;

        let session = self
            .session_manager
            .get_session(session_id)
            .await
            .map_err(|_| JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: format!("session not found: {session_id}"),
                data: None,
            })?;

        Ok((session_id, session))
    }
    pub fn new(args: SavfoxMessageProcessorArgs) -> Self {
        let SavfoxMessageProcessorArgs {
            auth_manager,
            session_manager,
            outgoing,
            savfox_linux_sandbox_exe,
            config,
            cli_overrides,
            cloud_requirements,
            feedback,
        } = args;
        Self {
            auth_manager,
            session_manager,
            outgoing,
            savfox_linux_sandbox_exe,
            config,
            cli_overrides,
            cloud_requirements,
            conversation_listeners: HashMap::new(),
            listener_session_ids_by_subscription: HashMap::new(),
            active_login: Arc::new(Mutex::new(None)),
            pending_interrupts: Arc::new(Mutex::new(HashMap::new())),
            pending_rollbacks: Arc::new(Mutex::new(HashMap::new())),
            turn_summary_store: Arc::new(Mutex::new(HashMap::new())),
            pending_fuzzy_searches: Arc::new(Mutex::new(HashMap::new())),
            feedback,
        }
    }

    async fn load_latest_config(&self) -> Result<Config, JSONRPCErrorError> {
        savfox_core::config::ConfigBuilder::default()
            .cli_overrides(self.cli_overrides.clone())
            .cloud_requirements(self.cloud_requirements.clone())
            .build()
            .await
            .map_err(|err| JSONRPCErrorError {
                code: INTERNAL_ERROR_CODE,
                message: format!("failed to reload config: {err}"),
                data: None,
            })
    }

    fn review_request_from_target(
        target: ApiReviewTarget,
    ) -> Result<(ReviewRequest, String), JSONRPCErrorError> {
        fn invalid_request(message: String) -> JSONRPCErrorError {
            JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message,
                data: None,
            }
        }

        let cleaned_target = match target {
            ApiReviewTarget::UncommittedChanges => ApiReviewTarget::UncommittedChanges,
            ApiReviewTarget::BaseBranch { branch } => {
                let branch = branch.trim().to_owned();
                if branch.is_empty() {
                    return Err(invalid_request("branch must not be empty".to_owned()));
                }
                ApiReviewTarget::BaseBranch { branch }
            }
            ApiReviewTarget::Commit { sha, title } => {
                let sha = sha.trim().to_owned();
                if sha.is_empty() {
                    return Err(invalid_request("sha must not be empty".to_owned()));
                }
                let title = title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
                ApiReviewTarget::Commit { sha, title }
            }
            ApiReviewTarget::Custom { instructions } => {
                let trimmed = instructions.trim().to_owned();
                if trimmed.is_empty() {
                    return Err(invalid_request("instructions must not be empty".to_owned()));
                }
                ApiReviewTarget::Custom {
                    instructions: trimmed,
                }
            }
        };

        let core_target = match cleaned_target {
            ApiReviewTarget::UncommittedChanges => CoreReviewTarget::UncommittedChanges,
            ApiReviewTarget::BaseBranch { branch } => CoreReviewTarget::BaseBranch { branch },
            ApiReviewTarget::Commit { sha, title } => CoreReviewTarget::Commit { sha, title },
            ApiReviewTarget::Custom { instructions } => CoreReviewTarget::Custom { instructions },
        };

        let hint = savfox_core::review_prompts::user_facing_hint(&core_target);
        let review_request = ReviewRequest {
            target: core_target,
            user_facing_hint: Some(hint.clone()),
        };

        Ok((review_request, hint))
    }

    pub async fn process_request(&mut self, request: ClientRequest) {
        match request {
            ClientRequest::Initialize { .. } => {
                panic!("Initialize should be handled in MessageProcessor");
            }
            // === v2 Session/Turn APIs ===
            ClientRequest::SessionStart { request_id, params } => {
                self.session_start(request_id, params).await;
            }
            ClientRequest::SessionResume { request_id, params } => {
                self.session_resume(request_id, params).await;
            }
            ClientRequest::SessionFork { request_id, params } => {
                self.session_fork(request_id, params).await;
            }
            ClientRequest::SessionArchive { request_id, params } => {
                self.session_archive(request_id, params).await;
            }
            ClientRequest::SessionSetName { request_id, params } => {
                self.session_set_name(request_id, params).await;
            }
            ClientRequest::SessionUnarchive { request_id, params } => {
                self.session_unarchive(request_id, params).await;
            }
            ClientRequest::SessionRollback { request_id, params } => {
                self.session_rollback(request_id, params).await;
            }
            ClientRequest::SessionList { request_id, params } => {
                self.session_list(request_id, params).await;
            }
            ClientRequest::SessionLoadedList { request_id, params } => {
                self.session_loaded_list(request_id, params).await;
            }
            ClientRequest::SessionRead { request_id, params } => {
                self.session_read(request_id, params).await;
            }
            ClientRequest::SkillsList { request_id, params } => {
                self.skills_list(request_id, params).await;
            }
            ClientRequest::AppsList { request_id, params } => {
                self.apps_list(request_id, params).await;
            }
            ClientRequest::SkillsConfigWrite { request_id, params } => {
                self.skills_config_write(request_id, params).await;
            }
            ClientRequest::TurnStart { request_id, params } => {
                self.turn_start(request_id, params).await;
            }
            ClientRequest::TurnInterrupt { request_id, params } => {
                self.turn_interrupt(request_id, params).await;
            }
            ClientRequest::ReviewStart { request_id, params } => {
                self.review_start(request_id, params).await;
            }
            ClientRequest::ModelList { request_id, params } => {
                let outgoing = self.outgoing.clone();
                let session_manager = self.session_manager.clone();
                let config = self.config.clone();

                tokio::spawn(async move {
                    Self::list_models(outgoing, session_manager, config, request_id, params).await;
                });
            }
            ClientRequest::CollaborationModeList { request_id, params } => {
                let outgoing = self.outgoing.clone();
                let session_manager = self.session_manager.clone();

                tokio::spawn(async move {
                    Self::list_collaboration_modes(outgoing, session_manager, request_id, params)
                        .await;
                });
            }
            ClientRequest::MockExperimentalMethod { request_id, params } => {
                self.mock_experimental_method(request_id, params).await;
            }
            ClientRequest::McpServerOauthLogin { request_id, params } => {
                self.mcp_server_oauth_login(request_id, params).await;
            }
            ClientRequest::McpServerRefresh { request_id, params } => {
                self.mcp_server_refresh(request_id, params).await;
            }
            ClientRequest::McpServerStatusList { request_id, params } => {
                self.list_mcp_server_status(request_id, params).await;
            }
            ClientRequest::LoginAccount { request_id, params } => {
                self.login_v2(request_id, params).await;
            }
            ClientRequest::LogoutAccount {
                request_id,
                params: _,
            } => {
                self.logout_v2(request_id).await;
            }
            ClientRequest::CancelLoginAccount { request_id, params } => {
                self.cancel_login_v2(request_id, params).await;
            }
            ClientRequest::GetAccount { request_id, params } => {
                self.get_account(request_id, params).await;
            }
            ClientRequest::FuzzyFileSearch { request_id, params } => {
                self.fuzzy_file_search(request_id, params).await;
            }
            ClientRequest::OneOffCommandExec { request_id, params } => {
                self.exec_one_off_command(request_id, params).await;
            }
            ClientRequest::ConfigRead { .. }
            | ClientRequest::ConfigValueWrite { .. }
            | ClientRequest::ConfigBatchWrite { .. } => {
                warn!("Config request reached SavfoxMessageProcessor unexpectedly");
            }
            ClientRequest::ConfigRequirementsRead { .. } => {
                warn!("ConfigRequirementsRead request reached SavfoxMessageProcessor unexpectedly");
            }
            ClientRequest::GetAccountRateLimits {
                request_id,
                params: _,
            } => {
                self.get_account_rate_limits(request_id).await;
            }
            ClientRequest::FeedbackUpload { request_id, params } => {
                self.upload_feedback(request_id, params).await;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_session_config_overrides(
        &self,
        model: Option<String>,
        model_provider: Option<String>,
        cwd: Option<String>,
        approval_policy: Option<savfox_app_server_protocol::AskForApproval>,
        sandbox: Option<SandboxMode>,
        base_instructions: Option<String>,
        developer_instructions: Option<String>,
        personality: Option<Personality>,
    ) -> ConfigOverrides {
        ConfigOverrides {
            model,
            model_provider,
            cwd: cwd.map(PathBuf::from),
            approval_policy: approval_policy
                .map(savfox_app_server_protocol::AskForApproval::to_core),
            sandbox_mode: sandbox.map(SandboxMode::to_core),
            savfox_linux_sandbox_exe: self.savfox_linux_sandbox_exe.clone(),
            base_instructions,
            developer_instructions,
            personality,
            ..Default::default()
        }
    }

    async fn send_invalid_request_error(&self, request_id: RequestId, message: String) {
        let error = JSONRPCErrorError {
            code: INVALID_REQUEST_ERROR_CODE,
            message,
            data: None,
        };
        self.outgoing.send_error(request_id, error).await;
    }

    async fn send_internal_error(&self, request_id: RequestId, message: String) {
        let error = JSONRPCErrorError {
            code: INTERNAL_ERROR_CODE,
            message,
            data: None,
        };
        self.outgoing.send_error(request_id, error).await;
    }
}

fn validate_dynamic_tools(
    tools: &[ApiDynamicToolSpec],
    mcp_tool_names: &HashSet<String>,
) -> Result<(), String> {
    let mut seen = HashSet::new();
    for tool in tools {
        let name = tool.name.trim();
        if name.is_empty() {
            return Err("dynamic tool name must not be empty".to_owned());
        }
        if name != tool.name {
            return Err(format!(
                "dynamic tool name has leading/trailing whitespace: {}",
                tool.name
            ));
        }
        if name == "mcp" || name.starts_with("mcp__") {
            return Err(format!("dynamic tool name is reserved: {name}"));
        }
        if mcp_tool_names.contains(name) {
            return Err(format!("dynamic tool name conflicts with MCP tool: {name}"));
        }
        if !seen.insert(name.to_owned()) {
            return Err(format!("duplicate dynamic tool name: {name}"));
        }

        if let Err(err) = savfox_core::parse_tool_input_schema(&tool.input_schema) {
            return Err(format!(
                "dynamic tool input schema is not supported for {name}: {err}"
            ));
        }
    }
    Ok(())
}

/// Derive the effective [`Config`] by layering three override sources.
///
/// Precedence (lowest to highest):
/// - `cli_overrides`: process-wide startup `--config` flags.
/// - `request_overrides`: per-request dotted-path overrides (`params.config`), converted
///   JSON->TOML.
/// - `typesafe_overrides`: Request objects such as `NewSessionParams` and `SessionStartParams`
///   support a limited set of _explicit_ config overrides, so `typesafe_overrides` is a
///   `ConfigOverrides` derived from the respective request object. Because the overrides are
///   defined explicitly in the `*Params`, this takes priority over the more general "bag of config
///   options" provided by `cli_overrides` and `request_overrides`.
async fn derive_config_from_params(
    cli_overrides: &[(String, TomlValue)],
    request_overrides: Option<HashMap<String, serde_json::Value>>,
    typesafe_overrides: ConfigOverrides,
    cloud_requirements: &CloudRequirementsLoader,
) -> std::io::Result<Config> {
    let merged_cli_overrides = cli_overrides
        .iter()
        .cloned()
        .chain(
            request_overrides
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| (k, json_to_toml(v))),
        )
        .collect::<Vec<_>>();

    savfox_core::config::ConfigBuilder::default()
        .cli_overrides(merged_cli_overrides)
        .harness_overrides(typesafe_overrides)
        .cloud_requirements(cloud_requirements.clone())
        .build()
        .await
}

async fn derive_config_for_cwd(
    cli_overrides: &[(String, TomlValue)],
    request_overrides: Option<HashMap<String, serde_json::Value>>,
    typesafe_overrides: ConfigOverrides,
    cwd: Option<PathBuf>,
    cloud_requirements: &CloudRequirementsLoader,
) -> std::io::Result<Config> {
    let merged_cli_overrides = cli_overrides
        .iter()
        .cloned()
        .chain(
            request_overrides
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| (k, json_to_toml(v))),
        )
        .collect::<Vec<_>>();

    savfox_core::config::ConfigBuilder::default()
        .cli_overrides(merged_cli_overrides)
        .harness_overrides(typesafe_overrides)
        .fallback_cwd(cwd)
        .cloud_requirements(cloud_requirements.clone())
        .build()
        .await
}

pub(crate) async fn read_summary_from_rollout(
    path: &Path,
    fallback_provider: &str,
) -> std::io::Result<ConversationSummary> {
    let head = read_head_for_summary(path).await?;

    let Some(first) = head.first() else {
        return Err(IoError::other(format!(
            "rollout at {} is empty",
            path.display()
        )));
    };

    let session_meta_line =
        serde_json::from_value::<SessionMetaLine>(first.clone()).map_err(|_| {
            IoError::other(format!(
                "rollout at {} does not start with session metadata",
                path.display()
            ))
        })?;
    let SessionMetaLine {
        meta: session_meta,
        git,
    } = session_meta_line;

    let created_at = if session_meta.timestamp.is_empty() {
        None
    } else {
        Some(session_meta.timestamp.as_str())
    };
    let updated_at = read_updated_at(path, created_at).await;
    if let Some(summary) = extract_conversation_summary(
        path.to_path_buf(),
        &head,
        &session_meta,
        git.as_ref(),
        fallback_provider,
        updated_at.clone(),
    ) {
        return Ok(summary);
    }

    let timestamp = if session_meta.timestamp.is_empty() {
        None
    } else {
        Some(session_meta.timestamp.clone())
    };
    let model_provider = session_meta
        .model_provider_id()
        .map(str::to_string)
        .unwrap_or_else(|| fallback_provider.to_owned());
    let git_info = git.as_ref().map(map_git_info);
    let updated_at = updated_at.or_else(|| timestamp.clone());

    Ok(ConversationSummary {
        conversation_id: session_meta.id,
        timestamp,
        updated_at,
        path: path.to_path_buf(),
        preview: String::new(),
        model_provider,
        cwd: session_meta.cwd,
        cli_version: session_meta.cli_version,
        source: session_meta.source,
        git_info,
    })
}

pub(crate) async fn read_event_msgs_from_rollout(
    path: &Path,
) -> std::io::Result<Vec<savfox_protocol::protocol::EventMsg>> {
    let items = match RolloutRecorder::get_rollout_history(path).await? {
        InitialHistory::New => Vec::new(),
        InitialHistory::Forked(items) => items,
        InitialHistory::Resumed(resumed) => resumed.history,
    };

    Ok(items
        .into_iter()
        .filter_map(|item| match item {
            RolloutItem::EventMsg(event) => Some(event),
            _ => None,
        })
        .collect())
}

fn extract_conversation_summary(
    path: PathBuf,
    head: &[serde_json::Value],
    session_meta: &SessionMeta,
    git: Option<&CoreGitInfo>,
    fallback_provider: &str,
    updated_at: Option<String>,
) -> Option<ConversationSummary> {
    let user_messages: Vec<String> = head
        .iter()
        .filter_map(|value| serde_json::from_value::<ResponseItem>(value.clone()).ok())
        .filter_map(|item| match savfox_core::parse_turn_item(&item) {
            Some(TurnItem::UserMessage(user)) => Some(user.message()),
            _ => None,
        })
        .collect();

    // Prefer the user message that contains the actual user request marker.
    let preview = user_messages
        .iter()
        .find(|msg| msg.contains(USER_MESSAGE_BEGIN))
        .or(user_messages.first())?;

    let preview = match preview.find(USER_MESSAGE_BEGIN) {
        Some(idx) => preview[idx + USER_MESSAGE_BEGIN.len()..].trim(),
        None => preview.as_str(),
    };

    let timestamp = if session_meta.timestamp.is_empty() {
        None
    } else {
        Some(session_meta.timestamp.clone())
    };
    let conversation_id = session_meta.id;
    let model_provider = session_meta
        .model_provider_id()
        .map(str::to_string)
        .unwrap_or_else(|| fallback_provider.to_owned());
    let git_info = git.map(map_git_info);
    let updated_at = updated_at.or_else(|| timestamp.clone());

    Some(ConversationSummary {
        conversation_id,
        timestamp,
        updated_at,
        path,
        preview: preview.to_owned(),
        model_provider,
        cwd: session_meta.cwd.clone(),
        cli_version: session_meta.cli_version.clone(),
        source: session_meta.source.clone(),
        git_info,
    })
}

fn map_git_info(git_info: &CoreGitInfo) -> ConversationGitInfo {
    ConversationGitInfo {
        sha: git_info.commit_hash.clone(),
        branch: git_info.branch.clone(),
        origin_url: git_info.repository_url.clone(),
    }
}

fn parse_datetime(timestamp: Option<&str>) -> Option<DateTime<Utc>> {
    timestamp.and_then(|ts| {
        chrono::DateTime::parse_from_rfc3339(ts)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc))
    })
}

async fn read_updated_at(path: &Path, created_at: Option<&str>) -> Option<String> {
    let updated_at = tokio::fs::metadata(path)
        .await
        .ok()
        .and_then(|meta| meta.modified().ok())
        .map(|modified| {
            let updated_at: DateTime<Utc> = modified.into();
            updated_at.to_rfc3339_opts(SecondsFormat::Secs, true)
        });
    updated_at.or_else(|| created_at.map(str::to_string))
}

fn build_ephemeral_session(
    session_id: SessionId,
    config_snapshot: &SessionConfigSnapshot,
) -> Session {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    Session {
        id: session_id.to_string(),
        preview: String::new(),
        model_provider: config_snapshot.model_provider_id.clone(),
        created_at: now,
        updated_at: now,
        path: None,
        cwd: config_snapshot.cwd.clone(),
        cli_version: env!("CARGO_PKG_VERSION").to_owned(),
        source: config_snapshot.session_source.clone().into(),
        git_info: None,
        turns: Vec::new(),
    }
}

pub(crate) fn summary_to_session(summary: ConversationSummary) -> Session {
    let ConversationSummary {
        conversation_id,
        path,
        preview,
        timestamp,
        updated_at,
        model_provider,
        cwd,
        cli_version,
        source,
        git_info,
    } = summary;

    let created_at = parse_datetime(timestamp.as_deref());
    let updated_at = parse_datetime(updated_at.as_deref()).or(created_at);
    let git_info = git_info.map(|info| savfox_app_server_protocol::GitInfo {
        sha: info.sha,
        branch: info.branch,
        origin_url: info.origin_url,
    });

    Session {
        id: conversation_id.to_string(),
        preview,
        model_provider,
        created_at: created_at.map(|dt| dt.timestamp()).unwrap_or(0),
        updated_at: updated_at.map(|dt| dt.timestamp()).unwrap_or(0),
        path: Some(path),
        cwd,
        cli_version,
        source: source.into(),
        git_info,
        turns: Vec::new(),
    }
}

fn skills_to_info(
    skills: &[savfox_core::skills::SkillMetadata],
    disabled_paths: &std::collections::HashSet<PathBuf>,
) -> Vec<savfox_app_server_protocol::SkillMetadata> {
    skills
        .iter()
        .map(|skill| {
            let enabled = !disabled_paths.contains(&skill.path);
            savfox_app_server_protocol::SkillMetadata {
                name: skill.name.clone(),
                description: skill.description.clone(),
                short_description: skill.short_description.clone(),
                interface: skill.interface.clone().map(|interface| {
                    savfox_app_server_protocol::SkillInterface {
                        name: interface.name,
                        short_description: interface.short_description,
                        icon_small: interface.icon_small,
                        icon_large: interface.icon_large,
                        brand_color: interface.brand_color,
                        default_prompt: interface.default_prompt,
                    }
                }),
                dependencies: skill.dependencies.clone().map(|dependencies| {
                    savfox_app_server_protocol::SkillDependencies {
                        tools: dependencies
                            .tools
                            .into_iter()
                            .map(|tool| savfox_app_server_protocol::SkillToolDependency {
                                r#type: tool.r#type,
                                value: tool.value,
                                description: tool.description,
                                transport: tool.transport,
                                command: tool.command,
                                url: tool.url,
                            })
                            .collect(),
                    }
                }),
                path: skill.path.clone(),
                scope: skill.scope.into(),
                enabled,
            }
        })
        .collect()
}

fn errors_to_info(
    errors: &[savfox_core::skills::SkillError],
) -> Vec<savfox_app_server_protocol::SkillErrorInfo> {
    errors
        .iter()
        .map(|err| savfox_app_server_protocol::SkillErrorInfo {
            path: err.path.clone(),
            message: err.message.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use savfox_protocol::protocol::SessionSource;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn validate_dynamic_tools_rejects_unsupported_input_schema() {
        let tools = vec![ApiDynamicToolSpec {
            name: "my_tool".to_owned(),
            description: "test".to_owned(),
            input_schema: json!({"type": "null"}),
        }];
        let err = validate_dynamic_tools(&tools, &HashSet::new()).expect_err("invalid schema");
        assert!(err.contains("my_tool"), "unexpected error: {err}");
    }

    #[test]
    fn validate_dynamic_tools_accepts_sanitizable_input_schema() {
        let tools = vec![ApiDynamicToolSpec {
            name: "my_tool".to_owned(),
            description: "test".to_owned(),
            // Missing `type` is common; core sanitizes these to a supported schema.
            input_schema: json!({"properties": {}}),
        }];
        validate_dynamic_tools(&tools, &HashSet::new()).expect("valid schema");
    }

    #[test]
    fn extract_conversation_summary_prefers_plain_user_messages() -> Result<()> {
        let conversation_id = SessionId::from_string("3f941c35-29b3-493b-b0a4-e25800d9aeb0")?;
        let timestamp = Some("2025-09-05T16:53:11.850Z".to_owned());
        let path = PathBuf::from("rollout.jsonl");

        let head = vec![
            json!({
                "id": conversation_id.to_string(),
                "timestamp": timestamp,
                "cwd": "/",
                "originator": "savfox",
                "cli_version": "0.0.0",
                "model_provider": "test-provider"
            }),
            json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "<user_instructions>\n<AGENTS.md contents>\n</user_instructions>".to_owned(),
                }],
            }),
            json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("<prior context> {USER_MESSAGE_BEGIN}Count to 5"),
                }],
            }),
        ];

        let session_meta = serde_json::from_value::<SessionMeta>(head[0].clone())?;

        let summary = extract_conversation_summary(
            path.clone(),
            &head,
            &session_meta,
            None,
            "test-provider",
            timestamp.clone(),
        )
        .expect("summary");

        let expected = ConversationSummary {
            conversation_id,
            timestamp: timestamp.clone(),
            updated_at: timestamp,
            path,
            preview: "Count to 5".to_owned(),
            model_provider: "test-provider".to_owned(),
            cwd: PathBuf::from("/"),
            cli_version: "0.0.0".to_owned(),
            source: SessionSource::VSCode,
            git_info: None,
        };

        assert_eq!(summary, expected);
        Ok(())
    }

    #[tokio::test]
    async fn read_summary_from_rollout_returns_empty_preview_when_no_user_message() -> Result<()> {
        use std::fs;
        use std::fs::FileTimes;

        use savfox_protocol::protocol::{RolloutItem, RolloutLine, SessionMetaLine};

        let temp_dir = TempDir::new()?;
        let path = temp_dir.path().join("rollout.jsonl");

        let conversation_id = SessionId::from_string("bfd12a78-5900-467b-9bc5-d3d35df08191")?;
        let timestamp = "2025-09-05T16:53:11.850Z".to_owned();

        let session_meta = SessionMeta {
            id: conversation_id,
            timestamp: timestamp.clone(),
            model_provider: None,
            ..SessionMeta::default()
        };

        let line = RolloutLine {
            timestamp: timestamp.clone(),
            item: RolloutItem::SessionMeta(SessionMetaLine {
                meta: session_meta.clone(),
                git: None,
            }),
        };

        fs::write(&path, format!("{}\n", serde_json::to_string(&line)?))?;
        let parsed = chrono::DateTime::parse_from_rfc3339(&timestamp)?.with_timezone(&Utc);
        let times = FileTimes::new().set_modified(parsed.into());
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)?
            .set_times(times)?;

        let summary = read_summary_from_rollout(path.as_path(), "fallback").await?;

        let expected = ConversationSummary {
            conversation_id,
            timestamp: Some(timestamp.clone()),
            updated_at: Some("2025-09-05T16:53:11Z".to_owned()),
            path: path.clone(),
            preview: String::new(),
            model_provider: "fallback".to_owned(),
            cwd: PathBuf::new(),
            cli_version: String::new(),
            source: SessionSource::VSCode,
            git_info: None,
        };

        assert_eq!(summary, expected);
        Ok(())
    }
}
