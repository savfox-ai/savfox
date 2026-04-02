//! The chat composer is the bottom-pane text input state machine.
//!
//! It is responsible for:
//!
//! - Editing the input buffer (a [`TextArea`]), including placeholder "elements" for attachments.
//! - Routing keys to the active popup (slash commands, file search, skill/apps mentions).
//! - Promoting typed slash commands into atomic elements when the command name is completed.
//! - Handling submit vs newline on Enter.
//! - Turning raw key streams into explicit paste operations on platforms where terminals don't
//!   provide reliable bracketed paste (notably Windows).
//!
//! # Key Event Routing
//!
//! Most key handling goes through [`ChatComposer::handle_key_event`], which dispatches to a
//! popup-specific handler if a popup is visible and otherwise to
//! [`ChatComposer::handle_key_event_without_popup`]. After every handled key, we call
//! [`ChatComposer::sync_popups`] so UI state follows the latest buffer/cursor.
//!
//! # History Navigation (Up/Down)
//!
//! The Up/Down history path is managed by [`ChatComposerHistory`]. It merges:
//!
//! - Persistent cross-session history (text-only; no element ranges or attachments).
//! - Local in-session history (full text + text elements + local image paths).
//!
//! When recalling a local entry, the composer rehydrates text elements and image attachments.
//! When recalling a persistent entry, only the text is restored.
//!
//! # Submission and Prompt Expansion
//!
//! On submit/queue paths, the composer:
//!
//! - Expands pending paste placeholders so element ranges align with the final text.
//! - Trims whitespace and rebases text elements accordingly.
//! - Expands `/prompts:` custom prompts (named or numeric args), preserving text elements.
//! - Prunes attached images so only placeholders that survive expansion are sent.
//!
//! The numeric auto-submit path used by the slash popup performs the same pending-paste expansion
//! and attachment pruning, and clears pending paste state on success.
//! Slash commands with arguments (like `/plan` and `/review`) reuse the same preparation path so
//! pasted content and text elements are preserved when extracting args.
//!
//! # Non-bracketed Paste Bursts
//!
//! On some terminals (especially on Windows), pastes arrive as a rapid sequence of
//! `KeyCode::Char` and `KeyCode::Enter` key events instead of a single paste event.
//!
//! To avoid misinterpreting these bursts as real typing (and to prevent transient UI effects like
//! shortcut overlays toggling on a pasted `?`), we feed "plain" character events into
//! [`PasteBurst`](super::paste_burst::PasteBurst), which buffers bursts and later flushes them
//! through [`ChatComposer::handle_paste`].
//!
//! The burst detector intentionally treats ASCII and non-ASCII differently:
//!
//! - ASCII: we briefly hold the first fast char (flicker suppression) until we know whether the
//!   stream is paste-like.
//! - non-ASCII: we do not hold the first char (IME input would feel dropped), but we still allow
//!   burst detection for actual paste streams.
//!
//! The burst detector can also be disabled (`disable_paste_burst`), which bypasses the state
//! machine and treats the key stream as normal typing. When toggling from enabled to disabled, the
//! composer flushes/clears any in-flight burst state so it cannot leak into subsequent input.
//!
//! For the detailed burst state machine, see `savfox-rs/tui/src/bottom_pane/paste_burst.rs`.
//! For a narrative overview of the combined state machine, see `docs/tui-chat-composer.md`.
//!
//! # PasteBurst Integration Points
//!
//! The burst detector is consulted in a few specific places:
//!
//! - [`ChatComposer::handle_input_basic`]: flushes any due burst first, then intercepts plain char
//!   input to either buffer it or insert normally.
//! - [`ChatComposer::handle_non_ascii_char`]: handles the non-ASCII/IME path without holding the
//!   first char, while still allowing paste detection via retro-capture.
//! - [`ChatComposer::flush_paste_burst_if_due`]/[`ChatComposer::handle_paste_burst_flush`]: called
//!   from UI ticks to turn a pending burst into either an explicit paste (`handle_paste`) or a
//!   normal typed character.
//!
//! # Input Disabled Mode
//!
//! The composer can be temporarily read-only (`input_enabled = false`). In that mode it ignores
//! edits and renders a placeholder prompt instead of the editable textarea. This is part of the
//! overall state machine, since it affects which transitions are even possible from a given UI
//! state.

