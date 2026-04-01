#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::Widget;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, WidgetRef, Wrap};
use savfox_core::auth::{AuthCredentialsStoreMode, CLIENT_ID};
use savfox_core::config::edit::{ConfigEdit, ConfigEditsBuilder};
use savfox_core::config::provider_store::{
    account_id_exists, persist_provider_connection, provider_env_key_for_store,
    read_provider_store_api_key, slugify_account_id,
};
use savfox_core::{AuthManager, ModelProviderInfo};
use savfox_login_oauth::{DeviceCode, ServerOptions, ShutdownHandle, run_login_server};
use savfox_protocol::config_types::ForcedLoginMethod;
use tokio::sync::Notify;
use toml_edit::value as toml_edit_value;

use super::onboarding_screen::StepState;
use crate::onboarding::onboarding_screen::{KeyboardHandler, StepStateProvider};
use crate::provider_connect::{
    ConnectProviderCandidate, ProviderConnectResult, ProviderConnectRuntimeAuth, connect_provider,
    connect_provider_candidates, provider_has_auth_in_env, provider_requires_api_key,
    select_default_model,
};
use crate::shimmer::shimmer_spans;
use crate::tui::FrameRequester;

mod headless_chatgpt_login;

#[derive(Clone)]
pub(crate) enum SignInState {
    PickMode,
    OpenAiAuthMethod,
    ChatGptContinueInBrowser(ContinueInBrowserState),
    ChatGptDeviceCode(ContinueWithDeviceCodeState),
    ChatGptSuccessMessage,
    ApiKeyEntry(ApiKeyInputState),
    ProviderConnecting(ProviderConnectingState),
    /// After connection succeeds, ask the user to name this account.
    ProviderNaming(ProviderNamingState),
    ProviderConfigured(ProviderConfiguredState),
    ProviderError(ProviderErrorState),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignInOption {
    ChatGpt,
    DeviceCode,
    ApiKey,
}

const API_KEY_DISABLED_MESSAGE: &str = "API key login is disabled.";

#[derive(Clone, Default)]
pub(crate) struct ApiKeyInputState {
    value: String,
    prepopulated_from_env: bool,
    provider_id: String,
    provider_name: String,
    allow_empty_submit: bool,
}

#[derive(Clone)]
pub(crate) struct ProviderConnectingState {
    provider_name: String,
}

#[derive(Clone)]
pub(crate) struct ProviderNamingState {
    pub(crate) result: ProviderConnectResult,
    pub(crate) name_input: String,
    pub(crate) error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct ProviderConfiguredState {
    provider_name: String,
    imported_model_count: usize,
}

#[derive(Clone)]
pub(crate) struct ProviderErrorState {
    message: String,
}

#[derive(Clone)]
/// Used to manage the lifecycle of SpawnedLogin and ensure it gets cleaned up.
pub(crate) struct ContinueInBrowserState {
    auth_url: String,
    shutdown_flag: Option<ShutdownHandle>,
}

#[derive(Clone)]
pub(crate) struct ContinueWithDeviceCodeState {
    device_code: Option<DeviceCode>,
    cancel: Option<Arc<Notify>>,
}

impl Drop for ContinueInBrowserState {
    fn drop(&mut self) {
        if let Some(handle) = &self.shutdown_flag {
            handle.shutdown();
        }
    }
}

impl KeyboardHandler for AuthModeWidget {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if self.handle_api_key_entry_key_event(&key_event) {
            return;
        }

        let sign_in_state = { (*self.sign_in_state.read().unwrap()).clone() };
        match sign_in_state {
            SignInState::PickMode => self.handle_provider_picker_key_event(key_event),
            SignInState::OpenAiAuthMethod => self.handle_openai_auth_key_event(key_event),
            SignInState::ChatGptSuccessMessage => {
                if key_event.code == KeyCode::Enter {
                    self.start_provider_connect("openai".to_string(), None);
                }
            }
            SignInState::ProviderError(_) => {
                if matches!(key_event.code, KeyCode::Enter | KeyCode::Esc) {
                    self.error = None;
                    *self.sign_in_state.write().unwrap() = SignInState::PickMode;
                    self.request_frame.schedule_frame();
                }
            }
            SignInState::ChatGptContinueInBrowser(_) => {
                if key_event.code == KeyCode::Esc {
                    *self.sign_in_state.write().unwrap() = SignInState::OpenAiAuthMethod;
                    self.request_frame.schedule_frame();
                }
            }
            SignInState::ChatGptDeviceCode(state) => {
                if key_event.code == KeyCode::Esc {
                    if let Some(cancel) = &state.cancel {
                        cancel.notify_one();
                    }
                    *self.sign_in_state.write().unwrap() = SignInState::OpenAiAuthMethod;
                    self.request_frame.schedule_frame();
                }
            }
            SignInState::ProviderNaming(state) => {
                self.handle_provider_naming_key_event(key_event, state);
            }
            SignInState::ApiKeyEntry(_)
            | SignInState::ProviderConnecting(_)
            | SignInState::ProviderConfigured(_) => {}
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        if let SignInState::ProviderNaming(ref mut state) = *self.sign_in_state.write().unwrap() {
            state.name_input.push_str(pasted.trim());
            self.request_frame.schedule_frame();
            return;
        }
        let _ = self.handle_api_key_entry_paste(pasted);
    }
}

#[derive(Clone)]
pub(crate) struct AuthModeWidget {
    pub request_frame: FrameRequester,
    pub highlighted_mode: SignInOption,
    pub highlighted_provider_index: usize,
    pub provider_search_query: String,
    pub error: Option<String>,
    pub sign_in_state: Arc<RwLock<SignInState>>,
    pub savfox_home: PathBuf,
    pub cli_auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub auth_manager: Arc<AuthManager>,
    pub model_providers: HashMap<String, ModelProviderInfo>,
    pub forced_chatgpt_workspace_id: Option<String>,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub animations_enabled: bool,
}

impl AuthModeWidget {
    fn is_api_login_allowed(&self) -> bool {
        !matches!(self.forced_login_method, Some(ForcedLoginMethod::Chatgpt))
    }

