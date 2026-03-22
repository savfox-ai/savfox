//! The bottom-pane footer renders transient hints and context indicators.
//!
//! The footer is pure rendering: it formats `FooterProps` into `Line`s without mutating any state.
//! It intentionally does not decide *which* footer content should be shown; that is owned by the
//! `ChatComposer` (which selects a `FooterMode`) and by higher-level state machines like
//! `ChatScreen` (which decides when quit/interrupt is allowed).
//!
//! Some footer content is time-based rather than event-based, such as the "press again to quit"
//! hint. The owning widgets schedule redraws so time-based hints can expire even if the UI is
//! otherwise idle.
//!
//! Single-line collapse overview:
//! 1. The composer decides the current `FooterMode` and hint flags, then calls
//!    `single_line_footer_layout` for the base single-line modes.
//! 2. `single_line_footer_layout` applies the width-based fallback rules: (If this description is
//!    hard to follow, just try it out by resizing your terminal width; these rules were built out
//!    of trial and error.)
//!    - Start with the fullest left-side hint plus the right-side context.
//!    - When the queue hint is active, prefer keeping that queue hint visible, even if it means
//!      dropping the right-side context earlier; the queue hint may also be shortened before it is
//!      removed.
//!    - When the queue hint is not active but the mode cycle hint is applicable, drop "? for
//!      shortcuts" before dropping "(shift+tab to cycle)".
//!    - If "(shift+tab to cycle)" cannot fit, also hide the right-side context to avoid too many
//!      state transitions in quick succession.
//!    - Finally, try a mode-only line (with and without context), and fall back to no left-side
//!      footer if nothing can fit.
//! 3. When collapse chooses a specific line, callers render it via `render_footer_line`. Otherwise,
//!    callers render the straightforward mode-to-text mapping via `render_footer_from_props`.
//!
//! In short: `single_line_footer_layout` chooses *what* best fits, and the two
//! render helpers choose whether to draw the chosen line or the default
//! `FooterProps` mapping.
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::render::line_utils::prefix_lines;
use crate::status::format_tokens_compact;
use crate::ui_consts::FOOTER_INDENT_COLS;

/// The rendering inputs for the footer area under the composer.
///
/// Callers are expected to construct `FooterProps` from higher-level state (`ChatComposer`,
/// `BottomPane`, and `ChatScreen`) and pass it to the footer render helpers
/// (`render_footer_from_props` or the single-line collapse logic). The footer
/// treats these values as authoritative and does not attempt to infer missing
/// state (for example, it does not query whether a task is running).
#[derive(Clone, Debug)]
pub(crate) struct FooterProps {
    pub(crate) mode: FooterMode,
    pub(crate) esc_backtrack_hint: bool,
    pub(crate) use_shift_enter_hint: bool,
    pub(crate) is_task_running: bool,
    pub(crate) steer_enabled: bool,
    pub(crate) collaboration_modes_enabled: bool,
    pub(crate) is_wsl: bool,
    /// Which key the user must press again to quit.
    ///
    /// This is rendered when `mode` is `FooterMode::QuitShortcutReminder`.
    pub(crate) quit_shortcut_key: KeyBinding,
    pub(crate) context_window_percent: Option<i64>,
    pub(crate) context_window_used_tokens: Option<i64>,
    /// Model display name shown in the right-side footer context.
    pub(crate) model_display: String,
    /// Provider display name shown in the right-side footer context.
    pub(crate) provider_display: String,
    /// Current working directory displayed in the footer.
    pub(crate) cwd_display: String,
    /// When true, render the session-mode footer layout (model status line +
    /// shortcut hints + path info) instead of the startup right-aligned context line.
    pub(crate) is_session_mode: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CollaborationModeIndicator {
    Plan,
    PairProgramming,
    Execute,
}

const MODE_CYCLE_HINT: &str = "shift+tab to cycle";
const FOOTER_CONTEXT_GAP_COLS: u16 = 1;

impl CollaborationModeIndicator {
    fn label(self, show_cycle_hint: bool) -> String {
        let suffix = if show_cycle_hint {
            format!(" ({MODE_CYCLE_HINT})")
        } else {
            String::new()
        };
        match self {
            CollaborationModeIndicator::Plan => format!("Plan mode{suffix}"),
            CollaborationModeIndicator::PairProgramming => {
                format!("Pair Programming mode{suffix}")
            }
            CollaborationModeIndicator::Execute => format!("Execute mode{suffix}"),
        }
    }

    fn styled_span(self, show_cycle_hint: bool) -> Span<'static> {
        let label = self.label(show_cycle_hint);
        match self {
            CollaborationModeIndicator::Plan => Span::from(label).magenta(),
            CollaborationModeIndicator::PairProgramming => Span::from(label).cyan(),
            CollaborationModeIndicator::Execute => Span::from(label).dim(),
        }
    }
}

/// Selects which footer content is rendered.
///
/// The current mode is owned by `ChatComposer`, which may override it based on transient state
/// (for example, showing `QuitShortcutReminder` only while its timer is active).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FooterMode {
    /// Transient "press again to quit" reminder (Ctrl+C/Ctrl+D).
    QuitShortcutReminder,
    /// Multi-line shortcut overlay shown after pressing `?`.
    ShortcutOverlay,
    /// Transient "press Esc again" hint shown after the first Esc while idle.
    EscHint,
    /// Base single-line footer when the composer is empty.
    ComposerEmpty,
    /// Base single-line footer when the composer contains a draft.
    ///
    /// The shortcuts hint is suppressed here; when a task is running with
    /// steer enabled, this mode can show the queue hint instead.
    ComposerHasDraft,
}