// Sub-modules containing logically grouped impl blocks for ChatComposer.
mod history_navigator;
mod paste_detector;
mod popup_manager;
mod rendering;
mod submission_handler;
mod text_editor;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::KeyCode;
use ratatui::style::Color;
use savfox_core::skills::model::SkillMetadata;
use savfox_protocol::custom_prompts::{CustomPrompt, PROMPTS_CMD_PREFIX};
use savfox_protocol::user_input::TextElement;

use super::chat_composer_history::ChatComposerHistory;
use super::command_popup::CommandPopup;
use super::file_search_popup::FileSearchPopup;
use super::footer::{
    CollaborationModeIndicator, FooterMode, esc_hint_mode, reset_mode_after_activity,
};
use super::paste_burst::PasteBurst;
use super::skill_popup::SkillPopup;
use crate::app_event::ConnectorsSnapshot;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::prompt_args::{
    expand_if_numeric_with_positional_args, prompt_argument_names,
    prompt_command_with_arg_placeholders, prompt_has_numeric_placeholders,
};
use crate::bottom_pane::textarea::{TextArea, TextAreaState};
use crate::key_hint::{self, KeyBinding};
/// If the pasted content exceeds this number of characters, replace it with a
/// placeholder in the UI.
const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;

/// Result returned when the user interacts with the text area.
#[derive(Debug, PartialEq)]
pub enum InputResult {
    Submitted {
        text: String,
        text_elements: Vec<TextElement>,
    },
    Queued {
        text: String,
        text_elements: Vec<TextElement>,
    },
    Command(crate::slash_command::SlashCommand),
    CommandWithArgs(crate::slash_command::SlashCommand, String, Vec<TextElement>),
    None,
}

#[derive(Clone, Debug, PartialEq)]
struct AttachedImage {
    placeholder: String,
    path: PathBuf,
}

enum PromptSelectionMode {
    Completion,
    Submit,
}

enum PromptSelectionAction {
    Insert {
        text: String,
        cursor: Option<usize>,
    },
    Submit {
        text: String,
        text_elements: Vec<TextElement>,
    },
}

/// Feature flags for reusing the chat composer in other bottom-pane surfaces.
///
/// The default keeps today's behavior intact. Other call sites can opt out of
/// specific behaviors by constructing a config with those flags set to `false`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChatComposerConfig {
    /// Whether command/file/skill popups are allowed to appear.
    pub(crate) popups_enabled: bool,
    /// Whether `/...` input is parsed and dispatched as slash commands.
    pub(crate) slash_commands_enabled: bool,
    /// Whether pasting a file path can attach local images.
    pub(crate) image_paste_enabled: bool,
}

impl Default for ChatComposerConfig {
    fn default() -> Self {
        Self {
            popups_enabled: true,
            slash_commands_enabled: true,
            image_paste_enabled: true,
        }
    }
}

impl ChatComposerConfig {
    /// A minimal preset for plain-text inputs embedded in other surfaces.
    ///
    /// This disables popups, slash commands, and image-path attachment behavior
    /// so the composer behaves like a simple notes field.
    pub(crate) const fn plain_text() -> Self {
        Self {
            popups_enabled: false,
            slash_commands_enabled: false,
            image_paste_enabled: false,
        }
    }
}

