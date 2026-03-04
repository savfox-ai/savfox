use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};
use savfox_ansi_escape::ansi_escape_line;
use savfox_app_server_protocol::ConfigLayerSource;
use savfox_core::auth::CLIENT_ID;
use savfox_core::config::edit::{ConfigEdit, ConfigEditsBuilder};
use savfox_core::config::provider_store::persist_provider_connection;
use savfox_core::config::{Config, ConfigBuilder, ConfigOverrides};
use savfox_core::config_loader::ConfigLayerStackOrdering;
use savfox_core::features::Feature;
use savfox_core::models_manager::manager::RefreshStrategy;
use savfox_core::models_manager::model_presets::{
    HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG, HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG,
};
use savfox_core::protocol::{
    AskForApproval, DeprecationNoticeEvent, Event, EventMsg, FinalOutput, ListSkillsResponseEvent,
    Op, SandboxPolicy, SessionSource, SkillErrorInfo, TokenUsage,
};
#[cfg(target_os = "windows")]
use savfox_core::windows_sandbox::WindowsSandboxLevelExt;
use savfox_core::{AuthManager, SavfoxAuth, SessionManager, built_in_model_providers};
use savfox_login_oauth::{
    ServerOptions, complete_device_code_login, request_device_code, run_login_server,
};
use savfox_otel::OtelManager;
use savfox_protocol::SessionId;
use savfox_protocol::config_types::Personality;
#[cfg(target_os = "windows")]
use savfox_protocol::config_types::WindowsSandboxLevel;
use savfox_protocol::items::TurnItem;
use savfox_protocol::openai_models::{
    ModelPreset, ModelUpgrade, ReasoningEffort as ReasoningEffortConfig,
};
use savfox_protocol::protocol::SessionConfiguredEvent;
use savfox_utils_absolute_path::AbsolutePathBuf;
use tokio::select;
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::{Mutex, broadcast, mpsc};
use toml::Value as TomlValue;
use toml_edit::value as toml_edit_value;

use crate::app_backtrack::BacktrackState;
#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxEnableMode;
#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxFallbackReason;
use crate::app_event::{AppEvent, ExitMode, OpenAiConnectAuthMethod};
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::bottom_pane::{ApprovalRequest, FeedbackAudience, SelectionItem, SelectionViewParams};
use crate::chat_screen::{ChatScreen, ExternalEditorState};
use crate::cwd_prompt::CwdPromptAction;
use crate::diff_render::DiffSummary;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::file_search::FileSearchManager;
use crate::history_cell::HistoryCell;
#[cfg(not(debug_assertions))]
use crate::history_cell::UpdateAvailableHistoryCell;
use crate::model_migration::{
    ModelMigrationOutcome, migration_copy_for_models, run_model_migration_prompt,
};
use crate::pager_overlay::Overlay;
use crate::provider_connect::{ProviderConnectRuntimeAuth, connect_provider, select_default_model};
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::Renderable;
use crate::resume_picker::SessionSelection;
use crate::tui::TuiEvent;
use crate::update_action::UpdateAction;
use crate::{external_editor, history_cell, tui};

const EXTERNAL_EDITOR_HINT: &str = "Save and close external editor to continue.";
const THREAD_EVENT_CHANNEL_CAPACITY: usize = 32768;

#[derive(Debug, Clone)]
pub struct AppExitInfo {
    pub token_usage: TokenUsage,
    pub session_id: Option<SessionId>,
    pub session_name: Option<String>,
    pub model_display: Option<String>,
    pub directory: Option<PathBuf>,
    pub update_action: Option<UpdateAction>,
    pub exit_reason: ExitReason,
}

impl AppExitInfo {
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            token_usage: TokenUsage::default(),
            session_id: None,
            session_name: None,
            model_display: None,
            directory: None,
            update_action: None,
            exit_reason: ExitReason::Fatal(message.into()),
        }
    }
}

#[derive(Debug)]
pub(crate) enum AppRunControl {
    Continue,
    Exit(ExitReason),
}

#[derive(Debug, Clone)]
pub enum ExitReason {
    UserRequested,
    Fatal(String),
}

fn session_summary(
    token_usage: TokenUsage,
    session_id: Option<SessionId>,
    session_name: Option<String>,
) -> Option<SessionSummary> {
    let usage_line = if token_usage.is_zero() {
        None
    } else {
        Some(FinalOutput::from(token_usage).to_string())
    };
    let resume_command = resume_command_for_summary(session_name.as_deref(), session_id);

    if usage_line.is_none() && resume_command.is_none() {
        return None;
    }

    Some(SessionSummary {
        usage_line,
        resume_command,
    })
}

fn resume_command_for_summary(
    session_name: Option<&str>,
    session_id: Option<SessionId>,
) -> Option<String> {
    let session_name = session_name.map(str::trim).filter(|name| !name.is_empty());
    if let Some(id) = session_id {
        savfox_core::util::resume_command(None, Some(id))
    } else {
        savfox_core::util::resume_command(session_name, None)
    }
}

fn errors_for_cwd(cwd: &Path, response: &ListSkillsResponseEvent) -> Vec<SkillErrorInfo> {
    response
        .skills
        .iter()
        .find(|entry| entry.cwd.as_path() == cwd)
        .map(|entry| entry.errors.clone())
        .unwrap_or_default()
}

fn emit_skill_load_warnings(app_event_tx: &AppEventSender, errors: &[SkillErrorInfo]) {
    if errors.is_empty() {
        return;
    }

    let error_count = errors.len();
    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        crate::history_cell::new_warning_event(format!(
            "Skipped loading {error_count} skill(s) due to invalid SKILL.md files."
        )),
    )));

    for error in errors {
        let path = error.path.display();
        let message = error.message.as_str();
        app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            crate::history_cell::new_warning_event(format!("{path}: {message}")),
        )));
    }
}

fn emit_deprecation_notice(app_event_tx: &AppEventSender, notice: Option<DeprecationNoticeEvent>) {
    let Some(DeprecationNoticeEvent { summary, details }) = notice else {
        return;
    };
    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        crate::history_cell::new_deprecation_notice(summary, details),
    )));
}

fn emit_project_config_warnings(app_event_tx: &AppEventSender, config: &Config) {
    let mut disabled_folders = Vec::new();

    for layer in config
        .config_layer_stack
        .get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, true)
    {
        let ConfigLayerSource::Project { dot_savfox_folder } = &layer.name else {
            continue;
        };
        if layer.disabled_reason.is_none() {
            continue;
        }
        disabled_folders.push((
            dot_savfox_folder.as_path().display().to_string(),
            layer
                .disabled_reason
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "config.toml is disabled.".to_string()),
        ));
    }

    if disabled_folders.is_empty() {
        return;
    }

    let mut message = concat!(
        "Project config.toml files are disabled in the following folders. ",
        "Settings in those files are ignored, but skills and exec policies still load.\n",
    )
    .to_string();
    for (index, (folder, reason)) in disabled_folders.iter().enumerate() {
        let display_index = index + 1;
        message.push_str(&format!("    {display_index}. {folder}\n"));
        message.push_str(&format!("       {reason}\n"));
    }

    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        history_cell::new_warning_event(message),
    )));
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSummary {
    usage_line: Option<String>,
    resume_command: Option<String>,
}

fn session_summary_lines(summary: SessionSummary) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(usage_line) = summary.usage_line {
        lines.push(usage_line.into());
    }
    if let Some(command) = summary.resume_command {
        lines.push(vec!["Continue with ".into(), command.cyan()].into());
    }
    lines
}

#[derive(Debug, Clone)]
struct SessionEventSnapshot {
    session_configured: Option<Event>,
    events: Vec<Event>,
}

#[derive(Debug)]
struct SessionEventStore {
    session_configured: Option<Event>,
    buffer: VecDeque<Event>,
    user_message_ids: HashSet<String>,
    capacity: usize,
    active: bool,
}

impl SessionEventStore {
    fn new(capacity: usize) -> Self {
        Self {
            session_configured: None,
            buffer: VecDeque::new(),
            user_message_ids: HashSet::new(),
            capacity,
            active: false,
        }
    }

    fn new_with_session_configured(capacity: usize, event: Event) -> Self {
        let mut store = Self::new(capacity);
        store.session_configured = Some(event);
        store
    }

    fn push_event(&mut self, event: Event) {
        match &event.msg {
            EventMsg::SessionConfigured(_) => {
                self.session_configured = Some(event);
                return;
            }
            EventMsg::ItemCompleted(completed) => {
                if let TurnItem::UserMessage(item) = &completed.item {
                    if !event.id.is_empty() && self.user_message_ids.contains(&event.id) {
                        return;
                    }
                    let legacy = Event {
                        id: event.id,
                        msg: item.as_legacy_event(),
                    };
                    self.push_legacy_event(legacy);
                    return;
                }
            }
            _ => {}
        }

        self.push_legacy_event(event);
    }

    fn push_legacy_event(&mut self, event: Event) {
        if let EventMsg::UserMessage(_) = &event.msg
            && !event.id.is_empty()
            && !self.user_message_ids.insert(event.id.clone())
        {
            return;
        }
        self.buffer.push_back(event);
        if self.buffer.len() > self.capacity
            && let Some(removed) = self.buffer.pop_front()
            && matches!(removed.msg, EventMsg::UserMessage(_))
            && !removed.id.is_empty()
        {
            self.user_message_ids.remove(&removed.id);
        }
    }

    fn snapshot(&self) -> SessionEventSnapshot {
        SessionEventSnapshot {
            session_configured: self.session_configured.clone(),
            events: self.buffer.iter().cloned().collect(),
        }
    }
}

#[derive(Debug)]
struct SessionEventChannel {
    sender: mpsc::Sender<Event>,
    receiver: Option<mpsc::Receiver<Event>>,
    store: Arc<Mutex<SessionEventStore>>,
}

impl SessionEventChannel {
    fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(SessionEventStore::new(capacity))),
        }
    }

    fn new_with_session_configured(capacity: usize, event: Event) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(SessionEventStore::new_with_session_configured(
                capacity, event,
            ))),
        }
    }
}

fn normalized_session_name(name: Option<&str>) -> Option<String> {
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn session_name_from_event_store(
    store: &SessionEventStore,
    session_id: SessionId,
) -> Option<String> {
    for event in store.buffer.iter().rev() {
        if let EventMsg::SessionNameUpdated(updated) = &event.msg
            && updated.session_id == session_id
        {
            return normalized_session_name(updated.session_name.as_deref());
        }
    }

    if let Some(event) = store.session_configured.as_ref()
        && let EventMsg::SessionConfigured(configured) = &event.msg
        && configured.session_id == session_id
    {
        return normalized_session_name(configured.session_name.as_deref());
    }

    None
}

fn should_show_model_migration_prompt(
    current_model: &str,
    target_model: &str,
    seen_migrations: &BTreeMap<String, String>,
    available_models: &[ModelPreset],
) -> bool {
    if target_model == current_model {
        return false;
    }

    if let Some(seen_target) = seen_migrations.get(current_model)
        && seen_target == target_model
    {
        return false;
    }

    if available_models
        .iter()
        .any(|preset| preset.slug == current_model && preset.upgrade.is_some())
    {
        return true;
    }

    if available_models
        .iter()
        .any(|preset| preset.upgrade.as_ref().map(|u| u.id.as_str()) == Some(target_model))
    {
        return true;
    }

    false
}

fn migration_prompt_hidden(config: &Config, migration_config_key: &str) -> bool {
    match migration_config_key {
        HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG => config
            .notices
            .hide_gpt_5_1_savfox_max_migration_prompt
            .unwrap_or(false),
        HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG => {
            config.notices.hide_gpt5_1_migration_prompt.unwrap_or(false)
        }
        _ => false,
    }
}

fn target_preset_for_upgrade<'a>(
    available_models: &'a [ModelPreset],
    target_model: &str,
) -> Option<&'a ModelPreset> {
    available_models
        .iter()
        .find(|preset| preset.slug == target_model)
}