pub(crate) fn toggle_shortcut_mode(
    current: FooterMode,
    ctrl_c_hint: bool,
    is_empty: bool,
) -> FooterMode {
    if ctrl_c_hint && matches!(current, FooterMode::QuitShortcutReminder) {
        return current;
    }

    let base_mode = if is_empty {
        FooterMode::ComposerEmpty
    } else {
        FooterMode::ComposerHasDraft
    };

    match current {
        FooterMode::ShortcutOverlay | FooterMode::QuitShortcutReminder => base_mode,
        _ => FooterMode::ShortcutOverlay,
    }
}

pub(crate) fn esc_hint_mode(current: FooterMode, is_task_running: bool) -> FooterMode {
    if is_task_running {
        current
    } else {
        FooterMode::EscHint
    }
}

pub(crate) fn reset_mode_after_activity(current: FooterMode) -> FooterMode {
    match current {
        FooterMode::EscHint
        | FooterMode::ShortcutOverlay
        | FooterMode::QuitShortcutReminder
        | FooterMode::ComposerHasDraft => FooterMode::ComposerEmpty,
        other => other,
    }
}

pub(crate) fn footer_height(props: FooterProps) -> u16 {
    let show_shortcuts_hint = match props.mode {
        FooterMode::ComposerEmpty => true,
        FooterMode::QuitShortcutReminder
        | FooterMode::ShortcutOverlay
        | FooterMode::EscHint
        | FooterMode::ComposerHasDraft => false,
    };
    let show_queue_hint = match props.mode {
        FooterMode::ComposerHasDraft => props.is_task_running && props.steer_enabled,
        FooterMode::QuitShortcutReminder
        | FooterMode::ComposerEmpty
        | FooterMode::ShortcutOverlay
        | FooterMode::EscHint => false,
    };
    footer_from_props_lines(props, None, false, show_shortcuts_hint, show_queue_hint).len() as u16
}

/// Render a single precomputed footer line.
pub(crate) fn render_footer_line(area: Rect, buf: &mut Buffer, line: Line<'static>) {
    Paragraph::new(prefix_lines(
        vec![line],
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    ))
    .render(area, buf);
}

/// Render footer content directly from `FooterProps`.
///
/// This is intentionally not part of the width-based collapse/fallback logic.
/// Transient instructional states (shortcut overlay, Esc hint, quit reminder)
/// prioritize "what to do next" instructions and currently suppress the
/// collaboration mode label entirely. When collapse logic has already chosen a
/// specific single line, prefer `render_footer_line`.
pub(crate) fn render_footer_from_props(
    area: Rect,
    buf: &mut Buffer,
    props: FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) {
    Paragraph::new(prefix_lines(
        footer_from_props_lines(
            props,
            collaboration_mode_indicator,
            show_cycle_hint,
            show_shortcuts_hint,
            show_queue_hint,
        ),
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    ))
    .render(area, buf);
}