pub(crate) struct ChatComposer {
    textarea: TextArea,
    textarea_state: RefCell<TextAreaState>,
    active_popup: ActivePopup,
    app_event_tx: AppEventSender,
    history: ChatComposerHistory,
    quit_shortcut_expires_at: Option<Instant>,
    quit_shortcut_key: KeyBinding,
    esc_backtrack_hint: bool,
    use_shift_enter_hint: bool,
    dismissed_file_popup_token: Option<String>,
    current_file_query: Option<String>,
    pending_pastes: Vec<(String, String)>,
    large_paste_counters: HashMap<usize, usize>,
    has_focus: bool,
    /// Invariant: attached images are labeled `[Image #1]..[Image #N]` in vec order.
    attached_images: Vec<AttachedImage>,
    placeholder_text: String,
    is_task_running: bool,
    /// When false, the composer is temporarily read-only (e.g. during sandbox setup).
    input_enabled: bool,
    input_disabled_placeholder: Option<String>,
    /// Non-bracketed paste burst tracker (see `bottom_pane/paste_burst.rs`).
    paste_burst: PasteBurst,
    // When true, disables paste-burst logic and inserts characters immediately.
    disable_paste_burst: bool,
    custom_prompts: Vec<CustomPrompt>,
    footer_mode: FooterMode,
    footer_hint_override: Option<Vec<(String, String)>>,
    footer_flash: Option<FooterFlash>,
    context_window_percent: Option<i64>,
    context_window_used_tokens: Option<i64>,
    model_display: String,
    provider_display: String,
    cwd_display: String,
    /// Whether we are in session mode (first message sent, sidebar visible).
    is_session_mode: bool,
    /// Whether the main app is currently rendering in the terminal alternate screen.
    is_alt_screen_active: bool,
    skills: Option<Vec<SkillMetadata>>,
    connectors_snapshot: Option<ConnectorsSnapshot>,
    dismissed_mention_popup_token: Option<String>,
    mention_paths: HashMap<String, String>,
    /// When enabled, `Enter` submits immediately and `Tab` requests queuing behavior.
    steer_enabled: bool,
    collaboration_modes_enabled: bool,
    config: ChatComposerConfig,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    connectors_enabled: bool,
    personality_command_enabled: bool,
    windows_degraded_sandbox_active: bool,
    /// Border color for the composer (e.g., based on collaboration mode)
    border_color: Option<Color>,
}

#[derive(Clone, Debug)]
struct FooterFlash {
    line: ratatui::text::Line<'static>,
    expires_at: Instant,
}

/// Popup state -- at most one can be visible at any time.
enum ActivePopup {
    None,
    Command(CommandPopup),
    File(FileSearchPopup),
    Skill(SkillPopup),
}

const FOOTER_SPACING_HEIGHT: u16 = 0;
const STARTUP_PLACEHOLDER_TEXT: &str =
    "Type @ to mention files, / for commands, or ? for shortcuts";

// ── Constructor and simple setters ──

impl ChatComposer {
    pub fn new(
        has_input_focus: bool,
        app_event_tx: AppEventSender,
        enhanced_keys_supported: bool,
        placeholder_text: String,
        disable_paste_burst: bool,
    ) -> Self {
        Self::new_with_config(
            has_input_focus,
            app_event_tx,
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
            ChatComposerConfig::default(),
        )
    }

    /// Construct a composer with explicit feature gating.
    ///
    /// This enables reuse in contexts like request-user-input where we want
    /// the same visuals and editing behavior without slash commands or popups.
    pub(crate) fn new_with_config(
        has_input_focus: bool,
        app_event_tx: AppEventSender,
        enhanced_keys_supported: bool,
        placeholder_text: String,
        disable_paste_burst: bool,
        config: ChatComposerConfig,
    ) -> Self {
        let use_shift_enter_hint = enhanced_keys_supported;

        let mut this = Self {
            textarea: TextArea::new(),
            textarea_state: RefCell::new(TextAreaState::default()),
            active_popup: ActivePopup::None,
            app_event_tx,
            history: ChatComposerHistory::new(),
            quit_shortcut_expires_at: None,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            esc_backtrack_hint: false,
            use_shift_enter_hint,
            dismissed_file_popup_token: None,
            current_file_query: None,
            pending_pastes: Vec::new(),
            large_paste_counters: HashMap::new(),
            has_focus: has_input_focus,
            attached_images: Vec::new(),
            placeholder_text,
            is_task_running: false,
            input_enabled: true,
            input_disabled_placeholder: None,
            paste_burst: PasteBurst::default(),
            disable_paste_burst: false,
            custom_prompts: Vec::new(),
            footer_mode: FooterMode::ComposerEmpty,
            footer_hint_override: None,
            footer_flash: None,
            context_window_percent: None,
            context_window_used_tokens: None,
            model_display: String::new(),
            provider_display: String::new(),
            cwd_display: String::new(),
            is_session_mode: false,
            is_alt_screen_active: false,
            skills: None,
            connectors_snapshot: None,
            dismissed_mention_popup_token: None,
            mention_paths: HashMap::new(),
            steer_enabled: false,
            collaboration_modes_enabled: false,
            config,
            collaboration_mode_indicator: None,
            connectors_enabled: false,
            personality_command_enabled: false,
            windows_degraded_sandbox_active: false,
            border_color: None,
        };
        // Apply configuration via the setter to keep side-effects centralized.
        this.set_disable_paste_burst(disable_paste_burst);
        this
    }