async fn handle_model_migration_prompt_if_needed(
    tui: &mut tui::Tui,
    config: &mut Config,
    model: &str,
    app_event_tx: &AppEventSender,
    available_models: Vec<ModelPreset>,
) -> Option<AppExitInfo> {
    let upgrade = available_models
        .iter()
        .find(|preset| preset.slug == model)
        .and_then(|preset| preset.upgrade.as_ref());

    if let Some(ModelUpgrade {
        id: target_model,
        reasoning_effort_mapping,
        migration_config_key,
        model_link,
        upgrade_copy,
        migration_markdown,
    }) = upgrade
    {
        if migration_prompt_hidden(config, migration_config_key.as_str()) {
            return None;
        }

        let target_model = target_model.to_string();
        if !should_show_model_migration_prompt(
            model,
            &target_model,
            &config.notices.model_migrations,
            &available_models,
        ) {
            return None;
        }

        let current_preset = available_models.iter().find(|preset| preset.slug == model);
        let target_preset = target_preset_for_upgrade(&available_models, &target_model);
        let target_preset = target_preset?;
        let target_display_name = target_preset.display_name.clone();
        let heading_label = if target_display_name == model {
            target_model.clone()
        } else {
            target_display_name.clone()
        };
        let target_description =
            (!target_preset.description.is_empty()).then(|| target_preset.description.clone());
        let can_opt_out = current_preset.is_some();
        let prompt_copy = migration_copy_for_models(
            model,
            &target_model,
            model_link.clone(),
            upgrade_copy.clone(),
            migration_markdown.clone(),
            heading_label,
            target_description,
            can_opt_out,
        );
        match run_model_migration_prompt(tui, prompt_copy).await {
            ModelMigrationOutcome::Accepted => {
                app_event_tx.send(AppEvent::PersistModelMigrationPromptAcknowledged {
                    from_model: model.to_string(),
                    to_model: target_model.clone(),
                });

                let mapped_effort = if let Some(reasoning_effort_mapping) = reasoning_effort_mapping
                    && let Some(reasoning_effort) = config.model_reasoning_effort
                {
                    reasoning_effort_mapping
                        .get(&reasoning_effort)
                        .cloned()
                        .or(config.model_reasoning_effort)
                } else {
                    config.model_reasoning_effort
                };

                config.model = Some(target_model.clone());
                config.model_reasoning_effort = mapped_effort;
                app_event_tx.send(AppEvent::UpdateModel(target_model.clone()));
                app_event_tx.send(AppEvent::UpdateReasoningEffort(mapped_effort));
                app_event_tx.send(AppEvent::PersistModelSelection {
                    model: target_model.clone(),
                    effort: mapped_effort,
                });
            }
            ModelMigrationOutcome::Rejected => {
                app_event_tx.send(AppEvent::PersistModelMigrationPromptAcknowledged {
                    from_model: model.to_string(),
                    to_model: target_model.clone(),
                });
            }
            ModelMigrationOutcome::Exit => {
                return Some(AppExitInfo {
                    token_usage: TokenUsage::default(),
                    session_id: None,
                    session_name: None,
                    model_display: None,
                    directory: None,
                    update_action: None,
                    exit_reason: ExitReason::UserRequested,
                });
            }
        }
    }

    None
}

pub(crate) struct App {
    pub(crate) server: Arc<SessionManager>,
    pub(crate) otel_manager: OtelManager,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) chat_screen: ChatScreen,
    pub(crate) auth_manager: Arc<AuthManager>,
    /// Config is stored here so we can recreate ChatScreens as needed.
    pub(crate) config: Config,
    pub(crate) active_profile: Option<String>,
    cli_kv_overrides: Vec<(String, TomlValue)>,
    harness_overrides: ConfigOverrides,
    runtime_approval_policy_override: Option<AskForApproval>,
    runtime_sandbox_policy_override: Option<SandboxPolicy>,

    pub(crate) file_search: FileSearchManager,

    pub(crate) transcript_cells: Vec<Arc<dyn HistoryCell>>,

    // Pager overlay state (Transcript or Static like Diff)
    pub(crate) overlay: Option<Overlay>,
    pub(crate) deferred_history_lines: Vec<Line<'static>>,
    has_emitted_history_lines: bool,

    pub(crate) enhanced_keys_supported: bool,

    /// Controls the animation session that sends CommitTick events.
    pub(crate) commit_anim_running: Arc<AtomicBool>,

    // Esc-backtracking state grouped
    pub(crate) backtrack: crate::app_backtrack::BacktrackState,
    /// When set, the next draw re-renders the transcript into terminal scrollback once.
    ///
    /// This is used after a confirmed session rollback to ensure scrollback reflects the trimmed
    /// transcript cells.
    pub(crate) backtrack_render_pending: bool,
    pub(crate) feedback: savfox_feedback::SavfoxFeedback,
    feedback_audience: FeedbackAudience,
    /// Set when the user confirms an update; propagated on exit.
    pub(crate) pending_update_action: Option<UpdateAction>,

    /// Ignore the next ShutdownComplete event when we're intentionally
    /// stopping a session (e.g., before starting a new one).
    suppress_shutdown_complete: bool,

    windows_sandbox: WindowsSandboxState,

    session_event_channels: HashMap<SessionId, SessionEventChannel>,
    active_session_id: Option<SessionId>,
    active_session_rx: Option<mpsc::Receiver<Event>>,
    primary_session_id: Option<SessionId>,
    primary_session_configured: Option<SessionConfiguredEvent>,
    pending_primary_events: VecDeque<Event>,
}

#[derive(Default)]
struct WindowsSandboxState {
    setup_started_at: Option<Instant>,
    // One-shot suppression of the next world-writable scan after user confirmation.
    skip_world_writable_scan_once: bool,
}

fn normalize_harness_overrides_for_cwd(
    mut overrides: ConfigOverrides,
    base_cwd: &Path,
) -> Result<ConfigOverrides> {
    if overrides.additional_writable_roots.is_empty() {
        return Ok(overrides);
    }

    let mut normalized = Vec::with_capacity(overrides.additional_writable_roots.len());
    for root in overrides.additional_writable_roots.drain(..) {
        let absolute = AbsolutePathBuf::resolve_path_against_base(root, base_cwd)?;
        normalized.push(absolute.into_path_buf());
    }
    overrides.additional_writable_roots = normalized;
    Ok(overrides)
}

impl App {
    pub fn chat_screen_init_for_forked_or_resumed_session(
        &self,
        tui: &mut tui::Tui,
        cfg: savfox_core::config::Config,
    ) -> crate::chat_screen::ChatScreenInit {
        crate::chat_screen::ChatScreenInit {
            config: cfg,
            frame_requester: tui.frame_requester(),
            app_event_tx: self.app_event_tx.clone(),
            // Fork/resume bootstraps here don't carry any prefilled message content.
            initial_user_message: None,
            enhanced_keys_supported: self.enhanced_keys_supported,
            auth_manager: self.auth_manager.clone(),
            models_manager: self.server.get_models_manager(),
            feedback: self.feedback.clone(),
            is_first_run: false,
            feedback_audience: self.feedback_audience,
            model: Some(self.chat_screen.current_model().to_string()),
            otel_manager: self.otel_manager.clone(),
        }
    }

    async fn rebuild_config_for_cwd(&self, cwd: PathBuf) -> Result<Config> {
        let mut overrides = self.harness_overrides.clone();
        overrides.cwd = Some(cwd.clone());
        let cwd_display = cwd.display().to_string();
        ConfigBuilder::default()
            .savfox_home(self.config.savfox_home.clone())
            .cli_overrides(self.cli_kv_overrides.clone())
            .harness_overrides(overrides)
            .build()
            .await
            .wrap_err_with(|| format!("Failed to rebuild config for cwd {cwd_display}"))
    }

    fn apply_runtime_policy_overrides(&mut self, config: &mut Config) {
        if let Some(policy) = self.runtime_approval_policy_override.as_ref()
            && let Err(err) = config.approval_policy.set(*policy)
        {
            tracing::warn!(%err, "failed to carry forward approval policy override");
            self.chat_screen.add_error_message(format!(
                "Failed to carry forward approval policy override: {err}"
            ));
        }
        if let Some(policy) = self.runtime_sandbox_policy_override.as_ref()
            && let Err(err) = config.sandbox_policy.set(policy.clone())
        {
            tracing::warn!(%err, "failed to carry forward sandbox policy override");
            self.chat_screen.add_error_message(format!(
                "Failed to carry forward sandbox policy override: {err}"
            ));
        }
    }

    fn openai_connect_server_options(&self) -> ServerOptions {
        ServerOptions::new(
            self.config.savfox_home.clone(),
            CLIENT_ID.to_string(),
            self.config.forced_chatgpt_workspace_id.clone(),
            self.config.cli_auth_credentials_store_mode,
        )
    }

    async fn provider_connect_runtime_auth(&self) -> Option<ProviderConnectRuntimeAuth> {
        let auth = self.auth_manager.auth().await?;
        let bearer_token = auth
            .get_token()
            .ok()
            .and_then(|token| (!token.trim().is_empty()).then_some(token));
        let account_id = auth
            .get_account_id()
            .and_then(|account_id| (!account_id.trim().is_empty()).then_some(account_id));

        Some(ProviderConnectRuntimeAuth {
            bearer_token,
            account_id,
            use_chatgpt_openai_base_url: auth.is_chatgpt_auth(),
        })
    }

    async fn shutdown_current_session(&mut self) {
        if let Some(session_id) = self.chat_screen.session_id() {
            // Clear any in-flight rollback guard when switching sessions.
            self.backtrack.pending_rollback = None;
            self.suppress_shutdown_complete = true;
            self.chat_screen.submit_op(Op::Shutdown);
            self.server.remove_session(&session_id).await;
        }
    }