pub(crate) fn left_fits(area: Rect, left_width: u16) -> bool {
    let max_width = area.width.saturating_sub(FOOTER_INDENT_COLS as u16);
    left_width <= max_width
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SummaryHintKind {
    None,
    Shortcuts,
    QueueMessage,
    QueueShort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LeftSideState {
    hint: SummaryHintKind,
    show_cycle_hint: bool,
}

fn left_side_line(
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    state: LeftSideState,
) -> Line<'static> {
    let mut line = Line::from("");
    match state.hint {
        SummaryHintKind::None => {}
        SummaryHintKind::Shortcuts => {
            line.push_span(key_hint::plain(KeyCode::Char('?')));
            line.push_span(" for shortcuts".dim());
        }
        SummaryHintKind::QueueMessage => {
            line.push_span(key_hint::plain(KeyCode::Tab));
            line.push_span(" to queue message".dim());
        }
        SummaryHintKind::QueueShort => {
            line.push_span(key_hint::plain(KeyCode::Tab));
            line.push_span(" to queue".dim());
        }
    };

    if let Some(collaboration_mode_indicator) = collaboration_mode_indicator {
        if !matches!(state.hint, SummaryHintKind::None) {
            line.push_span(" · ".dim());
        }
        line.push_span(collaboration_mode_indicator.styled_span(state.show_cycle_hint));
    }

    line
}

pub(crate) enum SummaryLeft {
    Default,
    Custom(Line<'static>),
    None,
}

/// A candidate left-side footer layout, ordered by desirability.
///
/// The layout engine builds a prioritised list of these candidates and picks
/// the first one that fits the available width.  Each candidate can request
/// that the right-side context indicator be shown, hidden, or conditionally
/// hidden.
#[derive(Clone, Copy, Debug)]
struct FooterCandidate {
    state: LeftSideState,
    /// Whether the right-side context indicator should be shown alongside
    /// this candidate. `true` = show context, `false` = hide context.
    show_context: bool,
    /// When true, the context indicator is suppressed because the cycle hint
    /// is applicable but was dropped in this candidate; we do not want the
    /// right side to outlive the left-side "(shift+tab to cycle)".
    context_blocked_by_missing_cycle: bool,
}

/// Compute the single-line footer layout and whether the right-side context
/// indicator can be shown alongside it.
///
/// This function builds a prioritised list of `FooterCandidate`s (most
/// desirable first) and picks the first one whose left-side content fits the
/// terminal width—optionally alongside the right-side context indicator.
/// The explicit candidate list replaces the former hand-rolled if/else cascade
/// while preserving the same layout rules:
///
/// 1. Start with the fullest left-side hint plus the right-side context.
/// 2. When the queue hint is active, prefer keeping the queue hint visible,
///    even if it means dropping the right-side context earlier; the queue
///    hint may also be shortened before it is removed.
/// 3. When the queue hint is not active but the mode cycle hint is applicable,
///    drop "? for shortcuts" before dropping "(shift+tab to cycle)".
/// 4. If "(shift+tab to cycle)" cannot fit, also hide the right-side context
///    to avoid too many state transitions in quick succession.
/// 5. Finally, try a mode-only line (with and without context), and fall back
///    to no left-side footer if nothing can fit.
pub(crate) fn single_line_footer_layout(
    area: Rect,
    context_width: u16,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) -> (SummaryLeft, bool) {
    let hint_kind = if show_queue_hint {
        SummaryHintKind::QueueMessage
    } else if show_shortcuts_hint {
        SummaryHintKind::Shortcuts
    } else {
        SummaryHintKind::None
    };
    let default_state = LeftSideState {
        hint: hint_kind,
        show_cycle_hint,
    };

    // When the mode cycle hint is applicable (idle, non-queue mode), only show
    // the right-side context indicator if the "(shift+tab to cycle)" variant
    // can also fit.
    let context_requires_cycle_hint = show_cycle_hint && !show_queue_hint;

    // ── Build candidate list (most desirable first) ─────────────

    let mut candidates: Vec<FooterCandidate> = Vec::with_capacity(12);

    if show_queue_hint {
        // Queue mode: prefer keeping the queue hint; drop context before queue.
        let queue_states = [
            LeftSideState { hint: SummaryHintKind::QueueMessage, show_cycle_hint },
            LeftSideState { hint: SummaryHintKind::QueueMessage, show_cycle_hint: false },
            LeftSideState { hint: SummaryHintKind::QueueShort, show_cycle_hint: false },
        ];
        for state in queue_states {
            candidates.push(FooterCandidate { state, show_context: true, context_blocked_by_missing_cycle: false });
            candidates.push(FooterCandidate { state, show_context: false, context_blocked_by_missing_cycle: false });
        }
    } else if collaboration_mode_indicator.is_some() {
        // Non-queue mode with collaboration: try full → drop shortcut hint
        // → drop cycle hint → mode-only.
        candidates.push(FooterCandidate { state: default_state, show_context: true, context_blocked_by_missing_cycle: false });

        if show_cycle_hint {
            let cycle_state = LeftSideState { hint: SummaryHintKind::None, show_cycle_hint: true };
            candidates.push(FooterCandidate { state: cycle_state, show_context: true, context_blocked_by_missing_cycle: false });
            candidates.push(FooterCandidate { state: cycle_state, show_context: false, context_blocked_by_missing_cycle: false });
        }

        let mode_only = LeftSideState { hint: SummaryHintKind::None, show_cycle_hint: false };
        if !context_requires_cycle_hint {
            candidates.push(FooterCandidate { state: mode_only, show_context: true, context_blocked_by_missing_cycle: false });
        }
        candidates.push(FooterCandidate { state: mode_only, show_context: false, context_blocked_by_missing_cycle: context_requires_cycle_hint });
    } else {
        // No collaboration mode, simple case.
        candidates.push(FooterCandidate { state: default_state, show_context: true, context_blocked_by_missing_cycle: false });
    }

    // Final fallback: mode label only (covers queue mode where all queue
    // variants were too wide for even a bare left side).
    if let Some(_) = collaboration_mode_indicator {
        let mode_only = LeftSideState { hint: SummaryHintKind::None, show_cycle_hint: false };
        if !context_requires_cycle_hint {
            candidates.push(FooterCandidate { state: mode_only, show_context: true, context_blocked_by_missing_cycle: false });
        }
        candidates.push(FooterCandidate { state: mode_only, show_context: false, context_blocked_by_missing_cycle: false });
    }

    // ── Pick the best candidate that fits ───────────────────────

    // Deduplicate adjacent identical candidates.
    candidates.dedup_by(|a, b| a.state == b.state && a.show_context == b.show_context);

    for candidate in &candidates {
        let line = left_side_line(collaboration_mode_indicator, candidate.state);
        let width = line.width() as u16;
        if width == 0 {
            continue;
        }

        let fits_with_context = candidate.show_context
            && !candidate.context_blocked_by_missing_cycle
            && can_show_left_with_context(area, width, context_width);
        let fits_without_context = left_fits(area, width);

        if candidate.show_context && fits_with_context {
            let left = if candidate.state == default_state {
                SummaryLeft::Default
            } else {
                SummaryLeft::Custom(line)
            };
            return (left, true);
        }

        if !candidate.show_context && fits_without_context {
            let left = if candidate.state == default_state {
                SummaryLeft::Default
            } else {
                SummaryLeft::Custom(line)
            };
            return (left, false);
        }
    }

    (SummaryLeft::None, true)
}

fn right_aligned_x(area: Rect, content_width: u16) -> Option<u16> {
    if area.is_empty() {
        return None;
    }

    let right_padding = FOOTER_INDENT_COLS as u16;
    let max_width = area.width.saturating_sub(right_padding);
    if content_width == 0 || max_width == 0 {
        return None;
    }

    if content_width >= max_width {
        return Some(area.x.saturating_add(right_padding));
    }

    Some(
        area.x
            .saturating_add(area.width)
            .saturating_sub(content_width)
            .saturating_sub(right_padding),
    )
}

pub(crate) fn can_show_left_with_context(area: Rect, left_width: u16, context_width: u16) -> bool {
    let Some(context_x) = right_aligned_x(area, context_width) else {
        return true;
    };
    if left_width == 0 {
        return true;
    }
    let left_extent = FOOTER_INDENT_COLS as u16 + left_width + FOOTER_CONTEXT_GAP_COLS;
    left_extent <= context_x.saturating_sub(area.x)
}

pub(crate) fn render_context_right(area: Rect, buf: &mut Buffer, line: &Line<'static>) {
    if area.is_empty() {
        return;
    }

    let context_width = line.width() as u16;
    let Some(mut x) = right_aligned_x(area, context_width) else {
        return;
    };
    let y = area.y + area.height.saturating_sub(1);
    let max_x = area.x.saturating_add(area.width);

    for span in &line.spans {
        if x >= max_x {
            break;
        }
        let span_width = span.width() as u16;
        if span_width == 0 {
            continue;
        }
        let remaining = max_x.saturating_sub(x);
        let draw_width = span_width.min(remaining);
        buf.set_span(x, y, span, draw_width);
        x = x.saturating_add(span_width);
    }
}

pub(crate) fn inset_footer_hint_area(mut area: Rect) -> Rect {
    if area.width > 2 {
        area.x += 2;
        area.width = area.width.saturating_sub(2);
    }
    area
}

pub(crate) fn render_footer_hint_items(area: Rect, buf: &mut Buffer, items: &[(String, String)]) {
    if items.is_empty() {
        return;
    }

    footer_hint_items_line(items).render(inset_footer_hint_area(area), buf);
}

/// Map `FooterProps` to footer lines without width-based collapse.
///
/// This is the canonical FooterMode-to-text mapping. It powers transient,
/// instructional states (shortcut overlay, Esc hint, quit reminder) and also
/// the default rendering for base states when collapse is not applied (or when
/// `single_line_footer_layout` returns `SummaryLeft::Default`). Collapse and
/// fallback decisions live in `single_line_footer_layout`; this function only
/// formats the chosen/default content.
fn footer_from_props_lines(
    props: FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) -> Vec<Line<'static>> {
    match props.mode {
        FooterMode::QuitShortcutReminder => {
            vec![quit_shortcut_reminder_line(props.quit_shortcut_key)]
        }
        FooterMode::ComposerEmpty => {
            let state = LeftSideState {
                hint: if show_shortcuts_hint {
                    SummaryHintKind::Shortcuts
                } else {
                    SummaryHintKind::None
                },
                show_cycle_hint,
            };
            vec![left_side_line(collaboration_mode_indicator, state)]
        }
        FooterMode::ShortcutOverlay => {
            let state = ShortcutsState {
                use_shift_enter_hint: props.use_shift_enter_hint,
                esc_backtrack_hint: props.esc_backtrack_hint,
                is_wsl: props.is_wsl,
                collaboration_modes_enabled: props.collaboration_modes_enabled,
            };
            shortcut_overlay_lines(state)
        }
        FooterMode::EscHint => vec![esc_hint_line(props.esc_backtrack_hint)],
        FooterMode::ComposerHasDraft => {
            let state = LeftSideState {
                hint: if show_queue_hint {
                    SummaryHintKind::QueueMessage
                } else {
                    SummaryHintKind::None
                },
                show_cycle_hint,
            };
            vec![left_side_line(collaboration_mode_indicator, state)]
        }
    }
}

pub(crate) fn footer_line_width(
    props: FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) -> u16 {
    footer_from_props_lines(
        props,
        collaboration_mode_indicator,
        show_cycle_hint,
        show_shortcuts_hint,
        show_queue_hint,
    )
    .last()
    .map(|line| line.width() as u16)
    .unwrap_or(0)
}

pub(crate) fn footer_hint_items_width(items: &[(String, String)]) -> u16 {
    if items.is_empty() {
        return 0;
    }
    footer_hint_items_line(items).width() as u16
}

fn footer_hint_items_line(items: &[(String, String)]) -> Line<'static> {
    let mut spans = Vec::with_capacity(items.len() * 4);
    for (idx, (key, label)) in items.iter().enumerate() {
        spans.push(" ".into());
        spans.push(key.clone().bold());
        spans.push(format!(" {label}").into());
        if idx + 1 != items.len() {
            spans.push("   ".into());
        }
    }
    Line::from(spans)
}