    fn is_chatgpt_login_allowed(&self) -> bool {
        !matches!(self.forced_login_method, Some(ForcedLoginMethod::Api))
    }

    fn filtered_provider_candidates(&self) -> Vec<ConnectProviderCandidate> {
        let mut candidates = connect_provider_candidates(&self.model_providers);
        let query = self.provider_search_query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return candidates;
        }
        candidates.retain(|candidate| {
            let haystack = format!(
                "{} {} {}",
                candidate.name, candidate.id, candidate.description
            )
            .to_ascii_lowercase();
            haystack.contains(&query)
        });
        candidates
    }

    fn move_provider_highlight(&mut self, delta: isize) {
        let candidates = self.filtered_provider_candidates();
        if candidates.is_empty() {
            self.highlighted_provider_index = 0;
            return;
        }

        let current = self
            .highlighted_provider_index
            .min(candidates.len().saturating_sub(1));
        let next = (current as isize + delta).rem_euclid(candidates.len() as isize) as usize;
        self.highlighted_provider_index = next;
    }

    fn handle_provider_picker_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_provider_highlight(-1);
                self.request_frame.schedule_frame();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_provider_highlight(1);
                self.request_frame.schedule_frame();
            }
            KeyCode::Backspace => {
                self.provider_search_query.pop();
                self.highlighted_provider_index = 0;
                self.request_frame.schedule_frame();
            }
            KeyCode::Enter => {
                self.open_highlighted_provider();
            }
            KeyCode::Char(c)
                if key_event.kind == KeyEventKind::Press
                    && !key_event.modifiers.contains(KeyModifiers::SUPER)
                    && !key_event.modifiers.contains(KeyModifiers::CONTROL)
                    && !key_event.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.provider_search_query.push(c);
                self.highlighted_provider_index = 0;
                self.request_frame.schedule_frame();
            }
            _ => {}
        }
    }

    fn open_highlighted_provider(&mut self) {
        let candidates = self.filtered_provider_candidates();
        if candidates.is_empty() {
            return;
        }
        let idx = self
            .highlighted_provider_index
            .min(candidates.len().saturating_sub(1));
        let candidate = candidates[idx].clone();
        self.error = None;

        if candidate.id.eq_ignore_ascii_case("openai") {
            self.highlighted_mode = if self.is_chatgpt_login_allowed() {
                SignInOption::ChatGpt
            } else {
                SignInOption::ApiKey
            };
            *self.sign_in_state.write().unwrap() = SignInState::OpenAiAuthMethod;
            self.request_frame.schedule_frame();
            return;
        }

        if provider_requires_api_key(&candidate.id) {
            self.start_api_key_entry_for_provider(candidate.id, candidate.name);
        } else {
            self.start_provider_connect(candidate.id, None);
        }
    }

    fn displayed_sign_in_options(&self) -> Vec<SignInOption> {
        let mut options = vec![SignInOption::ChatGpt];
        if self.is_chatgpt_login_allowed() {
            options.push(SignInOption::DeviceCode);
        }
        if self.is_api_login_allowed() {
            options.push(SignInOption::ApiKey);
        }
        options
    }

    fn selectable_sign_in_options(&self) -> Vec<SignInOption> {
        let mut options = Vec::new();
        if self.is_chatgpt_login_allowed() {
            options.push(SignInOption::ChatGpt);
            options.push(SignInOption::DeviceCode);
        }
        if self.is_api_login_allowed() {
            options.push(SignInOption::ApiKey);
        }
        options
    }

    fn handle_openai_auth_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_highlight(-1);
                self.request_frame.schedule_frame();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_highlight(1);
                self.request_frame.schedule_frame();
            }
            KeyCode::Char('1') => {
                self.select_option_by_index(0);
            }
            KeyCode::Char('2') => {
                self.select_option_by_index(1);
            }
            KeyCode::Char('3') => {
                self.select_option_by_index(2);
            }
            KeyCode::Enter => {
                self.handle_sign_in_option(self.highlighted_mode);
            }
            KeyCode::Esc => {
                self.error = None;
                *self.sign_in_state.write().unwrap() = SignInState::PickMode;
                self.request_frame.schedule_frame();
            }
            _ => {}
        }
    }

    fn move_highlight(&mut self, delta: isize) {
        let options = self.selectable_sign_in_options();
        if options.is_empty() {
            return;
        }

        let current_index = options
            .iter()
            .position(|option| *option == self.highlighted_mode)
            .unwrap_or(0);
        let next_index =
            (current_index as isize + delta).rem_euclid(options.len() as isize) as usize;
        self.highlighted_mode = options[next_index];
    }

    fn select_option_by_index(&mut self, index: usize) {
        let options = self.displayed_sign_in_options();
        if let Some(option) = options.get(index).copied() {
            self.handle_sign_in_option(option);
        }
    }

    fn handle_sign_in_option(&mut self, option: SignInOption) {
        match option {
            SignInOption::ChatGpt => {
                if self.is_chatgpt_login_allowed() {
                    self.start_chatgpt_login();
                }
            }
            SignInOption::DeviceCode => {
                if self.is_chatgpt_login_allowed() {
                    self.start_device_code_login();
                }
            }
            SignInOption::ApiKey => {
                if self.is_api_login_allowed() {
                    self.start_api_key_entry();
                } else {
                    self.disallow_api_login();
                }
            }
        }
    }

    fn disallow_api_login(&mut self) {
        self.highlighted_mode = SignInOption::ChatGpt;
        self.error = Some(API_KEY_DISABLED_MESSAGE.to_string());
        *self.sign_in_state.write().unwrap() = SignInState::OpenAiAuthMethod;
        self.request_frame.schedule_frame();
    }

    fn render_pick_mode(&self, area: Rect, buf: &mut Buffer) {
        let candidates = self.filtered_provider_candidates();
        let mut lines: Vec<Line> = vec![
            Line::from(vec!["  ".into(), "Configure a model provider".into()]),
            Line::from(vec![
                "  ".into(),
                "Type to search, then press Enter to connect".into(),
            ]),
            "".into(),
        ];

        if self.provider_search_query.is_empty() {
            lines.push(Line::from(vec![
                "  Search: ".dim(),
                "type provider name or id".dim(),
            ]));
        } else {
            lines.push(Line::from(vec![
                "  Search: ".dim(),
                self.provider_search_query.as_str().cyan(),
            ]));
        }
        lines.push("".into());

        if candidates.is_empty() {
            lines.push("  No providers match your search.".dim().into());
        } else {
            let total = candidates.len();
            let selected = self.highlighted_provider_index.min(total.saturating_sub(1));
            let visible_limit = 8usize;
            let window_start = selected.saturating_sub(visible_limit.saturating_sub(1));
            let window_end = (window_start + visible_limit).min(total);

            for (offset, candidate) in candidates[window_start..window_end].iter().enumerate() {
                let absolute_index = window_start + offset;
                let is_selected = absolute_index == selected;
                let caret = if is_selected { ">" } else { " " };
                let provider_line = if is_selected {
                    Line::from(vec![
                        format!("{caret} ").cyan().dim(),
                        candidate.name.as_str().cyan(),
                        " ".into(),
                        format!("({})", candidate.id).dim(),
                    ])
                } else {
                    Line::from(vec![
                        format!("  {}", candidate.name).into(),
                        format!(" ({})", candidate.id).dim(),
                    ])
                };
                lines.push(provider_line);
                if is_selected {
                    lines.push(
                        Line::from(format!("    {}", candidate.description))
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::DIM),
                    );
                } else {
                    lines.push(Line::from(format!("    {}", candidate.description)).dim());
                }
                lines.push("".into());
            }

            if window_end < total {
                lines.push(
                    Line::from(format!(
                        "  … {} more provider(s). Keep typing to narrow results.",
                        total - window_end
                    ))
                    .dim(),
                );
            }
        }

        lines.push("".into());
        lines.push("  Up/Down: navigate   Enter: select".dim().into());
        if let Some(err) = &self.error {
            lines.push("".into());
            lines.push(err.as_str().red().into());
        }

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_openai_auth_mode(&self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line> = vec![
            Line::from(vec![
                "  ".into(),
                "OpenAI provider: choose how to authenticate".into(),
            ]),
            "".into(),
        ];

        let create_mode_item = |idx: usize,
                                selected_mode: SignInOption,
                                text: &str,
                                description: &str|
         -> Vec<Line<'static>> {
            let is_selected = self.highlighted_mode == selected_mode;
            let caret = if is_selected { ">" } else { " " };

            let line1 = if is_selected {
                Line::from(vec![
                    format!("{caret} {index}. ", index = idx + 1).cyan().dim(),
                    text.to_string().cyan(),
                ])
            } else {
                format!("  {index}. {text}", index = idx + 1).into()
            };

            let line2 = if is_selected {
                Line::from(format!("     {description}"))
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::DIM)
            } else {
                Line::from(format!("     {description}"))
                    .style(Style::default().add_modifier(Modifier::DIM))
            };

            vec![line1, line2]
        };

        let chatgpt_description = if !self.is_chatgpt_login_allowed() {
            "ChatGPT login is disabled"
        } else {
            "Usage included with Plus, Pro, Team, and Enterprise plans"
        };
        let device_code_description = "Sign in from another device with a one-time code";

        for (idx, option) in self.displayed_sign_in_options().into_iter().enumerate() {
            match option {
                SignInOption::ChatGpt => {
                    lines.extend(create_mode_item(
                        idx,
                        option,
                        "Sign in with ChatGPT",
                        chatgpt_description,
                    ));
                }
                SignInOption::DeviceCode => {
                    lines.extend(create_mode_item(
                        idx,
                        option,
                        "Sign in with Device Code",
                        device_code_description,
                    ));
                }
                SignInOption::ApiKey => {
                    lines.extend(create_mode_item(
                        idx,
                        option,
                        "Provide your own API key",
                        "Pay for what you use",
                    ));
                }
            }
            lines.push("".into());
        }

        lines.push("  Enter to continue, Esc to go back".dim().into());
        if let Some(err) = &self.error {
            lines.push("".into());
            lines.push(err.as_str().red().into());
        }

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_continue_in_browser(&self, area: Rect, buf: &mut Buffer) {
        let mut spans = vec!["  ".into()];
        if self.animations_enabled {
            // Schedule a follow-up frame to keep the shimmer animation going.
            self.request_frame
                .schedule_frame_in(std::time::Duration::from_millis(100));
            spans.extend(shimmer_spans("Finish signing in via your browser"));
        } else {
            spans.push("Finish signing in via your browser".into());
        }
        let mut lines = vec![spans.into(), "".into()];

        let sign_in_state = self.sign_in_state.read().unwrap();
        if let SignInState::ChatGptContinueInBrowser(state) = &*sign_in_state
            && !state.auth_url.is_empty()
        {
            lines.push("  If the link doesn't open automatically, open the following link to authenticate:".into());
            lines.push("".into());
            lines.push(Line::from(vec![
                "  ".into(),
                state.auth_url.as_str().cyan().underlined(),
            ]));
            lines.push("".into());
            lines.push(Line::from(vec![
                "  On a remote or headless machine? Press Esc and choose ".into(),
                "Sign in with Device Code".cyan(),
                ".".into(),
            ]));
            lines.push("".into());
        }

        lines.push("  Press Esc to cancel".dim().into());
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_chatgpt_success_message(&self, area: Rect, buf: &mut Buffer) {
        let lines = vec![
            "✓ Signed in with your ChatGPT account"
                .fg(Color::Green)
                .into(),
            "".into(),
            "  Press Enter to import OpenAI models and continue."
                .fg(Color::Cyan)
                .into(),
        ];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_api_key_entry(&self, area: Rect, buf: &mut Buffer, state: &ApiKeyInputState) {
        let [intro_area, input_area, footer_area] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Min(2),
        ])
        .areas(area);

        let mut intro_lines: Vec<Line> = vec![
            Line::from(vec![
                "> ".into(),
                format!("Connect {}", state.provider_name).bold(),
            ]),
            "".into(),
            "  Paste or type your API key below. It will be saved for this provider.".into(),
            "".into(),
        ];
        if state.prepopulated_from_env {
            intro_lines.push("  Detected credentials from your environment.".into());
            intro_lines.push(
                "  Paste a different key if you prefer to use another account."
                    .dim()
                    .into(),
            );
            intro_lines.push("".into());
        }
        if state.allow_empty_submit {
            intro_lines.push(
                "  Leave blank and press Enter to reuse existing saved/environment credentials."
                    .dim()
                    .into(),
            );
            intro_lines.push("".into());
        }
        Paragraph::new(intro_lines)
            .wrap(Wrap { trim: false })
            .render(intro_area, buf);

        let content_line: Line = if state.value.is_empty() {
            vec!["Paste or type your API key".dim()].into()
        } else {
            Line::from(state.value.clone())
        };
        Paragraph::new(content_line)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(format!("{} API key", state.provider_name))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .render(input_area, buf);

        let mut footer_lines: Vec<Line> = vec![
            "  Press Enter to save".dim().into(),
            "  Press Esc to go back".dim().into(),
        ];
        if let Some(error) = &self.error {
            footer_lines.push("".into());
            footer_lines.push(error.as_str().red().into());
        }
        Paragraph::new(footer_lines)
            .wrap(Wrap { trim: false })
            .render(footer_area, buf);
    }

    fn handle_api_key_entry_key_event(&mut self, key_event: &KeyEvent) -> bool {
        let mut should_connect: Option<(String, Option<String>)> = None;
        let mut should_request_frame = false;

        {
            let mut guard = self.sign_in_state.write().unwrap();
            if let SignInState::ApiKeyEntry(state) = &mut *guard {
                match key_event.code {
                    KeyCode::Esc => {
                        *guard = SignInState::PickMode;
                        self.error = None;
                        should_request_frame = true;
                    }
                    KeyCode::Enter => {
                        let trimmed = state.value.trim().to_string();
                        if trimmed.is_empty() && !state.allow_empty_submit {
                            self.error = Some("API key cannot be empty".to_string());
                            should_request_frame = true;
                        } else {
                            let api_key = (!trimmed.is_empty()).then_some(trimmed);
                            should_connect = Some((state.provider_id.clone(), api_key));
                        }
                    }
                    KeyCode::Backspace => {
                        if state.prepopulated_from_env {
                            state.value.clear();
                            state.prepopulated_from_env = false;
                        } else {
                            state.value.pop();
                        }
                        self.error = None;
                        should_request_frame = true;
                    }
                    KeyCode::Char(c)
                        if key_event.kind == KeyEventKind::Press
                            && !key_event.modifiers.contains(KeyModifiers::SUPER)
                            && !key_event.modifiers.contains(KeyModifiers::CONTROL)
                            && !key_event.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        if state.prepopulated_from_env {
                            state.value.clear();
                            state.prepopulated_from_env = false;
                        }
                        state.value.push(c);
                        self.error = None;
                        should_request_frame = true;
                    }
                    _ => {}
                }
                // handled; let guard drop before potential save
            } else {
                return false;
            }
        }

        if let Some((provider_id, api_key)) = should_connect {
            self.start_provider_connect(provider_id, api_key);
        } else if should_request_frame {
            self.request_frame.schedule_frame();
        }
        true
    }

    fn handle_api_key_entry_paste(&mut self, pasted: String) -> bool {
        let trimmed = pasted.trim();
        if trimmed.is_empty() {
            return false;
        }

        let mut guard = self.sign_in_state.write().unwrap();
        if let SignInState::ApiKeyEntry(state) = &mut *guard {
            if state.prepopulated_from_env {
                state.value = trimmed.to_string();
                state.prepopulated_from_env = false;
            } else {
                state.value.push_str(trimmed);
            }
            self.error = None;
        } else {
            return false;
        }

        drop(guard);
        self.request_frame.schedule_frame();
        true
    }

    fn start_api_key_entry(&mut self) {
        self.start_api_key_entry_for_provider("openai".to_string(), "OpenAI".to_string());
    }

    fn start_api_key_entry_for_provider(&mut self, provider_id: String, provider_name: String) {
        if !self.is_api_login_allowed() {
            self.disallow_api_login();
            return;
        }

        let Some(provider) = self.model_providers.get(provider_id.as_str()).cloned() else {
            self.error = Some(format!("Unknown provider: {provider_id}"));
            *self.sign_in_state.write().unwrap() = SignInState::PickMode;
            self.request_frame.schedule_frame();
            return;
        };

        let env_prefill = provider_env_key_for_store(&provider_id, &provider)
            .and_then(|env_key| std::env::var(&env_key).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let allow_empty_submit = read_provider_store_api_key(&self.savfox_home, &provider_id)
            .is_some()
            || provider_has_auth_in_env(&provider_id, &provider);

        self.error = None;
        let mut guard = self.sign_in_state.write().unwrap();
        match &mut *guard {
            SignInState::ApiKeyEntry(state) => {
                if state.value.is_empty() {
                    if let Some(prefill) = env_prefill.clone() {
                        state.value = prefill;
                        state.prepopulated_from_env = true;
                    } else {
                        state.prepopulated_from_env = false;
                    }
                }
                state.provider_id = provider_id.clone();
                state.provider_name = provider_name.clone();
                state.allow_empty_submit = allow_empty_submit;
            }
            _ => {
                *guard = SignInState::ApiKeyEntry(ApiKeyInputState {
                    value: env_prefill.clone().unwrap_or_default(),
                    prepopulated_from_env: env_prefill.is_some(),
                    provider_id: provider_id.clone(),
                    provider_name: provider_name.clone(),
                    allow_empty_submit,
                });
            }
        }
        drop(guard);
        self.request_frame.schedule_frame();
    }

    fn start_provider_connect(&mut self, provider_id: String, api_key: Option<String>) {
        let Some(provider) = self.model_providers.get(provider_id.as_str()).cloned() else {
            self.error = Some(format!("Unknown provider: {provider_id}"));
            *self.sign_in_state.write().unwrap() = SignInState::PickMode;
            self.request_frame.schedule_frame();
            return;
        };

        let provider_name = provider.name.clone();
        self.error = None;
        *self.sign_in_state.write().unwrap() =
            SignInState::ProviderConnecting(ProviderConnectingState {
                provider_name: provider_name.clone(),
            });
        self.request_frame.schedule_frame();

        let sign_in_state = self.sign_in_state.clone();
        let request_frame = self.request_frame.clone();
        let savfox_home = self.savfox_home.clone();
        let auth_manager = self.auth_manager.clone();

        tokio::spawn(async move {
            let runtime_auth = auth_manager
                .auth()
                .await
                .map(|auth| ProviderConnectRuntimeAuth {
                    bearer_token: auth
                        .get_token()
                        .ok()
                        .and_then(|token| (!token.trim().is_empty()).then_some(token)),
                    account_id: auth.get_account_id().and_then(|account_id| {
                        (!account_id.trim().is_empty()).then_some(account_id)
                    }),
                    use_chatgpt_openai_base_url: auth.is_chatgpt_auth(),
                });

            let result = connect_provider(
                savfox_home.clone(),
                provider_id.clone(),
                provider,
                api_key,
                runtime_auth,
                None,
            )
            .await;

            let final_state = match result {
                Ok(result) => {
                    if result.models.is_empty() {
                        SignInState::ProviderError(ProviderErrorState {
                            message: format!(
                                "Connected {}, but no models were returned.",
                                result.provider_name
                            ),
                        })
                    } else {
                        SignInState::ProviderNaming(ProviderNamingState {
                            result,
                            name_input: String::new(),
                            error: None,
                        })
                    }
                }
                Err(err) => SignInState::ProviderError(ProviderErrorState { message: err }),
            };

            *sign_in_state.write().unwrap() = final_state;
            request_frame.schedule_frame();
        });
    }

    fn handle_provider_naming_key_event(
        &mut self,
        key_event: KeyEvent,
        mut state: ProviderNamingState,
    ) {
        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                // Cancel — go back to provider picker.
                *self.sign_in_state.write().unwrap() = SignInState::PickMode;
                self.request_frame.schedule_frame();
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                let name = state.name_input.trim().to_string();
                let account_id = if name.is_empty() {
                    state.result.provider_id.clone()
                } else {
                    slugify_account_id(&state.result.provider_id, &name)
                };

                // Check for conflict (skip check when using bare provider_id).
                if account_id != state.result.provider_id
                    && account_id_exists(&self.savfox_home, &account_id)
                {
                    state.error = Some(format!(
                        "Account '{}' already exists. Choose a different name.",
                        account_id
                    ));
                    state.name_input.clear();
                    *self.sign_in_state.write().unwrap() = SignInState::ProviderNaming(state);
                    self.request_frame.schedule_frame();
                    return;
                }

                state.result.account_id = account_id;
                self.finalize_provider_connect(state.result, &name);
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                state.name_input.pop();
                *self.sign_in_state.write().unwrap() = SignInState::ProviderNaming(state);
                self.request_frame.schedule_frame();
            }
            KeyEvent {
                code: KeyCode::Char(c),
                kind: KeyEventKind::Press,
                ..
            } => {
                state.name_input.push(c);
                *self.sign_in_state.write().unwrap() = SignInState::ProviderNaming(state);
                self.request_frame.schedule_frame();
            }
            _ => {}
        }
    }

    fn finalize_provider_connect(&mut self, result: ProviderConnectResult, account_name: &str) {
        let savfox_home = self.savfox_home.clone();
        let sign_in_state = self.sign_in_state.clone();
        let request_frame = self.request_frame.clone();
        let account_name = account_name.to_string();

        tokio::spawn(async move {
            let account_id = result.account_id.clone();
            let final_state = if let Err(err) = persist_provider_connection(
                savfox_home.as_path(),
                &account_id,
                &result.provider_id,
                &account_name,
                &result.models,
                result.env_key.as_deref(),
                result.api_key.as_deref(),
            ) {
                SignInState::ProviderError(ProviderErrorState {
                    message: format!("Failed to save provider settings: {err}"),
                })
            } else {
                savfox_core::inject_provider_auth_overrides_from_store(savfox_home.as_path());

                match select_default_model(&result.models, account_id.as_str()) {
                    Some(default_model) => {
                        // Strip any old provider prefix from the model (it may
                        // carry the preliminary account_id from before naming).
                        let normalized_model =
                            savfox_core::parse_provider_prefixed_model(default_model.as_str())
                                .map(|(_, slug)| slug.to_string())
                                .unwrap_or(default_model);
                        let mut edits: Vec<ConfigEdit> = Vec::new();
                        if !result.base_url.trim().is_empty() {
                            edits.push(ConfigEdit::SetPath {
                                segments: vec![
                                    "model_providers".to_string(),
                                    account_id.clone(),
                                    "base_url".to_string(),
                                ],
                                value: toml_edit_value(result.base_url.clone()),
                            });
                        }
                        let model_to_persist =
                            format!("{}/{}", account_id, normalized_model.trim());
                        let persist_result = ConfigEditsBuilder::new(&savfox_home)
                            .with_edits(edits)
                            .set_model(Some(model_to_persist.as_str()), None)
                            .apply()
                            .await;

                        match persist_result {
                            Ok(()) => SignInState::ProviderConfigured(ProviderConfiguredState {
                                provider_name: result.provider_name,
                                imported_model_count: result.models.len(),
                            }),
                            Err(err) => SignInState::ProviderError(ProviderErrorState {
                                message: format!(
                                    "Connected provider, but failed to update config: {err}"
                                ),
                            }),
                        }
                    }
                    None => SignInState::ProviderError(ProviderErrorState {
                        message: format!(
                            "Connected {}, but no usable model ID was returned.",
                            result.provider_name
                        ),
                    }),
                }
            };

            *sign_in_state.write().unwrap() = final_state;
            request_frame.schedule_frame();
        });
    }

    fn handle_existing_chatgpt_login(&mut self) -> bool {
        if self
            .auth_manager
            .auth_cached()
            .is_some_and(|auth| auth.is_chatgpt_auth())
        {
            *self.sign_in_state.write().unwrap() = SignInState::ChatGptSuccessMessage;
            self.request_frame.schedule_frame();
            true
        } else {
            false
        }
    }

    /// Kicks off the ChatGPT auth flow and keeps the UI state consistent with the attempt.
    fn start_chatgpt_login(&mut self) {
        // If we're already authenticated with ChatGPT, don't start a new login –
        // just proceed to the success message flow.
        if self.handle_existing_chatgpt_login() {
            return;
        }

        self.error = None;
        let opts = ServerOptions::new(
            self.savfox_home.clone(),
            CLIENT_ID.to_string(),
            self.forced_chatgpt_workspace_id.clone(),
            self.cli_auth_credentials_store_mode,
        );

        match run_login_server(opts) {
            Ok(child) => {
                let sign_in_state = self.sign_in_state.clone();
                let request_frame = self.request_frame.clone();
                let auth_manager = self.auth_manager.clone();
                tokio::spawn(async move {
                    let auth_url = child.auth_url.clone();
                    {
                        *sign_in_state.write().unwrap() =
                            SignInState::ChatGptContinueInBrowser(ContinueInBrowserState {
                                auth_url,
                                shutdown_flag: Some(child.cancel_handle()),
                            });
                    }
                    request_frame.schedule_frame();
                    let r = child.block_until_done().await;
                    match r {
                        Ok(()) => {
                            // Force the auth manager to reload the new auth information.
                            auth_manager.reload();

                            *sign_in_state.write().unwrap() = SignInState::ChatGptSuccessMessage;
                            request_frame.schedule_frame();
                        }
                        _ => {
                            *sign_in_state.write().unwrap() = SignInState::OpenAiAuthMethod;
                            // self.error = Some(e.to_string());
                            request_frame.schedule_frame();
                        }
                    }
                });
            }
            Err(e) => {
                *self.sign_in_state.write().unwrap() = SignInState::OpenAiAuthMethod;
                self.error = Some(e.to_string());
                self.request_frame.schedule_frame();
            }
        }
    }

    fn start_device_code_login(&mut self) {
        if self.handle_existing_chatgpt_login() {
            return;
        }

        self.error = None;
        let opts = ServerOptions::new(
            self.savfox_home.clone(),
            CLIENT_ID.to_string(),
            self.forced_chatgpt_workspace_id.clone(),
            self.cli_auth_credentials_store_mode,
        );
        headless_chatgpt_login::start_headless_chatgpt_login(self, opts);
    }
}