    pub fn set_skill_mentions(&mut self, skills: Option<Vec<SkillMetadata>>) {
        self.skills = skills;
    }

    /// Toggle composer-side image paste handling.
    ///
    /// This only affects whether image-like paste content is converted into attachments; the
    /// `ChatScreen` layer still performs capability checks before images are submitted.
    pub fn set_image_paste_enabled(&mut self, enabled: bool) {
        self.config.image_paste_enabled = enabled;
    }

    pub fn set_connector_mentions(&mut self, connectors_snapshot: Option<ConnectorsSnapshot>) {
        self.connectors_snapshot = connectors_snapshot;
    }

    pub(crate) fn take_mention_paths(&mut self) -> HashMap<String, String> {
        std::mem::take(&mut self.mention_paths)
    }

    /// Enables or disables "Steer" behavior for submission keys.
    ///
    /// When steer is enabled, `Enter` produces [`InputResult::Submitted`] (send immediately) and
    /// `Tab` produces [`InputResult::Queued`] (eligible to queue if a task is running).
    /// When steer is disabled, `Enter` produces [`InputResult::Queued`], preserving the default
    /// "queue while a task is running" behavior.
    pub fn set_steer_enabled(&mut self, enabled: bool) {
        self.steer_enabled = enabled;
    }

    pub fn set_collaboration_modes_enabled(&mut self, enabled: bool) {
        self.collaboration_modes_enabled = enabled;
    }

    pub fn set_connectors_enabled(&mut self, enabled: bool) {
        self.connectors_enabled = enabled;
    }

    /// Set the border color for the composer area.
    pub fn set_border_color(&mut self, color: Option<Color>) {
        self.border_color = color;
    }

    pub fn set_collaboration_mode_indicator(
        &mut self,
        indicator: Option<CollaborationModeIndicator>,
    ) {
        self.collaboration_mode_indicator = indicator;
    }

    pub fn set_personality_command_enabled(&mut self, enabled: bool) {
        self.personality_command_enabled = enabled;
    }

    /// Centralized feature gating keeps config checks out of call sites.
    fn popups_enabled(&self) -> bool {
        self.config.popups_enabled
    }

    fn slash_commands_enabled(&self) -> bool {
        self.config.slash_commands_enabled
    }

    fn image_paste_enabled(&self) -> bool {
        self.config.image_paste_enabled
    }

    #[cfg(target_os = "windows")]
    pub fn set_windows_degraded_sandbox_active(&mut self, enabled: bool) {
        self.windows_degraded_sandbox_active = enabled;
    }

    /// Show the transient "press again to quit" hint for `key`.
    ///
    /// The owner (`BottomPane`/`ChatScreen`) is responsible for scheduling a
    /// redraw after [`super::QUIT_SHORTCUT_TIMEOUT`] so the hint can disappear
    /// even when the UI is otherwise idle.
    pub fn show_quit_shortcut_hint(&mut self, key: KeyBinding, has_focus: bool) {
        self.quit_shortcut_expires_at = Instant::now()
            .checked_add(super::QUIT_SHORTCUT_TIMEOUT)
            .or_else(|| Some(Instant::now()));
        self.quit_shortcut_key = key;
        self.footer_mode = FooterMode::QuitShortcutReminder;
        self.set_has_focus(has_focus);
    }

    /// Clear the "press again to quit" hint immediately.
    pub fn clear_quit_shortcut_hint(&mut self, has_focus: bool) {
        self.quit_shortcut_expires_at = None;
        self.footer_mode = reset_mode_after_activity(self.footer_mode);
        self.set_has_focus(has_focus);
    }

    /// Whether the quit shortcut hint should currently be shown.
    ///
    /// This is time-based rather than event-based: it may become false without
    /// any additional user input, so the UI schedules a redraw when the hint
    /// expires.
    pub(crate) fn quit_shortcut_hint_visible(&self) -> bool {
        self.quit_shortcut_expires_at
            .is_some_and(|expires_at| Instant::now() < expires_at)
    }

    pub fn skills(&self) -> Option<&Vec<SkillMetadata>> {
        self.skills.as_ref()
    }

    fn mentions_enabled(&self) -> bool {
        let skills_ready = self
            .skills
            .as_ref()
            .is_some_and(|skills| !skills.is_empty());
        let connectors_ready = self.connectors_enabled
            && self
                .connectors_snapshot
                .as_ref()
                .is_some_and(|snapshot| !snapshot.connectors.is_empty());
        skills_ready || connectors_ready
    }