#[derive(Clone, Copy, Debug)]
struct ShortcutsState {
    use_shift_enter_hint: bool,
    esc_backtrack_hint: bool,
    is_wsl: bool,
    collaboration_modes_enabled: bool,
}

fn quit_shortcut_reminder_line(key: KeyBinding) -> Line<'static> {
    Line::from(vec![key.into(), " again to quit".into()]).dim()
}

fn esc_hint_line(esc_backtrack_hint: bool) -> Line<'static> {
    let esc = key_hint::plain(KeyCode::Esc);
    if esc_backtrack_hint {
        Line::from(vec![esc.into(), " again to edit previous message".into()]).dim()
    } else {
        Line::from(vec![
            esc.into(),
            " ".into(),
            esc.into(),
            " to edit previous message".into(),
        ])
        .dim()
    }
}

fn shortcut_overlay_lines(state: ShortcutsState) -> Vec<Line<'static>> {
    let mut commands = Line::from("");
    let mut shell_commands = Line::from("");
    let mut newline = Line::from("");
    let mut queue_message_tab = Line::from("");
    let mut file_paths = Line::from("");
    let mut paste_image = Line::from("");
    let mut external_editor = Line::from("");
    let mut edit_previous = Line::from("");
    let mut quit = Line::from("");
    let mut show_transcript = Line::from("");
    let mut change_mode = Line::from("");

    for descriptor in SHORTCUTS {
        if let Some(text) = descriptor.overlay_entry(state) {
            match descriptor.id {
                ShortcutId::Commands => commands = text,
                ShortcutId::ShellCommands => shell_commands = text,
                ShortcutId::InsertNewline => newline = text,
                ShortcutId::QueueMessageTab => queue_message_tab = text,
                ShortcutId::FilePaths => file_paths = text,
                ShortcutId::PasteImage => paste_image = text,
                ShortcutId::ExternalEditor => external_editor = text,
                ShortcutId::EditPrevious => edit_previous = text,
                ShortcutId::Quit => quit = text,
                ShortcutId::ShowTranscript => show_transcript = text,
                ShortcutId::ChangeMode => change_mode = text,
            }
        }
    }

    let mut ordered = vec![
        commands,
        shell_commands,
        newline,
        queue_message_tab,
        file_paths,
        paste_image,
        external_editor,
        edit_previous,
        quit,
    ];
    if change_mode.width() > 0 {
        ordered.push(change_mode);
    }
    ordered.push(Line::from(""));
    ordered.push(show_transcript);

    build_columns(ordered)
}