    fn ensure_session_channel(&mut self, session_id: SessionId) -> &mut SessionEventChannel {
        self.session_event_channels
            .entry(session_id)
            .or_insert_with(|| SessionEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY))
    }

    async fn set_session_active(&mut self, session_id: SessionId, active: bool) {
        if let Some(channel) = self.session_event_channels.get_mut(&session_id) {
            let mut store = channel.store.lock().await;
            store.active = active;
        }
    }

    async fn activate_session_channel(&mut self, session_id: SessionId) {
        if self.active_session_id.is_some() {
            return;
        }
        self.set_session_active(session_id, true).await;
        let receiver = if let Some(channel) = self.session_event_channels.get_mut(&session_id) {
            channel.receiver.take()
        } else {
            None
        };
        self.active_session_id = Some(session_id);
        self.active_session_rx = receiver;
    }

    async fn store_active_session_receiver(&mut self) {
        let Some(active_id) = self.active_session_id else {
            return;
        };
        let Some(receiver) = self.active_session_rx.take() else {
            return;
        };
        if let Some(channel) = self.session_event_channels.get_mut(&active_id) {
            let mut store = channel.store.lock().await;
            store.active = false;
            channel.receiver = Some(receiver);
        }
    }

    async fn activate_session_for_replay(
        &mut self,
        session_id: SessionId,
    ) -> Option<(mpsc::Receiver<Event>, SessionEventSnapshot)> {
        let channel = self.session_event_channels.get_mut(&session_id)?;
        let receiver = channel.receiver.take()?;
        let mut store = channel.store.lock().await;
        store.active = true;
        let snapshot = store.snapshot();
        Some((receiver, snapshot))
    }

    async fn clear_active_session(&mut self) {
        if let Some(active_id) = self.active_session_id.take() {
            self.set_session_active(active_id, false).await;
        }
        self.active_session_rx = None;
    }

    async fn enqueue_session_event(&mut self, session_id: SessionId, event: Event) -> Result<()> {
        let (sender, store) = {
            let channel = self.ensure_session_channel(session_id);
            (channel.sender.clone(), Arc::clone(&channel.store))
        };

        let should_send = {
            let mut guard = store.lock().await;
            guard.push_event(event.clone());
            guard.active
        };

        if should_send {
            // Never await a bounded channel send on the main TUI loop: if the receiver falls
            // behind, `send().await` can block and the UI stops drawing. If the channel
            // is full, wait in a spawned task instead.
            match sender.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    tokio::spawn(async move {
                        if let Err(err) = sender.send(event).await {
                            tracing::warn!("session {session_id} event channel closed: {err}");
                        }
                    });
                }
                Err(TrySendError::Closed(_)) => {
                    tracing::warn!("session {session_id} event channel closed");
                }
            }
        }
        Ok(())
    }

    async fn enqueue_primary_event(&mut self, event: Event) -> Result<()> {
        if self.primary_session_id.is_none() && matches!(&event.msg, EventMsg::ShutdownComplete) {
            self.handle_savfox_event_now(event);
            return Ok(());
        }

        if let Some(session_id) = self.primary_session_id {
            return self.enqueue_session_event(session_id, event).await;
        }

        if let EventMsg::SessionConfigured(session) = &event.msg {
            let session_id = session.session_id;
            self.primary_session_id = Some(session_id);
            self.primary_session_configured = Some(session.clone());
            self.ensure_session_channel(session_id);
            self.activate_session_channel(session_id).await;

            let pending = std::mem::take(&mut self.pending_primary_events);
            for pending_event in pending {
                self.enqueue_session_event(session_id, pending_event)
                    .await?;
            }
            self.enqueue_session_event(session_id, event).await?;
        } else {
            self.pending_primary_events.push_back(event);
        }
        Ok(())
    }

    fn open_agent_picker(&mut self) {
        if self.session_event_channels.is_empty() {
            self.chat_screen
                .add_info_message("No agents available yet.".to_string(), None);
            return;
        }

        let mut sessions: Vec<(SessionId, Option<String>)> = self
            .session_event_channels
            .iter()
            .map(|(session_id, channel)| {
                let session_name = channel
                    .store
                    .try_lock()
                    .ok()
                    .and_then(|store| session_name_from_event_store(&store, *session_id));
                (*session_id, session_name)
            })
            .collect();
        sessions.sort_by(|(left_id, left_name), (right_id, right_name)| {
            let left_display = left_name
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let right_display = right_name
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            left_display
                .cmp(&right_display)
                .then_with(|| left_id.to_string().cmp(&right_id.to_string()))
        });

        let mut initial_selected_idx = None;
        let items: Vec<SelectionItem> = sessions
            .iter()
            .enumerate()
            .map(|(idx, (session_id, session_name))| {
                if self.active_session_id == Some(*session_id) {
                    initial_selected_idx = Some(idx);
                }
                let id = *session_id;
                let display_name = session_name
                    .clone()
                    .unwrap_or_else(|| session_id.to_string());
                SelectionItem {
                    name: display_name.clone(),
                    is_current: self.active_session_id == Some(*session_id),
                    description: session_name
                        .as_ref()
                        .map(|_| format!("Session ID: {session_id}")),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::SelectAgentSession(id));
                    })],
                    dismiss_on_select: true,
                    search_value: Some(match session_name.as_ref() {
                        Some(_) => format!("{display_name} {session_id}"),
                        None => session_id.to_string(),
                    }),
                    ..Default::default()
                }
            })
            .collect();

        self.chat_screen.show_selection_view(SelectionViewParams {
            title: Some("Agents".to_string()),
            subtitle: Some("Select a session to focus".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..Default::default()
        });
    }

    async fn select_agent_session(
        &mut self,
        tui: &mut tui::Tui,
        session_id: SessionId,
    ) -> Result<()> {
        if self.active_session_id == Some(session_id) {
            return Ok(());
        }

        let session = match self.server.get_session(session_id).await {
            Ok(session) => session,
            Err(err) => {
                self.chat_screen.add_error_message(format!(
                    "Failed to attach to agent session {session_id}: {err}"
                ));
                return Ok(());
            }
        };

        let previous_session_id = self.active_session_id;
        self.store_active_session_receiver().await;
        self.active_session_id = None;
        let Some((receiver, snapshot)) = self.activate_session_for_replay(session_id).await else {
            self.chat_screen
                .add_error_message(format!("Agent session {session_id} is already active."));
            if let Some(previous_session_id) = previous_session_id {
                self.activate_session_channel(previous_session_id).await;
            }
            return Ok(());
        };

        self.active_session_id = Some(session_id);
        self.active_session_rx = Some(receiver);

        let init = self.chat_screen_init_for_forked_or_resumed_session(tui, self.config.clone());
        let savfox_op_tx = crate::chat_screen::spawn_op_forwarder(session);
        self.chat_screen = ChatScreen::new_with_op_sender(init, savfox_op_tx);

        self.reset_for_session_switch(tui)?;
        self.replay_session_snapshot(snapshot);
        self.drain_active_session_events(tui).await?;

        Ok(())
    }

    fn reset_for_session_switch(&mut self, tui: &mut tui::Tui) -> Result<()> {
        self.overlay = None;
        self.transcript_cells.clear();
        self.deferred_history_lines.clear();
        self.has_emitted_history_lines = false;
        self.backtrack = BacktrackState::default();
        self.backtrack_render_pending = false;
        tui.terminal.clear_scrollback()?;
        tui.terminal.clear()?;
        Ok(())
    }

    fn reset_session_event_state(&mut self) {
        self.session_event_channels.clear();
        self.active_session_id = None;
        self.active_session_rx = None;
        self.primary_session_id = None;
        self.pending_primary_events.clear();
    }

    async fn drain_active_session_events(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let Some(mut rx) = self.active_session_rx.take() else {
            return Ok(());
        };

        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(event) => self.handle_savfox_event_now(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if !disconnected {
            self.active_session_rx = Some(rx);
        } else {
            self.clear_active_session().await;
        }

        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }

    fn replay_session_snapshot(&mut self, snapshot: SessionEventSnapshot) {
        if let Some(event) = snapshot.session_configured {
            self.handle_savfox_event_replay(event);
        }
        for event in snapshot.events {
            self.handle_savfox_event_replay(event);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        tui: &mut tui::Tui,
        auth_manager: Arc<AuthManager>,
        mut config: Config,
        cli_kv_overrides: Vec<(String, TomlValue)>,
        harness_overrides: ConfigOverrides,
        active_profile: Option<String>,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
        session_selection: SessionSelection,
        feedback: savfox_feedback::SavfoxFeedback,
        is_first_run: bool,
        ollama_chat_support_notice: Option<DeprecationNoticeEvent>,
    ) -> Result<AppExitInfo> {
        use tokio_stream::StreamExt;
        let (app_event_tx, mut app_event_rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);
        emit_deprecation_notice(&app_event_tx, ollama_chat_support_notice);
        emit_project_config_warnings(&app_event_tx, &config);
        tui.set_notification_method(config.tui_notification_method);

        tui.enter_alt_screen()?;

        // Load provider auth persisted in ~/.savfox/models/*.json so provider API keys are
        // available even when env vars are unset.
        savfox_core::inject_provider_auth_overrides_from_store(config.savfox_home.as_path());

        let harness_overrides =
            normalize_harness_overrides_for_cwd(harness_overrides, &config.cwd)?;
        let session_manager = Arc::new(SessionManager::new(
            config.savfox_home.clone(),
            auth_manager.clone(),
            SessionSource::Cli,
        ));
        let mut model = session_manager
            .get_models_manager()
            .get_default_model(&config.model, &config, RefreshStrategy::Offline)
            .await;
        let available_models = session_manager
            .get_models_manager()
            .list_models(&config, RefreshStrategy::Offline)
            .await;
        let exit_info = handle_model_migration_prompt_if_needed(
            tui,
            &mut config,
            model.as_str(),
            &app_event_tx,
            available_models,
        )
        .await;
        if let Some(exit_info) = exit_info {
            return Ok(exit_info);
        }
        if let Some(updated_model) = config.model.clone() {
            model = updated_model;
        }

        let auth = auth_manager.auth().await;
        let auth_ref = auth.as_ref();
        // Determine who should see internal Slack routing. We treat
        // `@openai.com` emails as employees and default to `External` when the
        // email is unavailable (for example, API key auth).
        let feedback_audience = if auth_ref
            .and_then(SavfoxAuth::get_account_email)
            .is_some_and(|email| email.ends_with("@openai.com"))
        {
            FeedbackAudience::OpenAiEmployee
        } else {
            FeedbackAudience::External
        };
        let otel_manager = OtelManager::new(
            SessionId::new(),
            model.as_str(),
            model.as_str(),
            auth_ref.and_then(SavfoxAuth::get_account_id),
            auth_ref.and_then(SavfoxAuth::get_account_email),
            auth_ref.map(SavfoxAuth::api_auth_mode),
            config.otel.log_user_prompt,
            savfox_core::terminal::user_agent(),
            SessionSource::Cli,
        );

        let enhanced_keys_supported = tui.enhanced_keys_supported();
        let mut chat_screen = match session_selection {
            SessionSelection::StartFresh | SessionSelection::Exit => {
                let init = crate::chat_screen::ChatScreenInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chat_screen::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: session_manager.get_models_manager(),
                    feedback: feedback.clone(),
                    is_first_run,
                    feedback_audience,
                    model: Some(model.clone()),
                    otel_manager: otel_manager.clone(),
                };
                ChatScreen::new(init, session_manager.clone())
            }
            SessionSelection::Resume(path) => {
                let resumed = session_manager
                    .resume_session_from_rollout(config.clone(), path.clone(), auth_manager.clone())
                    .await
                    .wrap_err_with(|| {
                        let path_display = path.display();
                        format!("Failed to resume session from {path_display}")
                    })?;
                let init = crate::chat_screen::ChatScreenInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chat_screen::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: session_manager.get_models_manager(),
                    feedback: feedback.clone(),
                    is_first_run,
                    feedback_audience,
                    model: config.model.clone(),
                    otel_manager: otel_manager.clone(),
                };
                ChatScreen::new_from_existing(init, resumed.session, resumed.session_configured)
            }
            SessionSelection::Fork(path) => {
                let forked = session_manager
                    .fork_session(usize::MAX, config.clone(), path.clone())
                    .await
                    .wrap_err_with(|| {
                        let path_display = path.display();
                        format!("Failed to fork session from {path_display}")
                    })?;
                let init = crate::chat_screen::ChatScreenInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chat_screen::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: session_manager.get_models_manager(),
                    feedback: feedback.clone(),
                    is_first_run,
                    feedback_audience,
                    model: config.model.clone(),
                    otel_manager: otel_manager.clone(),
                };
                ChatScreen::new_from_existing(init, forked.session, forked.session_configured)
            }
        };

        chat_screen.maybe_prompt_windows_sandbox_enable();
        chat_screen.set_alt_screen_active(tui.is_alt_screen_active());

        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        #[cfg(not(debug_assertions))]
        let upgrade_version = crate::updates::get_upgrade_version(&config);

        let mut app = Self {
            server: session_manager.clone(),
            otel_manager: otel_manager.clone(),
            app_event_tx,
            chat_screen,
            auth_manager: auth_manager.clone(),
            config,
            active_profile,
            cli_kv_overrides,
            harness_overrides,
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            file_search,
            enhanced_keys_supported,
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            feedback: feedback.clone(),
            feedback_audience,
            pending_update_action: None,
            suppress_shutdown_complete: false,
            windows_sandbox: WindowsSandboxState::default(),
            session_event_channels: HashMap::new(),
            active_session_id: None,
            active_session_rx: None,
            primary_session_id: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
        };

        // On startup, if Agent mode (workspace-write) or ReadOnly is active, warn about
        // world-writable dirs on Windows.
        #[cfg(target_os = "windows")]
        {
            let should_check = WindowsSandboxLevel::from_config(&app.config)
                != WindowsSandboxLevel::Disabled
                && matches!(
                    app.config.sandbox_policy.get(),
                    savfox_core::protocol::SandboxPolicy::WorkspaceWrite { .. }
                        | savfox_core::protocol::SandboxPolicy::ReadOnly
                )
                && !app
                    .config
                    .notices
                    .hide_world_writable_warning
                    .unwrap_or(false);
            if should_check {
                let cwd = app.config.cwd.clone();
                let env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
                let tx = app.app_event_tx.clone();
                let logs_base_dir = app.config.savfox_home.clone();
                let sandbox_policy = app.config.sandbox_policy.get().clone();
                Self::spawn_world_writable_scan(cwd, env_map, logs_base_dir, sandbox_policy, tx);
            }
        }

        #[cfg(not(debug_assertions))]
        if let Some(latest_version) = upgrade_version {
            let control = app
                .handle_event(
                    tui,
                    AppEvent::InsertHistoryCell(Box::new(UpdateAvailableHistoryCell::new(
                        latest_version,
                        crate::update_action::get_update_action(),
                    ))),
                )
                .await?;
            if let AppRunControl::Exit(exit_reason) = control {
                let (model_display, directory) = app.exit_summary_context();
                let _ = tui.leave_alt_screen();
                return Ok(AppExitInfo {
                    token_usage: app.token_usage(),
                    session_id: app.chat_screen.session_id(),
                    session_name: app.chat_screen.session_name(),
                    model_display,
                    directory,
                    update_action: app.pending_update_action,
                    exit_reason,
                });
            }
        }

        let tui_events = tui.event_stream();
        tokio::pin!(tui_events);

        crate::ascii_logo::set_terminal_title("Savfox");
        tui.frame_requester().schedule_frame();

        let mut session_created_rx = session_manager.subscribe_session_created();
        let mut listen_for_sessions = true;

        let exit_reason = loop {
            let control = select! {
                Some(event) = app_event_rx.recv() => {
                    app.handle_event(tui, event).await?
                }
                active = async {
                    if let Some(rx) = app.active_session_rx.as_mut() {
                        rx.recv().await
                    } else {
                        None
                    }
                }, if app.active_session_rx.is_some() => {
                    if let Some(event) = active {
                        app.handle_active_session_event(tui, event)?;
                    } else {
                        app.clear_active_session().await;
                    }
                    AppRunControl::Continue
                }
                Some(event) = tui_events.next() => {
                    app.handle_tui_event(tui, event).await?
                }
                // Listen on new session creation due to collab tools.
                created = session_created_rx.recv(), if listen_for_sessions => {
                    match created {
                        Ok(session_id) => {
                            app.handle_session_created(session_id).await?;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            tracing::warn!("session_created receiver lagged; skipping resync");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            listen_for_sessions = false;
                        }
                    }
                    AppRunControl::Continue
                }
            };
            match control {
                AppRunControl::Continue => {}
                AppRunControl::Exit(reason) => break reason,
            }
        };
        crate::ascii_logo::clear_terminal_title();
        let _ = tui.leave_alt_screen();
        tui.terminal.clear()?;
        let (model_display, directory) = app.exit_summary_context();
        Ok(AppExitInfo {
            token_usage: app.token_usage(),
            session_id: app.chat_screen.session_id(),
            session_name: app.chat_screen.session_name(),
            model_display,
            directory,
            update_action: app.pending_update_action,
            exit_reason,
        })
    }

    pub(crate) async fn handle_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        event: TuiEvent,
    ) -> Result<AppRunControl> {
        if self.overlay.is_some() {
            let _ = self.handle_backtrack_overlay_event(tui, event).await?;
        } else {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_key_event(tui, key_event).await;
                }
                TuiEvent::Paste(pasted) => {
                    // Many terminals convert newlines to \r when pasting (e.g., iTerm2),
                    // but tui-textarea expects \n. Normalize CR to LF.
                    // [tui-textarea]: https://github.com/rhysd/tui-textarea/blob/4d18622eeac13b309e0ff6a55a46ac6706da68cf/src/textarea.rs#L782-L783
                    // [iTerm2]: https://github.com/gnachman/iTerm2/blob/5d0c0d9f68523cbd0494dad5422998964a2ecd8d/sources/iTermPasteHelper.m#L206-L216
                    let pasted = pasted.replace("\r", "\n");
                    self.chat_screen.handle_paste(pasted);
                }
                TuiEvent::Draw => {
                    if self.backtrack_render_pending {
                        self.backtrack_render_pending = false;
                        self.render_transcript_once(tui);
                    }
                    self.chat_screen.maybe_post_pending_notification(tui);
                    if self
                        .chat_screen
                        .handle_paste_burst_tick(tui.frame_requester())
                    {
                        return Ok(AppRunControl::Continue);
                    }

                    self.chat_screen
                        .set_alt_screen_active(tui.is_alt_screen_active());
                    tui.draw(
                        self.chat_screen.desired_height(tui.terminal.size()?.width),
                        |frame| {
                            self.chat_screen.render(frame.area(), frame.buffer);
                            if let Some((x, y)) = self.chat_screen.cursor_pos(frame.area()) {
                                frame.set_cursor_position((x, y));
                            }
                        },
                    )?;
                    if self.chat_screen.external_editor_state() == ExternalEditorState::Requested {
                        self.chat_screen
                            .set_external_editor_state(ExternalEditorState::Active);
                        self.app_event_tx.send(AppEvent::LaunchExternalEditor);
                    }
                }
            }
        }
        Ok(AppRunControl::Continue)
    }

    async fn handle_event(&mut self, tui: &mut tui::Tui, event: AppEvent) -> Result<AppRunControl> {
        match event {
            AppEvent::NewSession => {
                let model = self.chat_screen.current_model().to_string();
                let summary = session_summary(
                    self.chat_screen.token_usage(),
                    self.chat_screen.session_id(),
                    self.chat_screen.session_name(),
                );
                self.shutdown_current_session().await;
                if let Err(err) = self.server.remove_and_close_all_sessions().await {
                    tracing::warn!(error = %err, "failed to close all sessions");
                }
                let init = crate::chat_screen::ChatScreenInit {
                    config: self.config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: self.app_event_tx.clone(),
                    // New sessions start without prefilled message content.
                    initial_user_message: None,
                    enhanced_keys_supported: self.enhanced_keys_supported,
                    auth_manager: self.auth_manager.clone(),
                    models_manager: self.server.get_models_manager(),
                    feedback: self.feedback.clone(),
                    is_first_run: false,
                    feedback_audience: self.feedback_audience,
                    model: Some(model),
                    otel_manager: self.otel_manager.clone(),
                };
                self.chat_screen = ChatScreen::new(init, self.server.clone());
                self.reset_session_event_state();
                if let Some(summary) = summary {
                    let lines = session_summary_lines(summary);
                    if !lines.is_empty() {
                        self.chat_screen.add_plain_history_lines(lines);
                    }
                }
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenResumePicker => {
                match crate::resume_picker::run_resume_picker(
                    tui,
                    &self.config.savfox_home,
                    &self.config.model_provider_id,
                    false,
                )
                .await?
                {
                    SessionSelection::Resume(path) => {
                        let current_cwd = self.config.cwd.clone();
                        let resume_cwd = match crate::resolve_cwd_for_resume_or_fork(
                            tui,
                            &current_cwd,
                            &path,
                            CwdPromptAction::Resume,
                            true,
                        )
                        .await?
                        {
                            Some(cwd) => cwd,
                            None => current_cwd.clone(),
                        };
                        let mut resume_config = if crate::cwds_differ(&current_cwd, &resume_cwd) {
                            match self.rebuild_config_for_cwd(resume_cwd).await {
                                Ok(cfg) => cfg,
                                Err(err) => {
                                    self.chat_screen.add_error_message(format!(
                                        "Failed to rebuild configuration for resume: {err}"
                                    ));
                                    return Ok(AppRunControl::Continue);
                                }
                            }
                        } else {
                            // No rebuild needed: current_cwd comes from self.config.cwd.
                            self.config.clone()
                        };
                        self.apply_runtime_policy_overrides(&mut resume_config);
                        let summary = session_summary(
                            self.chat_screen.token_usage(),
                            self.chat_screen.session_id(),
                            self.chat_screen.session_name(),
                        );
                        match self
                            .server
                            .resume_session_from_rollout(
                                resume_config.clone(),
                                path.clone(),
                                self.auth_manager.clone(),
                            )
                            .await
                        {
                            Ok(resumed) => {
                                self.shutdown_current_session().await;
                                self.config = resume_config;
                                tui.set_notification_method(self.config.tui_notification_method);
                                self.file_search.update_search_dir(self.config.cwd.clone());
                                let init = self.chat_screen_init_for_forked_or_resumed_session(
                                    tui,
                                    self.config.clone(),
                                );
                                self.chat_screen = ChatScreen::new_from_existing(
                                    init,
                                    resumed.session,
                                    resumed.session_configured,
                                );
                                self.reset_session_event_state();
                                if let Some(summary) = summary {
                                    let lines = session_summary_lines(summary);
                                    if !lines.is_empty() {
                                        self.chat_screen.add_plain_history_lines(lines);
                                    }
                                }
                            }
                            Err(err) => {
                                let path_display = path.display();
                                self.chat_screen.add_error_message(format!(
                                    "Failed to resume session from {path_display}: {err}"
                                ));
                            }
                        }
                    }
                    SessionSelection::Exit
                    | SessionSelection::StartFresh
                    | SessionSelection::Fork(_) => {}
                }

                // Leaving alt-screen may blank the inline viewport; force a redraw either way.
                tui.frame_requester().schedule_frame();
            }
            AppEvent::ForkCurrentSession => {
                let summary = session_summary(
                    self.chat_screen.token_usage(),
                    self.chat_screen.session_id(),
                    self.chat_screen.session_name(),
                );
                if let Some(path) = self.chat_screen.rollout_path() {
                    match self
                        .server
                        .fork_session(usize::MAX, self.config.clone(), path.clone())
                        .await
                    {
                        Ok(forked) => {
                            self.shutdown_current_session().await;
                            let init = self.chat_screen_init_for_forked_or_resumed_session(
                                tui,
                                self.config.clone(),
                            );
                            self.chat_screen = ChatScreen::new_from_existing(
                                init,
                                forked.session,
                                forked.session_configured,
                            );
                            self.reset_session_event_state();
                            if let Some(summary) = summary {
                                let lines = session_summary_lines(summary);
                                if !lines.is_empty() {
                                    self.chat_screen.add_plain_history_lines(lines);
                                }
                            }
                        }
                        Err(err) => {
                            let path_display = path.display();
                            self.chat_screen.add_error_message(format!(
                                "Failed to fork current session from {path_display}: {err}"
                            ));
                        }
                    }
                } else {
                    self.chat_screen
                        .add_error_message("Current session is not ready to fork yet.".to_string());
                }

                tui.frame_requester().schedule_frame();
            }
            AppEvent::InsertHistoryCell(cell) => {
                let cell: Arc<dyn HistoryCell> = cell.into();
                if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                    t.insert_cell(cell.clone());
                    tui.frame_requester().schedule_frame();
                }
                self.transcript_cells.push(cell.clone());
                let mut display = cell.display_lines(tui.terminal.last_known_screen_size.width);
                if !display.is_empty() {
                    // Only insert a separating blank line for new cells that are not
                    // part of an ongoing stream. Streaming continuations should not
                    // accrue extra blank lines between chunks.
                    if !cell.is_stream_continuation() {
                        if self.has_emitted_history_lines {
                            display.insert(0, Line::from(""));
                        } else {
                            self.has_emitted_history_lines = true;
                        }
                    }
                    if self.overlay.is_some() {
                        self.deferred_history_lines.extend(display);
                    } else {
                        tui.insert_history_lines(display);
                    }
                }
            }
            AppEvent::StartCommitAnimation => {
                if self
                    .commit_anim_running
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let tx = self.app_event_tx.clone();
                    let running = self.commit_anim_running.clone();
                    thread::spawn(move || {
                        while running.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(50));
                            tx.send(AppEvent::CommitTick);
                        }
                    });
                }
            }
            AppEvent::StopCommitAnimation => {
                self.commit_anim_running.store(false, Ordering::Release);
            }
            AppEvent::CommitTick => {
                self.chat_screen.on_commit_tick();
            }
            AppEvent::SavfoxEvent(event) => {
                self.enqueue_primary_event(event).await?;
            }
            AppEvent::Exit(mode) => match mode {
                ExitMode::ShutdownFirst => self.chat_screen.submit_op(Op::Shutdown),
                ExitMode::Immediate => {
                    return Ok(AppRunControl::Exit(ExitReason::UserRequested));
                }
            },
            AppEvent::FatalExitRequest(message) => {
                return Ok(AppRunControl::Exit(ExitReason::Fatal(message)));
            }
            AppEvent::SavfoxOp(op) => {
                self.chat_screen.submit_op(op);
            }
            AppEvent::DiffResult(text) => {
                // Clear the in-progress state in the bottom pane
                self.chat_screen.on_diff_complete();
                // Enter alternate screen using TUI helper and build pager lines
                let _ = tui.enter_alt_screen();
                let pager_lines: Vec<ratatui::text::Line<'static>> = if text.trim().is_empty() {
                    vec!["No changes detected.".italic().into()]
                } else {
                    text.lines().map(ansi_escape_line).collect()
                };
                self.overlay = Some(Overlay::new_static_with_lines(
                    pager_lines,
                    "D I F F".to_string(),
                ));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenAppLink {
                title,
                description,
                instructions,
                url,
                is_installed,
            } => {
                self.chat_screen.open_app_link_view(
                    title,
                    description,
                    instructions,
                    url,
                    is_installed,
                );
            }
            AppEvent::StartFileSearch(query) => {
                self.file_search.on_user_query(query);
            }
            AppEvent::FileSearchResult { query, matches } => {
                self.chat_screen.apply_file_search_result(query, matches);
            }
            AppEvent::RateLimitSnapshotFetched(snapshot) => {
                self.chat_screen.on_rate_limit_snapshot(Some(snapshot));
            }
            AppEvent::ConnectorsLoaded(result) => {
                self.chat_screen.on_connectors_loaded(result);
            }
            AppEvent::OpenConnectAuthPrompt { provider_id } => {
                self.chat_screen.open_connect_auth_prompt(provider_id);
            }
            AppEvent::OpenConnectApiKeyPrompt { provider_id } => {
                self.chat_screen.open_connect_api_key_prompt(provider_id);
            }
            AppEvent::BeginOpenAiConnectAuth {
                provider_id,
                method,
            } => {
                if !provider_id.eq_ignore_ascii_case("openai") {
                    self.chat_screen.add_error_message(format!(
                        "OpenAI auth method selection is only available for the OpenAI provider (got `{provider_id}`)."
                    ));
                    return Ok(AppRunControl::Continue);
                }

                match method {
                    OpenAiConnectAuthMethod::Browser => {
                        let opts = self.openai_connect_server_options();
                        match run_login_server(opts) {
                            Ok(server) => {
                                let auth_url = server.auth_url.clone();
                                self.chat_screen.add_info_message(
                                    "Opening browser for ChatGPT sign-in...".to_string(),
                                    Some(
                                        "Complete sign-in, then Savfox will import OpenAI models automatically."
                                            .to_string(),
                                    ),
                                );
                                self.chat_screen.add_info_message(
                                    format!(
                                        "If the browser did not open, use this URL: {auth_url}"
                                    ),
                                    None,
                                );

                                let tx = self.app_event_tx.clone();
                                tokio::spawn(async move {
                                    let result = server.block_until_done().await.map_err(|err| {
                                        format!("ChatGPT browser sign-in failed: {err}")
                                    });
                                    tx.send(AppEvent::OpenAiConnectAuthCompleted {
                                        provider_id,
                                        method: OpenAiConnectAuthMethod::Browser,
                                        result,
                                    });
                                });
                            }
                            Err(err) => {
                                self.chat_screen.add_error_message(format!(
                                    "Failed to start ChatGPT browser sign-in: {err}"
                                ));
                            }
                        }
                    }
                    OpenAiConnectAuthMethod::Headless => {
                        self.chat_screen.add_info_message(
                            "Starting headless ChatGPT sign-in...".to_string(),
                            None,
                        );
                        let mut opts = self.openai_connect_server_options();
                        opts.open_browser = false;
                        let tx = self.app_event_tx.clone();

                        tokio::spawn(async move {
                            let device_code = match request_device_code(&opts).await {
                                Ok(device_code) => device_code,
                                Err(err) => {
                                    tx.send(AppEvent::OpenAiConnectAuthCompleted {
                                        provider_id,
                                        method: OpenAiConnectAuthMethod::Headless,
                                        result: Err(format!(
                                            "Failed to start headless ChatGPT sign-in: {err}"
                                        )),
                                    });
                                    return;
                                }
                            };

                            tx.send(AppEvent::OpenAiConnectDeviceCode {
                                verification_url: device_code.verification_url.clone(),
                                user_code: device_code.user_code.clone(),
                            });

                            let result = complete_device_code_login(opts, device_code)
                                .await
                                .map_err(|err| format!("Headless ChatGPT sign-in failed: {err}"));
                            tx.send(AppEvent::OpenAiConnectAuthCompleted {
                                provider_id,
                                method: OpenAiConnectAuthMethod::Headless,
                                result,
                            });
                        });
                    }
                }
            }
            AppEvent::OpenAiConnectDeviceCode {
                verification_url,
                user_code,
            } => {
                self.chat_screen.add_info_message(
                    format!(
                        "Headless ChatGPT sign-in:\n1) Open {verification_url}\n2) Enter code: {user_code}"
                    ),
                    Some("Savfox is waiting for authorization...".to_string()),
                );
            }
            AppEvent::OpenAiConnectAuthCompleted {
                provider_id,
                method,
                result,
            } => match result {
                Ok(()) => {
                    self.auth_manager.reload();
                    let method_label = match method {
                        OpenAiConnectAuthMethod::Browser => "browser",
                        OpenAiConnectAuthMethod::Headless => "headless",
                    };
                    self.chat_screen.add_info_message(
                        format!(
                            "ChatGPT sign-in completed ({method_label}). Importing OpenAI models..."
                        ),
                        None,
                    );
                    self.app_event_tx.send(AppEvent::BeginProviderConnect {
                        provider_id,
                        api_key: None,
                    });
                }
                Err(err) => {
                    self.chat_screen.add_error_message(err);
                }
            },
            AppEvent::BeginProviderConnect {
                provider_id,
                api_key,
            } => {
                let provider = self
                    .config
                    .model_providers
                    .get(provider_id.as_str())
                    .cloned()
                    .or_else(|| {
                        built_in_model_providers()
                            .get(provider_id.as_str())
                            .cloned()
                    });
                let Some(provider) = provider else {
                    self.chat_screen
                        .add_error_message(format!("Unknown provider: {provider_id}"));
                    return Ok(AppRunControl::Continue);
                };

                self.chat_screen
                    .add_info_message(format!("Connecting to {}...", provider.display_name), None);
                let tx = self.app_event_tx.clone();
                let savfox_home = self.config.savfox_home.clone();
                let runtime_auth = self.provider_connect_runtime_auth().await;
                tokio::spawn(async move {
                    let result = connect_provider(
                        savfox_home,
                        provider_id.clone(),
                        provider,
                        api_key,
                        runtime_auth,
                    )
                    .await;
                    tx.send(AppEvent::ProviderConnectCompleted(result));
                });
            }
            AppEvent::ProviderConnectCompleted(result) => {
                let result = match result {
                    Ok(result) => result,
                    Err(err) => {
                        self.chat_screen.add_error_message(err);
                        return Ok(AppRunControl::Continue);
                    }
                };

                if result.models.is_empty() {
                    self.chat_screen.add_error_message(
                        "Connected, but the provider did not return any models.".to_string(),
                    );
                    return Ok(AppRunControl::Continue);
                }

                if let Err(err) = persist_provider_connection(
                    self.config.savfox_home.as_path(),
                    &result.provider_id,
                    &result.provider_name,
                    &result.models,
                    result.env_key.as_deref(),
                    result.api_key.as_deref(),
                ) {
                    self.chat_screen.add_error_message(format!(
                        "Failed to save provider models for {}: {err}",
                        result.provider_id
                    ));
                    return Ok(AppRunControl::Continue);
                }
                savfox_core::inject_provider_auth_overrides_from_store(
                    self.config.savfox_home.as_path(),
                );

                let provider_id = result.provider_id.clone();
                let Some(mut provider_info) = self
                    .config
                    .model_providers
                    .get(provider_id.as_str())
                    .cloned()
                    .or_else(|| {
                        built_in_model_providers()
                            .get(provider_id.as_str())
                            .cloned()
                    })
                else {
                    self.chat_screen.add_error_message(format!(
                        "Connected provider `{provider_id}` is not configured in this build."
                    ));
                    return Ok(AppRunControl::Continue);
                };

                if !result.base_url.trim().is_empty() {
                    provider_info.base_url = Some(result.base_url.clone());
                }

                self.config
                    .model_providers
                    .insert(provider_id.clone(), provider_info.clone());
                self.config.model_provider_id = provider_id.clone();
                self.config.model_provider = provider_info.clone();
                self.chat_screen
                    .set_model_provider(provider_id.as_str(), provider_info);

                let Some(default_model) =
                    select_default_model(&result.models, provider_id.as_str())
                else {
                    self.chat_screen.add_error_message(format!(
                        "Connected to {}, but no usable model ID was found.",
                        result.provider_name
                    ));
                    return Ok(AppRunControl::Continue);
                };
                let normalized_model =
                    savfox_core::request_model_for_provider(&default_model, provider_id.as_str());

                self.app_event_tx
                    .send(AppEvent::SavfoxOp(Op::OverrideTurnContext {
                        cwd: None,
                        approval_policy: None,
                        sandbox_policy: None,
                        windows_sandbox_level: None,
                        model: Some(normalized_model.clone()),
                        effort: Some(None),
                        summary: None,
                        collaboration_mode: None,
                        personality: None,
                    }));
                self.app_event_tx
                    .send(AppEvent::UpdateModel(normalized_model.clone()));
                self.app_event_tx
                    .send(AppEvent::UpdateReasoningEffort(None));

                let profile = self.active_profile.as_deref();
                let mut edits: Vec<ConfigEdit> = vec![ConfigEdit::SetPath {
                    segments: vec!["model_provider".to_string()],
                    value: toml_edit_value(provider_id.clone()),
                }];
                if !result.base_url.trim().is_empty() {
                    edits.push(ConfigEdit::SetPath {
                        segments: vec![
                            "model_providers".to_string(),
                            provider_id.clone(),
                            "base_url".to_string(),
                        ],
                        value: toml_edit_value(result.base_url.clone()),
                    });
                }
                let persist_result = ConfigEditsBuilder::new(&self.config.savfox_home)
                    .with_profile(profile)
                    .with_edits(edits)
                    .set_model(Some(normalized_model.as_str()), None)
                    .apply()
                    .await;
                if let Err(err) = persist_result {
                    self.chat_screen.add_error_message(format!(
                        "Connected to {}, but failed to persist config: {err}",
                        result.provider_name
                    ));
                    return Ok(AppRunControl::Continue);
                }

                let mut message = format!(
                    "Connected {} and imported {} model(s).",
                    result.provider_name,
                    result.models.len()
                );
                if let Some(profile) = profile {
                    message.push_str(" Updated ");
                    message.push_str(profile);
                    message.push_str(" profile.");
                }
                self.chat_screen.add_info_message(message, None);
                self.chat_screen
                    .open_connected_provider_models_popup(provider_id.as_str());
            }
            AppEvent::UpdateReasoningEffort(effort) => {
                self.on_update_reasoning_effort(effort);
            }
            AppEvent::UpdateModel(model) => {
                self.chat_screen.set_model(&model);
            }
            AppEvent::UpdateCollaborationMode(mask) => {
                self.chat_screen.set_collaboration_mask(mask);
            }
            AppEvent::UpdatePersonality(personality) => {
                self.on_update_personality(personality);
            }
            AppEvent::OpenReasoningPopup { model } => {
                self.chat_screen.open_reasoning_popup(model);
            }
            AppEvent::OpenAllModelsPopup { models } => {
                self.chat_screen.open_all_models_popup(models);
            }
            AppEvent::OpenFullAccessConfirmation {
                preset,
                return_to_permissions,
            } => {
                self.chat_screen
                    .open_full_access_confirmation(preset, return_to_permissions);
            }
            AppEvent::OpenWorldWritableWarningConfirmation {
                preset,
                sample_paths,
                extra_count,
                failed_scan,
            } => {
                self.chat_screen.open_world_writable_warning_confirmation(
                    preset,
                    sample_paths,
                    extra_count,
                    failed_scan,
                );
            }
            AppEvent::OpenFeedbackNote {
                category,
                include_logs,
            } => {
                self.chat_screen.open_feedback_note(category, include_logs);
            }
            AppEvent::OpenFeedbackConsent { category } => {
                self.chat_screen.open_feedback_consent(category);
            }
            AppEvent::LaunchExternalEditor => {
                if self.chat_screen.external_editor_state() == ExternalEditorState::Active {
                    self.launch_external_editor(tui).await;
                }
            }
            AppEvent::OpenWindowsSandboxEnablePrompt { preset } => {
                self.chat_screen.open_windows_sandbox_enable_prompt(preset);
            }
            AppEvent::OpenWindowsSandboxFallbackPrompt { preset, reason } => {
                self.otel_manager
                    .counter("savfox.windows_sandbox.fallback_prompt_shown", 1, &[]);
                self.chat_screen.clear_windows_sandbox_setup_status();
                if let Some(started_at) = self.windows_sandbox.setup_started_at.take() {
                    self.otel_manager.record_duration(
                        "savfox.windows_sandbox.elevated_setup_duration_ms",
                        started_at.elapsed(),
                        &[("result", "failure")],
                    );
                }
                self.chat_screen
                    .open_windows_sandbox_fallback_prompt(preset, reason);
            }
            AppEvent::BeginWindowsSandboxElevatedSetup { preset } => {
                #[cfg(target_os = "windows")]
                {
                    let policy = preset.sandbox.clone();
                    let policy_cwd = self.config.cwd.clone();
                    let command_cwd = policy_cwd.clone();
                    let env_map: std::collections::HashMap<String, String> =
                        std::env::vars().collect();
                    let savfox_home = self.config.savfox_home.clone();
                    let tx = self.app_event_tx.clone();

                    // If the elevated setup already ran on this machine, don't prompt for
                    // elevation again - just flip the config to use the elevated path.
                    if savfox_core::windows_sandbox::sandbox_setup_is_complete(
                        savfox_home.as_path(),
                    ) {
                        tx.send(AppEvent::EnableWindowsSandboxForAgentMode {
                            preset,
                            mode: WindowsSandboxEnableMode::Elevated,
                        });
                        return Ok(AppRunControl::Continue);
                    }

                    self.chat_screen.show_windows_sandbox_setup_status();
                    self.windows_sandbox.setup_started_at = Some(Instant::now());
                    let otel_manager = self.otel_manager.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = savfox_core::windows_sandbox::run_elevated_setup(
                            &policy,
                            policy_cwd.as_path(),
                            command_cwd.as_path(),
                            &env_map,
                            savfox_home.as_path(),
                        );
                        let event = match result {
                            Ok(()) => {
                                otel_manager.counter(
                                    "savfox.windows_sandbox.elevated_setup_success",
                                    1,
                                    &[],
                                );
                                AppEvent::EnableWindowsSandboxForAgentMode {
                                    preset: preset.clone(),
                                    mode: WindowsSandboxEnableMode::Elevated,
                                }
                            }
                            Err(err) => {
                                let mut code_tag: Option<String> = None;
                                let mut message_tag: Option<String> = None;
                                if let Some((code, message)) =
                                    savfox_core::windows_sandbox::elevated_setup_failure_details(
                                        &err,
                                    )
                                {
                                    code_tag = Some(code);
                                    message_tag = Some(message);
                                }
                                let mut tags: Vec<(&str, &str)> = Vec::new();
                                if let Some(code) = code_tag.as_deref() {
                                    tags.push(("code", code));
                                }
                                if let Some(message) = message_tag.as_deref() {
                                    tags.push(("message", message));
                                }
                                otel_manager.counter(
                                    savfox_core::windows_sandbox::elevated_setup_failure_metric_name(
                                        &err,
                                    ),
                                    1,
                                    &tags,
                                );
                                tracing::error!(
                                    error = %err,
                                    "failed to run elevated Windows sandbox setup"
                                );
                                AppEvent::OpenWindowsSandboxFallbackPrompt {
                                    preset,
                                    reason: WindowsSandboxFallbackReason::ElevationFailed,
                                }
                            }
                        };
                        tx.send(event);
                    });
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = preset;
                }
            }
            AppEvent::EnableWindowsSandboxForAgentMode { preset, mode } => {
                #[cfg(target_os = "windows")]
                {
                    self.chat_screen.clear_windows_sandbox_setup_status();
                    if let Some(started_at) = self.windows_sandbox.setup_started_at.take() {
                        self.otel_manager.record_duration(
                            "savfox.windows_sandbox.elevated_setup_duration_ms",
                            started_at.elapsed(),
                            &[("result", "success")],
                        );
                    }
                    let profile = self.active_profile.as_deref();
                    let feature_key = Feature::WindowsSandbox.key();
                    let elevated_key = Feature::WindowsSandboxElevated.key();
                    let elevated_enabled = matches!(mode, WindowsSandboxEnableMode::Elevated);
                    let mut builder =
                        ConfigEditsBuilder::new(&self.config.savfox_home).with_profile(profile);
                    if elevated_enabled {
                        builder = builder.set_feature_enabled(elevated_key, true);
                    } else {
                        builder = builder
                            .set_feature_enabled(feature_key, true)
                            .set_feature_enabled(elevated_key, false);
                    }
                    match builder.apply().await {
                        Ok(()) => {
                            if elevated_enabled {
                                self.config.set_windows_elevated_sandbox_enabled(true);
                                self.chat_screen
                                    .set_feature_enabled(Feature::WindowsSandboxElevated, true);
                            } else {
                                self.config.set_windows_sandbox_enabled(true);
                                self.config.set_windows_elevated_sandbox_enabled(false);
                                self.chat_screen
                                    .set_feature_enabled(Feature::WindowsSandbox, true);
                                self.chat_screen
                                    .set_feature_enabled(Feature::WindowsSandboxElevated, false);
                            }
                            self.chat_screen.clear_forced_auto_mode_downgrade();
                            let windows_sandbox_level =
                                WindowsSandboxLevel::from_config(&self.config);
                            if let Some((sample_paths, extra_count, failed_scan)) =
                                self.chat_screen.world_writable_warning_details()
                            {
                                self.app_event_tx.send(AppEvent::SavfoxOp(
                                    Op::OverrideTurnContext {
                                        cwd: None,
                                        approval_policy: None,
                                        sandbox_policy: None,
                                        windows_sandbox_level: Some(windows_sandbox_level),
                                        model: None,
                                        effort: None,
                                        summary: None,
                                        collaboration_mode: None,
                                        personality: None,
                                    },
                                ));
                                self.app_event_tx.send(
                                    AppEvent::OpenWorldWritableWarningConfirmation {
                                        preset: Some(preset.clone()),
                                        sample_paths,
                                        extra_count,
                                        failed_scan,
                                    },
                                );
                            } else {
                                self.app_event_tx.send(AppEvent::SavfoxOp(
                                    Op::OverrideTurnContext {
                                        cwd: None,
                                        approval_policy: Some(preset.approval),
                                        sandbox_policy: Some(preset.sandbox.clone()),
                                        windows_sandbox_level: Some(windows_sandbox_level),
                                        model: None,
                                        effort: None,
                                        summary: None,
                                        collaboration_mode: None,
                                        personality: None,
                                    },
                                ));
                                self.app_event_tx
                                    .send(AppEvent::UpdateAskForApprovalPolicy(preset.approval));
                                self.app_event_tx
                                    .send(AppEvent::UpdateSandboxPolicy(preset.sandbox.clone()));
                                self.chat_screen.add_info_message(
                                    match mode {
                                        WindowsSandboxEnableMode::Elevated => {
                                            "Enabled elevated agent sandbox.".to_string()
                                        }
                                        WindowsSandboxEnableMode::Legacy => {
                                            "Enabled non-elevated agent sandbox.".to_string()
                                        }
                                    },
                                    None,
                                );
                            }
                        }
                        Err(err) => {
                            tracing::error!(
                                error = %err,
                                "failed to enable Windows sandbox feature"
                            );
                            self.chat_screen.add_error_message(format!(
                                "Failed to enable the Windows sandbox feature: {err}"
                            ));
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = (preset, mode);
                }
            }
            AppEvent::PersistModelSelection { model, effort } => {
                let profile = self.active_profile.as_deref();
                match ConfigEditsBuilder::new(&self.config.savfox_home)
                    .with_profile(profile)
                    .set_model(Some(model.as_str()), effort)
                    .apply()
                    .await
                {
                    Ok(()) => {
                        let mut message = format!("Model changed to {model}");
                        if let Some(label) = Self::reasoning_label_for(&model, effort) {
                            message.push(' ');
                            message.push_str(label);
                        }
                        if let Some(profile) = profile {
                            message.push_str(" for ");
                            message.push_str(profile);
                            message.push_str(" profile");
                        }
                        self.chat_screen.add_info_message(message, None);
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to persist model selection"
                        );
                        if let Some(profile) = profile {
                            self.chat_screen.add_error_message(format!(
                                "Failed to save model for profile `{profile}`: {err}"
                            ));
                        } else {
                            self.chat_screen
                                .add_error_message(format!("Failed to save default model: {err}"));
                        }
                    }
                }
            }
            AppEvent::PersistPersonalitySelection { personality } => {
                let profile = self.active_profile.as_deref();
                match ConfigEditsBuilder::new(&self.config.savfox_home)
                    .with_profile(profile)
                    .set_personality(Some(personality))
                    .apply()
                    .await
                {
                    Ok(()) => {
                        let label = Self::personality_label(personality);
                        let mut message = format!("Personality set to {label}");
                        if let Some(profile) = profile {
                            message.push_str(" for ");
                            message.push_str(profile);
                            message.push_str(" profile");
                        }
                        self.chat_screen.add_info_message(message, None);
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to persist personality selection"
                        );
                        if let Some(profile) = profile {
                            self.chat_screen.add_error_message(format!(
                                "Failed to save personality for profile `{profile}`: {err}"
                            ));
                        } else {
                            self.chat_screen.add_error_message(format!(
                                "Failed to save default personality: {err}"
                            ));
                        }
                    }
                }
            }
            AppEvent::UpdateAskForApprovalPolicy(policy) => {
                self.runtime_approval_policy_override = Some(policy);
                if let Err(err) = self.config.approval_policy.set(policy) {
                    tracing::warn!(%err, "failed to set approval policy on app config");
                    self.chat_screen
                        .add_error_message(format!("Failed to set approval policy: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                self.chat_screen.set_approval_policy(policy);
            }
            AppEvent::UpdateSandboxPolicy(policy) => {
                #[cfg(target_os = "windows")]
                let policy_is_workspace_write_or_ro = matches!(
                    &policy,
                    savfox_core::protocol::SandboxPolicy::WorkspaceWrite { .. }
                        | savfox_core::protocol::SandboxPolicy::ReadOnly
                );

                if let Err(err) = self.config.sandbox_policy.set(policy.clone()) {
                    tracing::warn!(%err, "failed to set sandbox policy on app config");
                    self.chat_screen
                        .add_error_message(format!("Failed to set sandbox policy: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                #[cfg(target_os = "windows")]
                if !matches!(&policy, savfox_core::protocol::SandboxPolicy::ReadOnly)
                    || WindowsSandboxLevel::from_config(&self.config)
                        != WindowsSandboxLevel::Disabled
                {
                    self.config.forced_auto_mode_downgraded_on_windows = false;
                }
                if let Err(err) = self.chat_screen.set_sandbox_policy(policy) {
                    tracing::warn!(%err, "failed to set sandbox policy on chat config");
                    self.chat_screen
                        .add_error_message(format!("Failed to set sandbox policy: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                self.runtime_sandbox_policy_override =
                    Some(self.config.sandbox_policy.get().clone());

                // If sandbox policy becomes workspace-write or read-only, run the Windows
                // world-writable scan.
                #[cfg(target_os = "windows")]
                {
                    // One-shot suppression if the user just confirmed continue.
                    if self.windows_sandbox.skip_world_writable_scan_once {
                        self.windows_sandbox.skip_world_writable_scan_once = false;
                        return Ok(AppRunControl::Continue);
                    }

                    let should_check = WindowsSandboxLevel::from_config(&self.config)
                        != WindowsSandboxLevel::Disabled
                        && policy_is_workspace_write_or_ro
                        && !self.chat_screen.world_writable_warning_hidden();
                    if should_check {
                        let cwd = self.config.cwd.clone();
                        let env_map: std::collections::HashMap<String, String> =
                            std::env::vars().collect();
                        let tx = self.app_event_tx.clone();
                        let logs_base_dir = self.config.savfox_home.clone();
                        let sandbox_policy = self.config.sandbox_policy.get().clone();
                        Self::spawn_world_writable_scan(
                            cwd,
                            env_map,
                            logs_base_dir,
                            sandbox_policy,
                            tx,
                        );
                    }
                }
            }
            AppEvent::UpdateFeatureFlags { updates } => {
                if updates.is_empty() {
                    return Ok(AppRunControl::Continue);
                }
                let windows_sandbox_changed = updates.iter().any(|(feature, _)| {
                    matches!(
                        feature,
                        Feature::WindowsSandbox | Feature::WindowsSandboxElevated
                    )
                });
                let mut builder = ConfigEditsBuilder::new(&self.config.savfox_home)
                    .with_profile(self.active_profile.as_deref());
                for (feature, enabled) in &updates {
                    let feature_key = feature.key();
                    if *enabled {
                        // Update the in-memory configs.
                        self.config.features.enable(*feature);
                        self.chat_screen.set_feature_enabled(*feature, true);
                        builder = builder.set_feature_enabled(feature_key, true);
                    } else {
                        // Update the in-memory configs.
                        self.config.features.disable(*feature);
                        self.chat_screen.set_feature_enabled(*feature, false);
                        if feature.default_enabled() {
                            builder = builder.set_feature_enabled(feature_key, false);
                        } else {
                            // If the feature already default to `false`, we drop the key
                            // in the config file so that the user does not miss the feature
                            // once it gets globally released.
                            builder = builder.with_edits(vec![ConfigEdit::ClearPath {
                                segments: vec!["features".to_string(), feature_key.to_string()],
                            }]);
                        }
                    }
                }
                if windows_sandbox_changed {
                    #[cfg(target_os = "windows")]
                    {
                        let windows_sandbox_level = WindowsSandboxLevel::from_config(&self.config);
                        self.app_event_tx
                            .send(AppEvent::SavfoxOp(Op::OverrideTurnContext {
                                cwd: None,
                                approval_policy: None,
                                sandbox_policy: None,
                                windows_sandbox_level: Some(windows_sandbox_level),
                                model: None,
                                effort: None,
                                summary: None,
                                collaboration_mode: None,
                                personality: None,
                            }));
                    }
                }
                if let Err(err) = builder.apply().await {
                    tracing::error!(error = %err, "failed to persist feature flags");
                    self.chat_screen.add_error_message(format!(
                        "Failed to update experimental features: {err}"
                    ));
                }
            }
            AppEvent::SkipNextWorldWritableScan => {
                self.windows_sandbox.skip_world_writable_scan_once = true;
            }
            AppEvent::UpdateFullAccessWarningAcknowledged(ack) => {
                self.chat_screen.set_full_access_warning_acknowledged(ack);
            }
            AppEvent::UpdateWorldWritableWarningAcknowledged(ack) => {
                self.chat_screen
                    .set_world_writable_warning_acknowledged(ack);
            }
            AppEvent::UpdateRateLimitSwitchPromptHidden(hidden) => {
                self.chat_screen.set_rate_limit_switch_prompt_hidden(hidden);
            }
            AppEvent::PersistFullAccessWarningAcknowledged => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.savfox_home)
                    .set_hide_full_access_warning(true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist full access warning acknowledgement"
                    );
                    self.chat_screen.add_error_message(format!(
                        "Failed to save full access confirmation preference: {err}"
                    ));
                }
            }
            AppEvent::PersistWorldWritableWarningAcknowledged => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.savfox_home)
                    .set_hide_world_writable_warning(true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist world-writable warning acknowledgement"
                    );
                    self.chat_screen.add_error_message(format!(
                        "Failed to save Agent mode warning preference: {err}"
                    ));
                }
            }
            AppEvent::PersistRateLimitSwitchPromptHidden => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.savfox_home)
                    .set_hide_rate_limit_model_nudge(true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist rate limit switch prompt preference"
                    );
                    self.chat_screen.add_error_message(format!(
                        "Failed to save rate limit reminder preference: {err}"
                    ));
                }
            }
            AppEvent::PersistModelMigrationPromptAcknowledged {
                from_model,
                to_model,
            } => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.savfox_home)
                    .record_model_migration_seen(from_model.as_str(), to_model.as_str())
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist model migration prompt acknowledgement"
                    );
                    self.chat_screen.add_error_message(format!(
                        "Failed to save model migration prompt preference: {err}"
                    ));
                }
            }
            AppEvent::OpenApprovalsPopup => {
                self.chat_screen.open_approvals_popup();
            }
            AppEvent::OpenAgentPicker => {
                self.open_agent_picker();
            }
            AppEvent::SelectAgentSession(session_id) => {
                self.select_agent_session(tui, session_id).await?;
            }
            AppEvent::OpenSkillsList => {
                self.chat_screen.open_skills_list();
            }
            AppEvent::OpenManageSkillsPopup => {
                self.chat_screen.open_manage_skills_popup();
            }
            AppEvent::SetSkillEnabled { path, enabled } => {
                let edits = [ConfigEdit::SetSkillConfig {
                    path: path.clone(),
                    enabled,
                }];
                match ConfigEditsBuilder::new(&self.config.savfox_home)
                    .with_edits(edits)
                    .apply()
                    .await
                {
                    Ok(()) => {
                        self.chat_screen.update_skill_enabled(path.clone(), enabled);
                    }
                    Err(err) => {
                        let path_display = path.display();
                        self.chat_screen.add_error_message(format!(
                            "Failed to update skill config for {path_display}: {err}"
                        ));
                    }
                }
            }
            AppEvent::OpenPermissionsPopup => {
                self.chat_screen.open_permissions_popup();
            }
            AppEvent::OpenReviewBranchPicker(cwd) => {
                self.chat_screen.show_review_branch_picker(&cwd).await;
            }
            AppEvent::OpenReviewCommitPicker(cwd) => {
                self.chat_screen.show_review_commit_picker(&cwd).await;
            }
            AppEvent::OpenReviewCustomPrompt => {
                self.chat_screen.show_review_custom_prompt();
            }
            AppEvent::SubmitUserMessageWithMode {
                text,
                collaboration_mode,
            } => {
                self.chat_screen
                    .submit_user_message_with_mode(text, collaboration_mode);
            }
            AppEvent::ManageSkillsClosed => {
                self.chat_screen.handle_manage_skills_closed();
            }
            AppEvent::FullScreenApprovalRequest(request) => match request {
                ApprovalRequest::ApplyPatch { cwd, changes, .. } => {
                    let _ = tui.enter_alt_screen();
                    let diff_summary = DiffSummary::new(changes, cwd);
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![diff_summary.into()],
                        "P A T C H".to_string(),
                    ));
                }
                ApprovalRequest::Exec { command, .. } => {
                    let _ = tui.enter_alt_screen();
                    let full_cmd = strip_bash_lc_and_escape(&command);
                    let full_cmd_lines = highlight_bash_to_lines(&full_cmd);
                    self.overlay = Some(Overlay::new_static_with_lines(
                        full_cmd_lines,
                        "E X E C".to_string(),
                    ));
                }
                ApprovalRequest::McpElicitation {
                    server_name,
                    message,
                    ..
                } => {
                    let _ = tui.enter_alt_screen();
                    let paragraph = Paragraph::new(vec![
                        Line::from(vec!["Server: ".into(), server_name.bold()]),
                        Line::from(""),
                        Line::from(message),
                    ])
                    .wrap(Wrap { trim: false });
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![Box::new(paragraph)],
                        "E L I C I T A T I O N".to_string(),
                    ));
                }
            },
        }
        Ok(AppRunControl::Continue)
    }

    fn handle_savfox_event_now(&mut self, event: Event) {
        if self.suppress_shutdown_complete && matches!(event.msg, EventMsg::ShutdownComplete) {
            self.suppress_shutdown_complete = false;
            return;
        }
        if let EventMsg::ListSkillsResponse(response) = &event.msg {
            let cwd = self.chat_screen.config_ref().cwd.clone();
            let errors = errors_for_cwd(&cwd, response);
            emit_skill_load_warnings(&self.app_event_tx, &errors);
        }
        self.handle_backtrack_event(&event.msg);
        self.chat_screen.handle_savfox_event(event);
    }

    fn handle_savfox_event_replay(&mut self, event: Event) {
        self.handle_backtrack_event(&event.msg);
        self.chat_screen.handle_savfox_event_replay(event);
    }

    fn handle_active_session_event(&mut self, tui: &mut tui::Tui, event: Event) -> Result<()> {
        self.handle_savfox_event_now(event);
        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }

    async fn handle_session_created(&mut self, session_id: SessionId) -> Result<()> {
        if self.session_event_channels.contains_key(&session_id) {
            return Ok(());
        }
        let session = match self.server.get_session(session_id).await {
            Ok(session) => session,
            Err(err) => {
                tracing::warn!("failed to attach listener for session {session_id}: {err}");
                return Ok(());
            }
        };
        let config_snapshot = session.config_snapshot().await;
        let event = Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id,
                forked_from_id: None,
                session_name: None,
                model: config_snapshot.model,
                model_provider_id: config_snapshot.model_provider_id,
                approval_policy: config_snapshot.approval_policy,
                sandbox_policy: config_snapshot.sandbox_policy,
                cwd: config_snapshot.cwd,
                reasoning_effort: config_snapshot.reasoning_effort,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: session.rollout_path(),
            }),
        };
        let channel =
            SessionEventChannel::new_with_session_configured(THREAD_EVENT_CHANNEL_CAPACITY, event);
        let sender = channel.sender.clone();
        let store = Arc::clone(&channel.store);
        self.session_event_channels.insert(session_id, channel);
        tokio::spawn(async move {
            loop {
                let event = match session.next_event().await {
                    Ok(event) => event,
                    Err(err) => {
                        tracing::debug!("external session {session_id} listener stopped: {err}");
                        break;
                    }
                };
                let should_send = {
                    let mut guard = store.lock().await;
                    guard.push_event(event.clone());
                    guard.active
                };
                if should_send && let Err(err) = sender.send(event).await {
                    tracing::debug!("external session {session_id} channel closed: {err}");
                    break;
                }
            }
        });
        Ok(())
    }

    fn reasoning_label(reasoning_effort: Option<ReasoningEffortConfig>) -> &'static str {
        match reasoning_effort {
            Some(ReasoningEffortConfig::Minimal) => "minimal",
            Some(ReasoningEffortConfig::Low) => "low",
            Some(ReasoningEffortConfig::Medium) => "medium",
            Some(ReasoningEffortConfig::High) => "high",
            Some(ReasoningEffortConfig::XHigh) => "xhigh",
            None | Some(ReasoningEffortConfig::None) => "default",
        }
    }

    fn reasoning_label_for(
        model: &str,
        reasoning_effort: Option<ReasoningEffortConfig>,
    ) -> Option<&'static str> {
        (!model.starts_with("savfox-auto-")).then(|| Self::reasoning_label(reasoning_effort))
    }

    pub(crate) fn token_usage(&self) -> savfox_core::protocol::TokenUsage {
        self.chat_screen.token_usage()
    }

    fn exit_summary_context(&self) -> (Option<String>, Option<PathBuf>) {
        if self.chat_screen.session_id().is_some() {
            (
                Some(self.current_model_display()),
                Some(self.config.cwd.clone()),
            )
        } else {
            (None, None)
        }
    }

    fn current_model_display(&self) -> String {
        let model = self.chat_screen.current_model().to_string();
        let reasoning = match self.config.model_reasoning_effort {
            None | Some(ReasoningEffortConfig::None) => None,
            effort => Self::reasoning_label_for(model.as_str(), effort),
        };
        if let Some(reasoning) = reasoning {
            format!("{model} {reasoning}")
        } else {
            model
        }
    }

    fn on_update_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        // TODO(aibrahim): Remove this and don't use config as a state object.
        // Instead, explicitly pass the stored collaboration mode's effort into new sessions.
        self.config.model_reasoning_effort = effort;
        self.chat_screen.set_reasoning_effort(effort);
    }

    fn on_update_personality(&mut self, personality: Personality) {
        self.config.personality = Some(personality);
        self.chat_screen.set_personality(personality);
    }

    fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    async fn launch_external_editor(&mut self, tui: &mut tui::Tui) {
        let editor_cmd = match external_editor::resolve_editor_command() {
            Ok(cmd) => cmd,
            Err(external_editor::EditorError::MissingEditor) => {
                self.chat_screen
                    .add_to_history(history_cell::new_error_event(
                    "Cannot open external editor: set $VISUAL or $EDITOR before starting Savfox."
                        .to_string(),
                ));
                self.reset_external_editor_state(tui);
                return;
            }
            Err(err) => {
                self.chat_screen
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to open editor: {err}",
                    )));
                self.reset_external_editor_state(tui);
                return;
            }
        };

        let seed = self.chat_screen.composer_text_with_pending();
        let editor_result = tui
            .with_restored(tui::RestoreMode::KeepRaw, || async {
                external_editor::run_editor(&seed, &editor_cmd).await
            })
            .await;
        self.reset_external_editor_state(tui);

        match editor_result {
            Ok(new_text) => {
                // Trim trailing whitespace
                let cleaned = new_text.trim_end().to_string();
                self.chat_screen.apply_external_edit(cleaned);
            }
            Err(err) => {
                self.chat_screen
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to open editor: {err}",
                    )));
            }
        }
        tui.frame_requester().schedule_frame();
    }

    fn request_external_editor_launch(&mut self, tui: &mut tui::Tui) {
        self.chat_screen
            .set_external_editor_state(ExternalEditorState::Requested);
        self.chat_screen.set_footer_hint_override(Some(vec![(
            EXTERNAL_EDITOR_HINT.to_string(),
            String::new(),
        )]));
        tui.frame_requester().schedule_frame();
    }

    fn reset_external_editor_state(&mut self, tui: &mut tui::Tui) {
        self.chat_screen
            .set_external_editor_state(ExternalEditorState::Closed);
        self.chat_screen.set_footer_hint_override(None);
        tui.frame_requester().schedule_frame();
    }

    async fn handle_key_event(&mut self, tui: &mut tui::Tui, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                // Enter alternate screen and set viewport to full size.
                let _ = tui.enter_alt_screen();
                self.overlay = Some(Overlay::new_transcript(self.transcript_cells.clone()));
                tui.frame_requester().schedule_frame();
            }
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                // Only launch the external editor if there is no overlay and the bottom pane is not
                // in use. Note that it can be launched while a task is running to
                // enable editing while the previous turn is ongoing.
                if self.overlay.is_none()
                    && self.chat_screen.can_launch_external_editor()
                    && self.chat_screen.external_editor_state() == ExternalEditorState::Closed
                {
                    self.request_external_editor_launch(tui);
                }
            }
            // Esc primes/advances backtracking only in normal (not working) mode
            // with the composer focused and empty. In any other state, forward
            // Esc so the active UI (e.g. status indicator, modals, popups)
            // handles it.
            KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                if self.chat_screen.is_normal_backtrack_mode()
                    && self.chat_screen.composer_is_empty()
                {
                    self.handle_backtrack_esc_key(tui);
                } else {
                    self.chat_screen.handle_key_event(key_event);
                }
            }
            // Enter confirms backtrack when primed + count > 0. Otherwise pass to widget.
            KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            } if self.backtrack.primed
                && self.backtrack.nth_user_message != usize::MAX
                && self.chat_screen.composer_is_empty() =>
            {
                if let Some(selection) = self.confirm_backtrack_from_main() {
                    self.apply_backtrack_selection(tui, selection);
                }
            }
            KeyEvent {
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                // Any non-Esc key press should cancel a primed backtrack.
                // This avoids stale "Esc-primed" state after the user starts typing
                // (even if they later backspace to empty).
                if key_event.code != KeyCode::Esc && self.backtrack.primed {
                    self.reset_backtrack_state();
                }
                self.chat_screen.handle_key_event(key_event);
            }
            _ => {
                // Ignore Release key events.
            }
        };
    }

    #[cfg(target_os = "windows")]
    fn spawn_world_writable_scan(
        cwd: PathBuf,
        env_map: std::collections::HashMap<String, String>,
        logs_base_dir: PathBuf,
        sandbox_policy: savfox_core::protocol::SandboxPolicy,
        tx: AppEventSender,
    ) {
        tokio::task::spawn_blocking(move || {
            let result = savfox_windows_sandbox::apply_world_writable_scan_and_denies(
                &logs_base_dir,
                &cwd,
                &env_map,
                &sandbox_policy,
                Some(logs_base_dir.as_path()),
            );
            if result.is_err() {
                // Scan failed: warn without examples.
                tx.send(AppEvent::OpenWorldWritableWarningConfirmation {
                    preset: None,
                    sample_paths: Vec::new(),
                    extra_count: 0usize,
                    failed_scan: true,
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use assert_matches::assert_matches;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::prelude::Line;
    use savfox_core::config::{ConfigBuilder, ConfigOverrides};
    use savfox_core::models_manager::manager::ModelsManager;
    use savfox_core::protocol::{
        AskForApproval, Event, EventMsg, SandboxPolicy, SessionConfiguredEvent, SessionSource,
    };
    use savfox_core::{AuthManager, SavfoxAuth, SessionManager};
    use savfox_otel::OtelManager;
    use savfox_protocol::SessionId;
    use savfox_protocol::user_input::TextElement;
    use tempfile::tempdir;
    use tokio::time;

    use super::*;
    use crate::app_backtrack::{BacktrackState, user_count};
    use crate::chat_screen::tests::make_chat_screen_manual_with_sender;
    use crate::file_search::FileSearchManager;
    use crate::history_cell::{AgentMessageCell, HistoryCell, UserHistoryCell, new_session_info};

    #[test]
    fn normalize_harness_overrides_resolves_relative_add_dirs() -> Result<()> {
        let temp_dir = tempdir()?;
        let base_cwd = temp_dir.path().join("base");
        std::fs::create_dir_all(&base_cwd)?;

        let overrides = ConfigOverrides {
            additional_writable_roots: vec![PathBuf::from("rel")],
            ..Default::default()
        };
        let normalized = normalize_harness_overrides_for_cwd(overrides, &base_cwd)?;

        assert_eq!(
            normalized.additional_writable_roots,
            vec![base_cwd.join("rel")]
        );
        Ok(())
    }

    #[tokio::test]
    async fn enqueue_session_event_does_not_block_when_channel_full() -> Result<()> {
        let mut app = make_test_app().await;
        let session_id = SessionId::new();
        app.session_event_channels
            .insert(session_id, SessionEventChannel::new(1));
        app.set_session_active(session_id, true).await;

        let event = Event {
            id: String::new(),
            msg: EventMsg::ShutdownComplete,
        };

        app.enqueue_session_event(session_id, event.clone()).await?;
        time::timeout(
            Duration::from_millis(50),
            app.enqueue_session_event(session_id, event),
        )
        .await
        .expect("enqueue_session_event blocked on a full channel")?;

        let mut rx = app
            .session_event_channels
            .get_mut(&session_id)
            .expect("missing session channel")
            .receiver
            .take()
            .expect("missing receiver");

        time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for first event")
            .expect("channel closed unexpectedly");
        time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for second event")
            .expect("channel closed unexpectedly");

        Ok(())
    }

    async fn make_test_app() -> App {
        let (chat_screen, app_event_tx, _rx, _op_rx) = make_chat_screen_manual_with_sender().await;
        let config = chat_screen.config_ref().clone();
        let server = Arc::new(SessionManager::with_models_provider(
            SavfoxAuth::from_api_key("Test API Key"),
            config.model_provider.clone(),
        ));
        let auth_manager =
            AuthManager::from_auth_for_testing(SavfoxAuth::from_api_key("Test API Key"));
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        let model = ModelsManager::get_model_offline(config.model.as_deref());
        let otel_manager = test_otel_manager(&config, model.as_str());

        App {
            server,
            otel_manager,
            app_event_tx,
            chat_screen,
            auth_manager,
            config,
            active_profile: None,
            cli_kv_overrides: Vec::new(),
            harness_overrides: ConfigOverrides::default(),
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            file_search,
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            enhanced_keys_supported: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            feedback: savfox_feedback::SavfoxFeedback::new(),
            feedback_audience: FeedbackAudience::External,
            pending_update_action: None,
            suppress_shutdown_complete: false,
            windows_sandbox: WindowsSandboxState::default(),
            session_event_channels: HashMap::new(),
            active_session_id: None,
            active_session_rx: None,
            primary_session_id: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
        }
    }

    async fn make_test_app_with_channels() -> (
        App,
        tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        tokio::sync::mpsc::UnboundedReceiver<Op>,
    ) {
        let (chat_screen, app_event_tx, rx, op_rx) = make_chat_screen_manual_with_sender().await;
        let config = chat_screen.config_ref().clone();
        let server = Arc::new(SessionManager::with_models_provider(
            SavfoxAuth::from_api_key("Test API Key"),
            config.model_provider.clone(),
        ));
        let auth_manager =
            AuthManager::from_auth_for_testing(SavfoxAuth::from_api_key("Test API Key"));
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        let model = ModelsManager::get_model_offline(config.model.as_deref());
        let otel_manager = test_otel_manager(&config, model.as_str());

        (
            App {
                server,
                otel_manager,
                app_event_tx,
                chat_screen,
                auth_manager,
                config,
                active_profile: None,
                cli_kv_overrides: Vec::new(),
                harness_overrides: ConfigOverrides::default(),
                runtime_approval_policy_override: None,
                runtime_sandbox_policy_override: None,
                file_search,
                transcript_cells: Vec::new(),
                overlay: None,
                deferred_history_lines: Vec::new(),
                has_emitted_history_lines: false,
                enhanced_keys_supported: false,
                commit_anim_running: Arc::new(AtomicBool::new(false)),
                backtrack: BacktrackState::default(),
                backtrack_render_pending: false,
                feedback: savfox_feedback::SavfoxFeedback::new(),
                feedback_audience: FeedbackAudience::External,
                pending_update_action: None,
                suppress_shutdown_complete: false,
                windows_sandbox: WindowsSandboxState::default(),
                session_event_channels: HashMap::new(),
                active_session_id: None,
                active_session_rx: None,
                primary_session_id: None,
                primary_session_configured: None,
                pending_primary_events: VecDeque::new(),
            },
            rx,
            op_rx,
        )
    }

    fn test_otel_manager(config: &Config, model: &str) -> OtelManager {
        let model_info = ModelsManager::construct_model_info_offline(model, config);
        OtelManager::new(
            SessionId::new(),
            model,
            model_info.slug.as_str(),
            None,
            None,
            None,
            false,
            "test".to_string(),
            SessionSource::Cli,
        )
    }

    fn all_model_presets() -> Vec<ModelPreset> {
        savfox_core::models_manager::model_presets::all_model_presets().clone()
    }

    fn model_migration_copy_to_plain_text(
        copy: &crate::model_migration::ModelMigrationCopy,
    ) -> String {
        if let Some(markdown) = copy.markdown.as_ref() {
            return markdown.clone();
        }
        let mut s = String::new();
        for span in &copy.heading {
            s.push_str(&span.content);
        }
        s.push('\n');
        s.push('\n');
        for line in &copy.content {
            for span in &line.spans {
                s.push_str(&span.content);
            }
            s.push('\n');
        }
        s
    }

    #[tokio::test]
    async fn model_migration_prompt_only_shows_for_deprecated_models() {
        let presets = all_model_presets();
        let seen = BTreeMap::new();
        let deprecated_with_upgrade: Vec<_> = presets
            .iter()
            .filter(|preset| preset.upgrade.is_some())
            .collect();
        assert!(
            !deprecated_with_upgrade.is_empty(),
            "expected at least one deprecated model with upgrade"
        );
        for preset in deprecated_with_upgrade.iter().take(3) {
            let target = preset
                .upgrade
                .as_ref()
                .expect("upgrade should be present")
                .id
                .as_str();
            assert!(
                should_show_model_migration_prompt(
                    preset.slug.as_str(),
                    target,
                    &seen,
                    presets.as_slice()
                ),
                "expected migration prompt for deprecated model {} -> {target}",
                preset.slug
            );
        }

        let stable = presets
            .iter()
            .find(|preset| preset.upgrade.is_none())
            .expect("expected at least one non-deprecated model preset");
        assert!(!should_show_model_migration_prompt(
            stable.slug.as_str(),
            stable.slug.as_str(),
            &seen,
            presets.as_slice()
        ));
    }

    #[tokio::test]
    async fn model_migration_prompt_respects_hide_flag_and_self_target() {
        let mut seen = BTreeMap::new();
        seen.insert("gpt-5".to_string(), "gpt-5.1".to_string());
        assert!(!should_show_model_migration_prompt(
            "gpt-5",
            "gpt-5.1",
            &seen,
            &all_model_presets()
        ));
        assert!(!should_show_model_migration_prompt(
            "gpt-5.1",
            "gpt-5.1",
            &seen,
            &all_model_presets()
        ));
    }

    #[tokio::test]
    async fn model_migration_prompt_skips_when_target_missing() {
        let mut available = all_model_presets();
        let mut current = available
            .iter()
            .find(|preset| preset.slug == "gpt-5-savfox")
            .cloned()
            .expect("preset present");
        current.upgrade = Some(ModelUpgrade {
            id: "missing-target".to_string(),
            reasoning_effort_mapping: None,
            migration_config_key: HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG.to_string(),
            model_link: None,
            upgrade_copy: None,
            migration_markdown: None,
        });
        available.retain(|preset| preset.slug != "gpt-5-savfox");
        available.push(current.clone());

        assert!(should_show_model_migration_prompt(
            &current.slug,
            "missing-target",
            &BTreeMap::new(),
            &available,
        ));

        assert!(target_preset_for_upgrade(&available, "missing-target").is_none());
    }

    #[tokio::test]
    async fn model_migration_prompt_shows_for_hidden_model() {
        let savfox_home = tempdir().expect("temp savfox home");
        let config = ConfigBuilder::default()
            .savfox_home(savfox_home.path().to_path_buf())
            .build()
            .await
            .expect("config");

        let available_models = all_model_presets();
        let current = available_models
            .iter()
            .find(|preset| preset.slug == "gpt-5.1-savfox")
            .cloned()
            .expect("gpt-5.1-savfox preset present");
        assert!(
            !current.show_in_picker,
            "expected gpt-5.1-savfox to be hidden from picker for this test"
        );

        let upgrade = current.upgrade.as_ref().expect("upgrade configured");
        assert!(
            should_show_model_migration_prompt(
                &current.slug,
                &upgrade.id,
                &config.notices.model_migrations,
                &available_models,
            ),
            "expected migration prompt to be eligible for hidden model"
        );

        let target = target_preset_for_upgrade(&available_models, &upgrade.id)
            .expect("upgrade target present");
        let target_description =
            (!target.description.is_empty()).then(|| target.description.clone());
        let can_opt_out = true;
        let copy = migration_copy_for_models(
            &current.slug,
            &upgrade.id,
            upgrade.model_link.clone(),
            upgrade.upgrade_copy.clone(),
            upgrade.migration_markdown.clone(),
            target.display_name.clone(),
            target_description,
            can_opt_out,
        );

        // Snapshot the copy we would show; rendering is covered by model_migration snapshots.
        assert_snapshot!(
            "model_migration_prompt_shows_for_hidden_model",
            model_migration_copy_to_plain_text(&copy)
        );
    }

    #[tokio::test]
    async fn update_reasoning_effort_updates_collaboration_mode() {
        let mut app = make_test_app().await;
        app.chat_screen
            .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

        app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

        assert_eq!(
            app.chat_screen.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn backtrack_selection_with_duplicate_history_targets_unique_turn() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let user_cell = |text: &str,
                         text_elements: Vec<TextElement>,
                         local_image_paths: Vec<PathBuf>|
         -> Arc<dyn HistoryCell> {
            Arc::new(UserHistoryCell {
                message: text.to_string(),
                text_elements,
                local_image_paths,
            }) as Arc<dyn HistoryCell>
        };
        let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(AgentMessageCell::new(
                vec![Line::from(text.to_string())],
                true,
            )) as Arc<dyn HistoryCell>
        };

        let make_header = |is_first| {
            let event = SessionConfiguredEvent {
                session_id: SessionId::new(),
                forked_from_id: None,
                session_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                approval_policy: AskForApproval::Never,
                sandbox_policy: SandboxPolicy::ReadOnly,
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: Some(PathBuf::new()),
            };
            Arc::new(new_session_info(
                app.chat_screen.config_ref(),
                app.chat_screen.current_model(),
                event,
                is_first,
                None,
            )) as Arc<dyn HistoryCell>
        };

        let placeholder = "[Image #1]";
        let edited_text = format!("follow-up (edited) {placeholder}");
        let edited_range = edited_text.len().saturating_sub(placeholder.len())..edited_text.len();
        let edited_text_elements = vec![TextElement::new(edited_range.into(), None)];
        let edited_local_image_paths = vec![PathBuf::from("/tmp/fake-image.png")];

        // Simulate a transcript with duplicated history (e.g., from prior backtracks)
        // and an edited turn appended after a session header boundary.
        app.transcript_cells = vec![
            make_header(true),
            user_cell("first question", Vec::new(), Vec::new()),
            agent_cell("answer first"),
            user_cell("follow-up", Vec::new(), Vec::new()),
            agent_cell("answer follow-up"),
            make_header(false),
            user_cell("first question", Vec::new(), Vec::new()),
            agent_cell("answer first"),
            user_cell(
                &edited_text,
                edited_text_elements.clone(),
                edited_local_image_paths.clone(),
            ),
            agent_cell("answer edited"),
        ];

        assert_eq!(user_count(&app.transcript_cells), 2);

        let base_id = SessionId::new();
        app.chat_screen.handle_savfox_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: base_id,
                forked_from_id: None,
                session_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                approval_policy: AskForApproval::Never,
                sandbox_policy: SandboxPolicy::ReadOnly,
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: Some(PathBuf::new()),
            }),
        });

        app.backtrack.base_id = Some(base_id);
        app.backtrack.primed = true;
        app.backtrack.nth_user_message = user_count(&app.transcript_cells).saturating_sub(1);

        let selection = app
            .confirm_backtrack_from_main()
            .expect("backtrack selection");
        assert_eq!(selection.nth_user_message, 1);
        assert_eq!(selection.prefill, edited_text);
        assert_eq!(selection.text_elements, edited_text_elements);
        assert_eq!(selection.local_image_paths, edited_local_image_paths);

        app.apply_backtrack_rollback(selection);

        let mut rollback_turns = None;
        while let Ok(op) = op_rx.try_recv() {
            if let Op::SessionRollback { num_turns } = op {
                rollback_turns = Some(num_turns);
            }
        }

        assert_eq!(rollback_turns, Some(1));
    }

    #[tokio::test]
    async fn new_session_requests_shutdown_for_previous_conversation() {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let session_id = SessionId::new();
        let event = SessionConfiguredEvent {
            session_id,
            forked_from_id: None,
            session_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::ReadOnly,
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            rollout_path: Some(PathBuf::new()),
        };

        app.chat_screen.handle_savfox_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(event),
        });

        while app_event_rx.try_recv().is_ok() {}
        while op_rx.try_recv().is_ok() {}

        app.shutdown_current_session().await;

        match op_rx.try_recv() {
            Ok(Op::Shutdown) => {}
            Ok(other) => panic!("expected Op::Shutdown, got {other:?}"),
            Err(_) => panic!("expected shutdown op to be sent"),
        }
    }

    #[tokio::test]
    async fn shutdown_complete_without_session_requests_immediate_exit() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        while app_event_rx.try_recv().is_ok() {}

        app.enqueue_primary_event(Event {
            id: String::new(),
            msg: EventMsg::ShutdownComplete,
        })
        .await?;

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::Exit(ExitMode::Immediate))
        );
        Ok(())
    }

    #[tokio::test]
    async fn session_summary_skip_when_no_usage_and_no_restore_target() {
        assert!(session_summary(TokenUsage::default(), None, None).is_none());
    }

    #[tokio::test]
    async fn session_summary_includes_resume_hint() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        };
        let conversation = SessionId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();

        let summary = session_summary(usage, Some(conversation), None).expect("summary");
        assert_eq!(
            summary.usage_line,
            Some("Token usage: total=12 input=10 output=2".to_string())
        );
        assert_eq!(
            summary.resume_command,
            Some("savfox resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }

    #[tokio::test]
    async fn session_summary_includes_restore_hint_even_with_zero_usage() {
        let conversation = SessionId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();

        let summary = session_summary(
            TokenUsage::default(),
            Some(conversation),
            Some("my-session".to_string()),
        )
        .expect("summary");
        assert_eq!(summary.usage_line, None);
        assert_eq!(
            summary.resume_command,
            Some("savfox resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }

    #[tokio::test]
    async fn session_summary_prefers_id_over_name_for_restore_command() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        };
        let conversation = SessionId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();

        let summary = session_summary(usage, Some(conversation), Some("my-session".to_string()))
            .expect("summary");
        assert_eq!(
            summary.resume_command,
            Some("savfox resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }
}