    fn set_has_focus(&mut self, has_focus: bool) {
        self.has_focus = has_focus;
    }

    #[allow(dead_code)]
    pub(crate) fn set_input_enabled(&mut self, enabled: bool, placeholder: Option<String>) {
        self.input_enabled = enabled;
        self.input_disabled_placeholder = if enabled { None } else { placeholder };

        // Avoid leaving interactive popups open while input is blocked.
        if !enabled && !matches!(self.active_popup, ActivePopup::None) {
            self.active_popup = ActivePopup::None;
        }
    }

    pub fn set_task_running(&mut self, running: bool) {
        self.is_task_running = running;
    }

    pub(crate) fn set_context_window(&mut self, percent: Option<i64>, used_tokens: Option<i64>) {
        if self.context_window_percent == percent && self.context_window_used_tokens == used_tokens
        {
            return;
        }
        self.context_window_percent = percent;
        self.context_window_used_tokens = used_tokens;
    }

    pub(crate) fn set_model_display(&mut self, model: String, provider: String) {
        self.model_display = model;
        self.provider_display = provider;
    }

    pub(crate) fn set_cwd_display(&mut self, cwd: String) {
        self.cwd_display = cwd;
    }

    pub(crate) fn set_session_mode(&mut self, session_mode: bool) {
        self.is_session_mode = session_mode;
    }

    pub(crate) fn set_alt_screen_active(&mut self, active: bool) {
        self.is_alt_screen_active = active;
    }

    pub(crate) fn set_esc_backtrack_hint(&mut self, show: bool) {
        self.esc_backtrack_hint = show;
        if show {
            self.footer_mode = esc_hint_mode(self.footer_mode, self.is_task_running);
        } else {
            self.footer_mode = reset_mode_after_activity(self.footer_mode);
        }
    }
}

// ── Free functions ──

fn skill_name(skill: &SkillMetadata) -> &str {
    skill
        .interface
        .as_ref()
        .and_then(|interface| interface.name.as_deref())
        .unwrap_or(&skill.name)
}

fn skill_description(skill: &SkillMetadata) -> Option<String> {
    let description = skill
        .interface
        .as_ref()
        .and_then(|interface| interface.short_description.as_deref())
        .or(skill.short_description.as_deref())
        .unwrap_or(&skill.description);
    let trimmed = description.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn is_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')
}