fn build_columns(entries: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if entries.is_empty() {
        return Vec::new();
    }

    const COLUMNS: usize = 2;
    const COLUMN_PADDING: [usize; COLUMNS] = [4, 4];
    const COLUMN_GAP: usize = 4;

    let rows = entries.len().div_ceil(COLUMNS);
    let target_len = rows * COLUMNS;
    let mut entries = entries;
    if entries.len() < target_len {
        entries.extend(std::iter::repeat_n(
            Line::from(""),
            target_len - entries.len(),
        ));
    }

    let mut column_widths = [0usize; COLUMNS];

    for (idx, entry) in entries.iter().enumerate() {
        let column = idx % COLUMNS;
        column_widths[column] = column_widths[column].max(entry.width());
    }

    for (idx, width) in column_widths.iter_mut().enumerate() {
        *width += COLUMN_PADDING[idx];
    }

    entries
        .chunks(COLUMNS)
        .map(|chunk| {
            let mut line = Line::from("");
            for (col, entry) in chunk.iter().enumerate() {
                line.extend(entry.spans.clone());
                if col < COLUMNS - 1 {
                    let target_width = column_widths[col];
                    let padding = target_width.saturating_sub(entry.width()) + COLUMN_GAP;
                    line.push_span(Span::from(" ".repeat(padding)));
                }
            }
            line.dim()
        })
        .collect()
}

/// Build a compact progress bar for the context window usage.
///
/// Example output: `[████████░░] 78%`
///
/// Colors: green (<50%), yellow (50-80%), red (>80%).
fn context_window_bar(percent_used: i64) -> Vec<Span<'static>> {
    let pct = percent_used.clamp(0, 100) as usize;
    let bar_width = 10usize;
    let filled = (pct * bar_width + 50) / 100; // round
    let empty = bar_width.saturating_sub(filled);

    let bar_color = if pct < 50 {
        ratatui::style::Color::Green
    } else if pct <= 80 {
        ratatui::style::Color::Yellow
    } else {
        ratatui::style::Color::Red
    };

    let filled_str: String = "█".repeat(filled);
    let empty_str: String = "░".repeat(empty);

    vec![
        Span::from("[").dim(),
        Span::styled(filled_str, ratatui::style::Style::default().fg(bar_color)),
        Span::from(empty_str).dim(),
        Span::from("] ").dim(),
        Span::styled(
            format!("{pct}%"),
            ratatui::style::Style::default().fg(bar_color),
        ),
    ]
}

pub(crate) fn context_window_line(
    percent: Option<i64>,
    used_tokens: Option<i64>,
    model_display: &str,
    provider_display: &str,
    cwd_display: &str,
) -> Line<'static> {
    let context_spans: Vec<Span<'static>> = if let Some(percent) = percent {
        let percent_used = (100 - percent).clamp(0, 100);
        context_window_bar(percent_used)
    } else if let Some(tokens) = used_tokens {
        let used_fmt = format_tokens_compact(tokens);
        vec![Span::from(format!("{used_fmt} used")).dim()]
    } else {
        context_window_bar(0)
    };

    if model_display.is_empty() {
        return Line::from(context_spans);
    }

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(8);
    spans.push(Span::from(model_display.to_string()).cyan().bold());
    if !provider_display.is_empty() {
        spans.push(Span::from(" · ").dim());
        spans.push(Span::from(provider_display.to_string()).dim());
    }
    if !cwd_display.is_empty() {
        spans.push(Span::from(" · ").dim());
        spans.push(Span::from(cwd_display.to_string()).dim());
    }
    spans.push(Span::from("  ").dim());
    spans.extend(context_spans);
    Line::from(spans)
}