impl StepStateProvider for AuthModeWidget {
    fn get_step_state(&self) -> StepState {
        let sign_in_state = self.sign_in_state.read().unwrap();
        match &*sign_in_state {
            SignInState::PickMode
            | SignInState::OpenAiAuthMethod
            | SignInState::ApiKeyEntry(_)
            | SignInState::ChatGptContinueInBrowser(_)
            | SignInState::ChatGptDeviceCode(_)
            | SignInState::ChatGptSuccessMessage
            | SignInState::ProviderConnecting(_)
            | SignInState::ProviderNaming(_)
            | SignInState::ProviderError(_) => StepState::InProgress,
            SignInState::ProviderConfigured(_) => StepState::Complete,
        }
    }
}

impl WidgetRef for AuthModeWidget {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let sign_in_state = self.sign_in_state.read().unwrap();
        match &*sign_in_state {
            SignInState::PickMode => {
                self.render_pick_mode(area, buf);
            }
            SignInState::OpenAiAuthMethod => {
                self.render_openai_auth_mode(area, buf);
            }
            SignInState::ChatGptContinueInBrowser(_) => {
                self.render_continue_in_browser(area, buf);
            }
            SignInState::ChatGptDeviceCode(state) => {
                headless_chatgpt_login::render_device_code_login(self, area, buf, state);
            }
            SignInState::ChatGptSuccessMessage => {
                self.render_chatgpt_success_message(area, buf);
            }
            SignInState::ApiKeyEntry(state) => {
                self.render_api_key_entry(area, buf, state);
            }
            SignInState::ProviderConnecting(state) => {
                let mut lines = vec!["".into()];
                if self.animations_enabled {
                    self.request_frame
                        .schedule_frame_in(std::time::Duration::from_millis(100));
                    lines.push(Line::from(shimmer_spans(&format!(
                        "Connecting {}...",
                        state.provider_name
                    ))));
                } else {
                    lines.push(Line::from(format!("Connecting {}...", state.provider_name)));
                }
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .render(area, buf);
            }
            SignInState::ProviderNaming(state) => {
                let mut lines = vec![
                    Line::from(format!(
                        "✓ Connected {} ({} model(s) found)",
                        state.result.provider_name,
                        state.result.models.len()
                    ))
                    .fg(Color::Green),
                    "".into(),
                ];
                if let Some(err) = &state.error {
                    lines.push(Line::from(err.clone()).fg(Color::Red));
                    lines.push("".into());
                }
                lines.push("  Name this account (e.g. 'Work', 'Personal')".into());
                lines.push(
                    "  Leave empty and press Enter for the default name."
                        .dim()
                        .into(),
                );
                lines.push("".into());
                let cursor_line = format!("  > {}_", state.name_input);
                lines.push(Line::from(cursor_line).fg(Color::Cyan));
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .render(area, buf);
            }
            SignInState::ProviderConfigured(state) => {
                let lines = vec![
                    Line::from(format!("✓ Connected {}", state.provider_name)).fg(Color::Green),
                    "".into(),
                    Line::from(format!(
                        "  Imported {} model(s) and updated your config.",
                        state.imported_model_count
                    )),
                ];
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .render(area, buf);
            }
            SignInState::ProviderError(state) => {
                let lines = vec![
                    "Provider connection failed".red().into(),
                    "".into(),
                    Line::from(state.message.clone()),
                    "".into(),
                    "Press Enter to choose another provider".dim().into(),
                ];
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .render(area, buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use savfox_core::auth::AuthCredentialsStoreMode;
    use savfox_core::built_in_model_providers;
    use tempfile::TempDir;

    use super::*;

    fn widget_forced_chatgpt() -> (AuthModeWidget, TempDir) {
        let savfox_home = TempDir::new().unwrap();
        let savfox_home_path = savfox_home.path().to_path_buf();
        let widget = AuthModeWidget {
            request_frame: FrameRequester::test_dummy(),
            highlighted_mode: SignInOption::ChatGpt,
            highlighted_provider_index: 0,
            provider_search_query: String::new(),
            error: None,
            sign_in_state: Arc::new(RwLock::new(SignInState::PickMode)),
            savfox_home: savfox_home_path.clone(),
            cli_auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            auth_manager: AuthManager::shared(
                savfox_home_path,
                false,
                AuthCredentialsStoreMode::File,
            ),
            model_providers: built_in_model_providers(),
            forced_chatgpt_workspace_id: None,
            forced_login_method: Some(ForcedLoginMethod::Chatgpt),
            animations_enabled: true,
        };
        (widget, savfox_home)
    }

    #[test]
    fn api_key_flow_disabled_when_chatgpt_forced() {
        let (mut widget, _tmp) = widget_forced_chatgpt();

        widget.start_api_key_entry();

        assert_eq!(widget.error.as_deref(), Some(API_KEY_DISABLED_MESSAGE));
        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::OpenAiAuthMethod
        ));
    }

    #[test]
    fn selecting_openai_api_key_option_is_blocked_when_chatgpt_forced() {
        let (mut widget, _tmp) = widget_forced_chatgpt();

        widget.handle_sign_in_option(SignInOption::ApiKey);

        assert_eq!(widget.error.as_deref(), Some(API_KEY_DISABLED_MESSAGE));
        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::OpenAiAuthMethod
        ));
    }
}