fn prompt_selection_action(
    prompt: &CustomPrompt,
    first_line: &str,
    mode: PromptSelectionMode,
    text_elements: &[TextElement],
) -> PromptSelectionAction {
    let named_args = prompt_argument_names(&prompt.content);
    let has_numeric = prompt_has_numeric_placeholders(&prompt.content);

    match mode {
        PromptSelectionMode::Completion => {
            if !named_args.is_empty() {
                let (text, cursor) =
                    prompt_command_with_arg_placeholders(&prompt.name, &named_args);
                return PromptSelectionAction::Insert {
                    text,
                    cursor: Some(cursor),
                };
            }
            if has_numeric {
                let text = format!("/{PROMPTS_CMD_PREFIX}:{} ", prompt.name);
                return PromptSelectionAction::Insert { text, cursor: None };
            }
            let text = format!("/{PROMPTS_CMD_PREFIX}:{}", prompt.name);
            PromptSelectionAction::Insert { text, cursor: None }
        }
        PromptSelectionMode::Submit => {
            if !named_args.is_empty() {
                let (text, cursor) =
                    prompt_command_with_arg_placeholders(&prompt.name, &named_args);
                return PromptSelectionAction::Insert {
                    text,
                    cursor: Some(cursor),
                };
            }
            if has_numeric {
                if let Some(expanded) =
                    expand_if_numeric_with_positional_args(prompt, first_line, text_elements)
                {
                    return PromptSelectionAction::Submit {
                        text: expanded.text,
                        text_elements: expanded.text_elements,
                    };
                }
                let text = format!("/{PROMPTS_CMD_PREFIX}:{} ", prompt.name);
                return PromptSelectionAction::Insert { text, cursor: None };
            }
            PromptSelectionAction::Submit {
                text: prompt.content.clone(),
                // By now we know this custom prompt has no args, so no text elements to preserve.
                text_elements: Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    // ── Test includes ──
    use std::time::{Duration, Instant};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use image::{ImageBuffer, Rgba};
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use savfox_protocol::models::local_image_label_text;
    use savfox_protocol::user_input::ByteRange;
    use tempfile::tempdir;
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;
    use crate::app_event::AppEvent;
    use crate::bottom_pane::chat_composer::{AttachedImage, LARGE_PASTE_CHAR_THRESHOLD};
    use crate::bottom_pane::chat_composer_history::HistoryEntry;
    use crate::bottom_pane::footer::{FooterMode, footer_height};
    use crate::bottom_pane::paste_burst::PasteBurst;
    use crate::bottom_pane::prompt_args::{PromptArg, extract_positional_args_for_prompt_line};
    use crate::bottom_pane::textarea::TextArea;
    use crate::bottom_pane::{AppEventSender, ChatComposer, InputResult};
    use crate::render::renderable::Renderable;

    #[test]
    fn footer_hint_row_is_separated_from_composer() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_string(),
            false,
        );

        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        composer.render(area, &mut buf);

        let row_to_string = |y: u16| {
            let mut row = String::new();
            for x in 0..area.width {
                row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            row
        };

        let mut hint_row: Option<(u16, String)> = None;
        for y in 0..area.height {
            let row = row_to_string(y);
            if row.contains("? for shortcuts") {
                hint_row = Some((y, row));
                break;
            }
        }

        let (hint_row_idx, hint_row_contents) =
            hint_row.expect("expected footer hint row to be rendered");
        assert_eq!(
            hint_row_idx,
            area.height - 1,
            "hint row should occupy the bottom line: {hint_row_contents:?}",
        );

        assert!(
            hint_row_idx > 0,
            "expected a spacing row above the footer hints",
        );

        let spacing_row = row_to_string(hint_row_idx - 1);
        let trimmed_spacing = spacing_row.trim();
        let separator_spacing =
            !trimmed_spacing.is_empty() && trimmed_spacing.chars().all(|ch| ch == '\u{2500}');
        assert!(
            trimmed_spacing.is_empty() || separator_spacing,
            "expected blank/separator row above hints but saw: {spacing_row:?}",
        );
    }

    #[test]
    fn footer_flash_overrides_footer_hint_override() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_string(),
            false,
        );
        composer.set_footer_hint_override(Some(vec![("K".to_string(), "label".to_string())]));
        composer.show_footer_flash(ratatui::text::Line::from("FLASH"), Duration::from_secs(10));

        let area = Rect::new(0, 0, 60, 6);
        let mut buf = Buffer::empty(area);
        composer.render(area, &mut buf);

        let mut bottom_row = String::new();
        for x in 0..area.width {
            bottom_row.push(
                buf[(x, area.height - 1)]
                    .symbol()
                    .chars()
                    .next()
                    .unwrap_or(' '),
            );
        }
        assert!(
            bottom_row.contains("FLASH"),
            "expected flash content to render in footer row, saw: {bottom_row:?}",
        );
        assert!(
            !bottom_row.contains("K label"),
            "expected flash to override hint override, saw: {bottom_row:?}",
        );
    }

    #[test]
    fn footer_flash_expires_and_falls_back_to_hint_override() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_string(),
            false,
        );
        composer.set_footer_hint_override(Some(vec![("K".to_string(), "label".to_string())]));
        composer.show_footer_flash(ratatui::text::Line::from("FLASH"), Duration::from_secs(10));
        composer.footer_flash.as_mut().unwrap().expires_at =
            Instant::now() - Duration::from_secs(1);

        let area = Rect::new(0, 0, 60, 6);
        let mut buf = Buffer::empty(area);
        composer.render(area, &mut buf);

        let mut bottom_row = String::new();
        for x in 0..area.width {
            bottom_row.push(
                buf[(x, area.height - 1)]
                    .symbol()
                    .chars()
                    .next()
                    .unwrap_or(' '),
            );
        }
        assert!(
            bottom_row.contains("K label"),
            "expected hint override to render after flash expired, saw: {bottom_row:?}",
        );
        assert!(
            !bottom_row.contains("FLASH"),
            "expected expired flash to be hidden, saw: {bottom_row:?}",
        );
    }

    // Include the remaining tests via a separate file to keep mod.rs focused.
    // Tests are kept inline here rather than in sub-modules because they need
    // access to private fields and internal types.
    include!("tests.rs");
}