/// Render the model/provider status line (opencode-style, left-aligned below textarea).
///
/// Layout: `model  provider`
/// This is shown on its own line in the footer area, styled to match the opencode
/// prompt component's status bar.
pub(crate) fn render_model_status_line(area: Rect, buf: &mut Buffer, model: &str, provider: &str) {
    if area.is_empty() || area.height == 0 {
        return;
    }
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(4);
    if !model.is_empty() {
        spans.push(Span::from(model.to_string()));
    }
    if !provider.is_empty() {
        spans.push(Span::from(" ").dim());
        spans.push(Span::from(provider.to_string()).dim());
    }
    if spans.is_empty() {
        return;
    }
    let line = Line::from(spans);
    let indented = prefix_lines(
        vec![line],
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    );
    Paragraph::new(indented).render(area, buf);
}

/// Render the shortcut hints line (opencode-style, left-aligned below model status).
///
/// Shows: `tab agents  ctrl+p commands`
/// When a task is running, shows interrupt hint instead.
pub(crate) fn render_shortcut_hints_line(
    area: Rect,
    buf: &mut Buffer,
    is_task_running: bool,
    interrupt_highlight: bool,
) {
    if area.is_empty() || area.height == 0 {
        return;
    }
    let line = if is_task_running {
        if interrupt_highlight {
            Line::from(vec![
                Span::from("esc ").bold(),
                Span::from("again to interrupt").dim(),
            ])
        } else {
            Line::from(vec![
                Span::from("esc ").bold(),
                Span::from("interrupt").dim(),
            ])
        }
    } else {
        let tab_span: Span<'static> = key_hint::plain(KeyCode::Tab).into();
        let slash_span: Span<'static> = key_hint::plain(KeyCode::Char('/')).into();
        Line::from(vec![
            tab_span,
            Span::from(" agents").dim(),
            Span::from("  "),
            slash_span,
            Span::from(" commands").dim(),
        ])
    };
    let indented = prefix_lines(
        vec![line],
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    );
    Paragraph::new(indented).render(area, buf);
}

/// Render the path info line at the very bottom of the session footer.
///
/// Shows the current working directory with the last component bold.
pub(crate) fn render_path_info_line(area: Rect, buf: &mut Buffer, cwd: &str) {
    if area.is_empty() || area.height == 0 || cwd.is_empty() {
        return;
    }
    let sep = if cwd.contains('\\') { "\\" } else { "/" };
    let line = if let Some(pos) = cwd.rfind(sep) {
        let parent = &cwd[..=pos];
        let name = &cwd[pos + 1..];
        Line::from(vec![
            Span::from(parent.to_string()).dim(),
            Span::from(name.to_string()).bold(),
        ])
    } else {
        Line::from(vec![Span::from(cwd.to_string()).dim()])
    };
    let indented = prefix_lines(
        vec![line],
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    );
    Paragraph::new(indented).render(area, buf);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShortcutId {
    Commands,
    ShellCommands,
    InsertNewline,
    QueueMessageTab,
    FilePaths,
    PasteImage,
    ExternalEditor,
    EditPrevious,
    Quit,
    ShowTranscript,
    ChangeMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ShortcutBinding {
    key: KeyBinding,
    condition: DisplayCondition,
}

impl ShortcutBinding {
    fn matches(&self, state: ShortcutsState) -> bool {
        self.condition.matches(state)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayCondition {
    Always,
    WhenShiftEnterHint,
    WhenNotShiftEnterHint,
    WhenUnderWSL,
    WhenCollaborationModesEnabled,
}

impl DisplayCondition {
    fn matches(self, state: ShortcutsState) -> bool {
        match self {
            DisplayCondition::Always => true,
            DisplayCondition::WhenShiftEnterHint => state.use_shift_enter_hint,
            DisplayCondition::WhenNotShiftEnterHint => !state.use_shift_enter_hint,
            DisplayCondition::WhenUnderWSL => state.is_wsl,
            DisplayCondition::WhenCollaborationModesEnabled => state.collaboration_modes_enabled,
        }
    }
}

struct ShortcutDescriptor {
    id: ShortcutId,
    bindings: &'static [ShortcutBinding],
    prefix: &'static str,
    label: &'static str,
}

impl ShortcutDescriptor {
    fn binding_for(&self, state: ShortcutsState) -> Option<&'static ShortcutBinding> {
        self.bindings.iter().find(|binding| binding.matches(state))
    }

    fn overlay_entry(&self, state: ShortcutsState) -> Option<Line<'static>> {
        let binding = self.binding_for(state)?;
        let mut line = Line::from(vec![self.prefix.into(), binding.key.into()]);
        match self.id {
            ShortcutId::EditPrevious => {
                if state.esc_backtrack_hint {
                    line.push_span(" again to edit previous message");
                } else {
                    line.extend(vec![
                        " ".into(),
                        key_hint::plain(KeyCode::Esc).into(),
                        " to edit previous message".into(),
                    ]);
                }
            }
            _ => line.push_span(self.label),
        };
        Some(line)
    }
}

const SHORTCUTS: &[ShortcutDescriptor] = &[
    ShortcutDescriptor {
        id: ShortcutId::Commands,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('/')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for commands",
    },
    ShortcutDescriptor {
        id: ShortcutId::ShellCommands,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('!')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for shell commands",
    },
    ShortcutDescriptor {
        id: ShortcutId::InsertNewline,
        bindings: &[
            ShortcutBinding {
                key: key_hint::shift(KeyCode::Enter),
                condition: DisplayCondition::WhenShiftEnterHint,
            },
            ShortcutBinding {
                key: key_hint::ctrl(KeyCode::Char('j')),
                condition: DisplayCondition::WhenNotShiftEnterHint,
            },
        ],
        prefix: "",
        label: " for newline",
    },
    ShortcutDescriptor {
        id: ShortcutId::QueueMessageTab,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Tab),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to queue message",
    },
    ShortcutDescriptor {
        id: ShortcutId::FilePaths,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('@')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for file paths",
    },
    ShortcutDescriptor {
        id: ShortcutId::PasteImage,
        // Show Ctrl+Alt+V when running under WSL (terminals often intercept plain
        // Ctrl+V); otherwise fall back to Ctrl+V.
        bindings: &[
            ShortcutBinding {
                key: key_hint::ctrl_alt(KeyCode::Char('v')),
                condition: DisplayCondition::WhenUnderWSL,
            },
            ShortcutBinding {
                key: key_hint::ctrl(KeyCode::Char('v')),
                condition: DisplayCondition::Always,
            },
        ],
        prefix: "",
        label: " to paste images",
    },
    ShortcutDescriptor {
        id: ShortcutId::ExternalEditor,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('g')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to edit in external editor",
    },
    ShortcutDescriptor {
        id: ShortcutId::EditPrevious,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Esc),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: "",
    },
    ShortcutDescriptor {
        id: ShortcutId::Quit,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('c')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to exit",
    },
    ShortcutDescriptor {
        id: ShortcutId::ShowTranscript,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('t')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to view transcript",
    },
    ShortcutDescriptor {
        id: ShortcutId::ChangeMode,
        bindings: &[ShortcutBinding {
            key: key_hint::shift(KeyCode::Tab),
            condition: DisplayCondition::WhenCollaborationModesEnabled,
        }],
        prefix: "",
        label: " to change mode",
    },
];

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn snapshot_footer(name: &str, props: FooterProps) {
        snapshot_footer_with_mode_indicator(name, 80, props, None);
    }

    fn snapshot_footer_with_mode_indicator(
        name: &str,
        width: u16,
        props: FooterProps,
        collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    ) {
        let height = footer_height(props.clone()).max(1);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, f.area().width, height);
                let context_line = context_window_line(
                    props.context_window_percent,
                    props.context_window_used_tokens,
                    &props.model_display,
                    &props.provider_display,
                    &props.cwd_display,
                );
                let context_width = context_line.width() as u16;
                let show_cycle_hint = !props.is_task_running;
                let show_shortcuts_hint = match props.mode {
                    FooterMode::ComposerEmpty => true,
                    FooterMode::QuitShortcutReminder
                    | FooterMode::ShortcutOverlay
                    | FooterMode::EscHint
                    | FooterMode::ComposerHasDraft => false,
                };
                let show_queue_hint = match props.mode {
                    FooterMode::ComposerHasDraft => props.is_task_running && props.steer_enabled,
                    FooterMode::QuitShortcutReminder
                    | FooterMode::ComposerEmpty
                    | FooterMode::ShortcutOverlay
                    | FooterMode::EscHint => false,
                };
                let left_width = footer_line_width(
                    props.clone(),
                    collaboration_mode_indicator,
                    show_cycle_hint,
                    show_shortcuts_hint,
                    show_queue_hint,
                );
                let can_show_left_and_context =
                    can_show_left_with_context(area, left_width, context_width);
                if matches!(
                    props.mode,
                    FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
                ) {
                    let (summary_left, show_context) = single_line_footer_layout(
                        area,
                        context_width,
                        collaboration_mode_indicator,
                        show_cycle_hint,
                        show_shortcuts_hint,
                        show_queue_hint,
                    );
                    match summary_left {
                        SummaryLeft::Default => {
                            render_footer_from_props(
                                area,
                                f.buffer_mut(),
                                props.clone(),
                                collaboration_mode_indicator,
                                show_cycle_hint,
                                show_shortcuts_hint,
                                show_queue_hint,
                            );
                        }
                        SummaryLeft::Custom(line) => {
                            render_footer_line(area, f.buffer_mut(), line);
                        }
                        SummaryLeft::None => {}
                    }
                    if show_context {
                        render_context_right(area, f.buffer_mut(), &context_line);
                    }
                } else {
                    render_footer_from_props(
                        area,
                        f.buffer_mut(),
                        props.clone(),
                        collaboration_mode_indicator,
                        show_cycle_hint,
                        show_shortcuts_hint,
                        show_queue_hint,
                    );
                    let show_context = can_show_left_and_context
                        && !matches!(
                            props.mode,
                            FooterMode::EscHint
                                | FooterMode::QuitShortcutReminder
                                | FooterMode::ShortcutOverlay
                        );
                    if show_context {
                        render_context_right(area, f.buffer_mut(), &context_line);
                    }
                }
            })
            .unwrap();
        assert_snapshot!(name, terminal.backend());
    }

    #[test]
    fn footer_snapshots() {
        snapshot_footer(
            "footer_shortcuts_default",
            FooterProps {
                mode: FooterMode::ComposerEmpty,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                steer_enabled: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                model_display: String::new(),
                provider_display: String::new(),
                cwd_display: String::new(),
                is_session_mode: false,
            },
        );

        snapshot_footer(
            "footer_shortcuts_shift_and_esc",
            FooterProps {
                mode: FooterMode::ShortcutOverlay,
                esc_backtrack_hint: true,
                use_shift_enter_hint: true,
                is_task_running: false,
                steer_enabled: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                model_display: String::new(),
                provider_display: String::new(),
                cwd_display: String::new(),
                is_session_mode: false,
            },
        );

        snapshot_footer(
            "footer_shortcuts_collaboration_modes_enabled",
            FooterProps {
                mode: FooterMode::ShortcutOverlay,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                steer_enabled: false,
                collaboration_modes_enabled: true,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                model_display: String::new(),
                provider_display: String::new(),
                cwd_display: String::new(),
                is_session_mode: false,
            },
        );

        snapshot_footer(
            "footer_ctrl_c_quit_idle",
            FooterProps {
                mode: FooterMode::QuitShortcutReminder,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                steer_enabled: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                model_display: String::new(),
                provider_display: String::new(),
                cwd_display: String::new(),
                is_session_mode: false,
            },
        );

        snapshot_footer(
            "footer_ctrl_c_quit_running",
            FooterProps {
                mode: FooterMode::QuitShortcutReminder,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                steer_enabled: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                model_display: String::new(),
                provider_display: String::new(),
                cwd_display: String::new(),
                is_session_mode: false,
            },
        );

        snapshot_footer(
            "footer_esc_hint_idle",
            FooterProps {
                mode: FooterMode::EscHint,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                steer_enabled: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                model_display: String::new(),
                provider_display: String::new(),
                cwd_display: String::new(),
                is_session_mode: false,
            },
        );

        snapshot_footer(
            "footer_esc_hint_primed",
            FooterProps {
                mode: FooterMode::EscHint,
                esc_backtrack_hint: true,
                use_shift_enter_hint: false,
                is_task_running: false,
                steer_enabled: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                model_display: String::new(),
                provider_display: String::new(),
                cwd_display: String::new(),
                is_session_mode: false,
            },
        );

        snapshot_footer(
            "footer_shortcuts_context_running",
            FooterProps {
                mode: FooterMode::ComposerEmpty,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                steer_enabled: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: Some(72),
                context_window_used_tokens: None,
                model_display: String::new(),
                provider_display: String::new(),
                cwd_display: String::new(),
                is_session_mode: false,
            },
        );

        snapshot_footer(
            "footer_context_tokens_used",
            FooterProps {
                mode: FooterMode::ComposerEmpty,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: false,
                steer_enabled: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: Some(123_456),
                model_display: String::new(),
                provider_display: String::new(),
                cwd_display: String::new(),
                is_session_mode: false,
            },
        );

        snapshot_footer(
            "footer_composer_has_draft_queue_hint_disabled",
            FooterProps {
                mode: FooterMode::ComposerHasDraft,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                steer_enabled: false,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                model_display: String::new(),
                provider_display: String::new(),
                cwd_display: String::new(),
                is_session_mode: false,
            },
        );

        snapshot_footer(
            "footer_composer_has_draft_queue_hint_enabled",
            FooterProps {
                mode: FooterMode::ComposerHasDraft,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                steer_enabled: true,
                collaboration_modes_enabled: false,
                is_wsl: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
                model_display: String::new(),
                provider_display: String::new(),
                cwd_display: String::new(),
                is_session_mode: false,
            },
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            steer_enabled: false,
            collaboration_modes_enabled: true,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: None,
            context_window_used_tokens: None,
            model_display: String::new(),
            provider_display: String::new(),
            cwd_display: String::new(),
            is_session_mode: false,
        };

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_wide",
            120,
            props.clone(),
            Some(CollaborationModeIndicator::Plan),
        );

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_narrow_overlap_hides",
            50,
            props,
            Some(CollaborationModeIndicator::Plan),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: true,
            steer_enabled: false,
            collaboration_modes_enabled: true,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: None,
            context_window_used_tokens: None,
            model_display: String::new(),
            provider_display: String::new(),
            cwd_display: String::new(),
            is_session_mode: false,
        };

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_running_hides_hint",
            120,
            props,
            Some(CollaborationModeIndicator::Plan),
        );
    }

    #[test]
    fn paste_image_shortcut_prefers_ctrl_alt_v_under_wsl() {
        let descriptor = SHORTCUTS
            .iter()
            .find(|descriptor| descriptor.id == ShortcutId::PasteImage)
            .expect("paste image shortcut");

        let is_wsl = {
            #[cfg(target_os = "linux")]
            {
                crate::clipboard_paste::is_probably_wsl()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        };

        let expected_key = if is_wsl {
            key_hint::ctrl_alt(KeyCode::Char('v'))
        } else {
            key_hint::ctrl(KeyCode::Char('v'))
        };

        let actual_key = descriptor
            .binding_for(ShortcutsState {
                use_shift_enter_hint: false,
                esc_backtrack_hint: false,
                is_wsl,
                collaboration_modes_enabled: false,
            })
            .expect("shortcut binding")
            .key;

        assert_eq!(actual_key, expected_key);
    }
}
