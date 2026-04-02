use std::cell::RefCell;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, StatefulWidgetRef, Widget};
use savfox_core::protocol::SessionSource;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::popup_consts::standard_popup_hint_line;
use super::textarea::{TextArea, TextAreaState};
use crate::app_event::{AppEvent, FeedbackCategory};
use crate::app_event_sender::AppEventSender;
use crate::history_cell;
use crate::render::renderable::Renderable;

const BASE_BUG_ISSUE_URL: &str =
    "https://github.com/openai/savfox/issues/new?template=2-bug-report.yml";
/// Internal routing link for employee feedback follow-ups. This must not be shown to external
/// users.
const SAVFOX_FEEDBACK_INTERNAL_URL: &str = "http://go/savfox-feedback-internal";

/// The target audience for feedback follow-up instructions.
///
/// This is used strictly for messaging/links after feedback upload completes. It
/// must not change feedback upload behavior itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FeedbackAudience {
    OpenAiEmployee,
    External,
}

/// Minimal input overlay to collect an optional feedback note, then upload
/// both logs and rollout with classification + metadata.
pub(crate) struct FeedbackNoteView {
    category: FeedbackCategory,
    snapshot: savfox_feedback::SavfoxLogSnapshot,
    rollout_path: Option<PathBuf>,
    app_event_tx: AppEventSender,
    include_logs: bool,
    feedback_audience: FeedbackAudience,

    // UI state
    textarea: TextArea,
    textarea_state: RefCell<TextAreaState>,
    complete: bool,
}

impl FeedbackNoteView {
    pub(crate) fn new(
        category: FeedbackCategory,
        snapshot: savfox_feedback::SavfoxLogSnapshot,
        rollout_path: Option<PathBuf>,
        app_event_tx: AppEventSender,
        include_logs: bool,
        feedback_audience: FeedbackAudience,
    ) -> Self {
        Self {
            category,
            snapshot,
            rollout_path,
            app_event_tx,
            include_logs,
            feedback_audience,
            textarea: TextArea::new(),
            textarea_state: RefCell::new(TextAreaState::default()),
            complete: false,
        }
    }

    fn submit(&mut self) {
        let note = self.textarea.text().trim().to_owned();
        let reason_opt = if note.is_empty() {
            None
        } else {
            Some(note.as_str())
        };
        let rollout_path_ref = self.rollout_path.as_deref();
        let classification = feedback_classification(self.category);

        let mut session_id = self.snapshot.session_id.clone();

        let result = self.snapshot.upload_feedback(
            classification,
            reason_opt,
            self.include_logs,
            if self.include_logs {
                rollout_path_ref
            } else {
                None
            },
            Some(SessionSource::Cli),
        );

        match result {
            Ok(()) => {
                let prefix = if self.include_logs {
                    "• Feedback uploaded."
                } else {
                    "• Feedback recorded (no logs)."
                };
                let issue_url =
                    issue_url_for_category(self.category, &session_id, self.feedback_audience);
                let mut lines = vec![Line::from(match issue_url.as_ref() {
                    Some(_) if self.feedback_audience == FeedbackAudience::OpenAiEmployee => {
                        format!("{prefix} Please report this in #savfox-feedback:")
                    }
                    Some(_) => format!("{prefix} Please open an issue using the following URL:"),
                    None => format!("{prefix} Thanks for the feedback!"),
                })];
                match issue_url {
                    Some(url) if self.feedback_audience == FeedbackAudience::OpenAiEmployee => {
                        lines.extend([
                            "".into(),
                            Line::from(vec!["  ".into(), url.cyan().underlined()]),
                            "".into(),
                            Line::from("  Share this and add some info about your problem:"),
                            Line::from(vec![
                                "    ".into(),
                                format!("go/savfox-feedback/{session_id}").bold(),
                            ]),
                        ]);
                    }
                    Some(url) => {
                        lines.extend([
                            "".into(),
                            Line::from(vec!["  ".into(), url.cyan().underlined()]),
                            "".into(),
                            Line::from(vec![
                                "  Or mention your session ID ".into(),
                                std::mem::take(&mut session_id).bold(),
                                " in an existing issue.".into(),
                            ]),
                        ]);
                    }
                    None => {
                        lines.extend([
                            "".into(),
                            Line::from(vec![
                                "  Session ID: ".into(),
                                std::mem::take(&mut session_id).bold(),
                            ]),
                        ]);
                    }
                }
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::PlainHistoryCell::new(lines),
                )));
            }
            Err(e) => {
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_error_event(format!("Failed to upload feedback: {e}")),
                )));
            }
        }
        self.complete = true;
    }
}

impl BottomPaneView for FeedbackNoteView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.on_ctrl_c();
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.submit();
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                self.textarea.input(key_event);
            }
            other => {
                self.textarea.input(other);
            }
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.complete = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if pasted.is_empty() {
            return false;
        }
        self.textarea.insert_str(&pasted);
        true
    }
}

impl Renderable for FeedbackNoteView {
    fn desired_height(&self, width: u16) -> u16 {
        1u16 + self.input_height(width) + 3u16
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if area.height < 2 || area.width <= 2 {
            return None;
        }
        let text_area_height = self.input_height(area.width).saturating_sub(1);
        if text_area_height == 0 {
            return None;
        }
        let top_line_count = 1u16; // title only
        let textarea_rect = Rect {
            x: area.x.saturating_add(2),
            y: area.y.saturating_add(top_line_count).saturating_add(1),
            width: area.width.saturating_sub(2),
            height: text_area_height,
        };
        let state = *self.textarea_state.borrow();
        self.textarea.cursor_pos_with_state(textarea_rect, state)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let (title, placeholder) = feedback_title_and_placeholder(self.category);
        let input_height = self.input_height(area.width);

        // Title line
        let title_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        let title_spans: Vec<Span<'static>> = vec![gutter(), title.bold()];
        Paragraph::new(Line::from(title_spans)).render(title_area, buf);

        // Input line
        let input_area = Rect {
            x: area.x,
            y: area.y.saturating_add(1),
            width: area.width,
            height: input_height,
        };
        if input_area.width >= 2 {
            for row in 0..input_area.height {
                Paragraph::new(Line::from(vec![gutter()])).render(
                    Rect {
                        x: input_area.x,
                        y: input_area.y.saturating_add(row),
                        width: 2,
                        height: 1,
                    },
                    buf,
                );
            }

            let text_area_height = input_area.height.saturating_sub(1);
            if text_area_height > 0 {
                if input_area.width > 2 {
                    let blank_rect = Rect {
                        x: input_area.x.saturating_add(2),
                        y: input_area.y,
                        width: input_area.width.saturating_sub(2),
                        height: 1,
                    };
                    Clear.render(blank_rect, buf);
                }
                let textarea_rect = Rect {
                    x: input_area.x.saturating_add(2),
                    y: input_area.y.saturating_add(1),
                    width: input_area.width.saturating_sub(2),
                    height: text_area_height,
                };
                let mut state = self.textarea_state.borrow_mut();
                StatefulWidgetRef::render_ref(&(&self.textarea), textarea_rect, buf, &mut state);
                if self.textarea.text().is_empty() {
                    Paragraph::new(Line::from(placeholder.dim())).render(textarea_rect, buf);
                }
            }
        }

        let hint_blank_y = input_area.y.saturating_add(input_height);
        if hint_blank_y < area.y.saturating_add(area.height) {
            let blank_area = Rect {
                x: area.x,
                y: hint_blank_y,
                width: area.width,
                height: 1,
            };
            Clear.render(blank_area, buf);
        }

        let hint_y = hint_blank_y.saturating_add(1);
        if hint_y < area.y.saturating_add(area.height) {
            Paragraph::new(standard_popup_hint_line()).render(
                Rect {
                    x: area.x,
                    y: hint_y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
        }
    }
}

impl FeedbackNoteView {
    fn input_height(&self, width: u16) -> u16 {
        let usable_width = width.saturating_sub(2);
        let text_height = self.textarea.desired_height(usable_width).clamp(1, 8);
        text_height.saturating_add(1).min(9)
    }
}

fn gutter() -> Span<'static> {
    "▌ ".cyan()
}

fn feedback_title_and_placeholder(category: FeedbackCategory) -> (String, String) {
    match category {
        FeedbackCategory::BadResult => (
            "Tell us more (bad result)".to_owned(),
            "(optional) Write a short description to help us further".to_owned(),
        ),
        FeedbackCategory::GoodResult => (
            "Tell us more (good result)".to_owned(),
            "(optional) Write a short description to help us further".to_owned(),
        ),
        FeedbackCategory::Bug => (
            "Tell us more (bug)".to_owned(),
            "(optional) Write a short description to help us further".to_owned(),
        ),
        FeedbackCategory::Other => (
            "Tell us more (other)".to_owned(),
            "(optional) Write a short description to help us further".to_owned(),
        ),
    }
}

fn feedback_classification(category: FeedbackCategory) -> &'static str {
    match category {
        FeedbackCategory::BadResult => "bad_result",
        FeedbackCategory::GoodResult => "good_result",
        FeedbackCategory::Bug => "bug",
        FeedbackCategory::Other => "other",
    }
}

fn issue_url_for_category(
    category: FeedbackCategory,
    session_id: &str,
    feedback_audience: FeedbackAudience,
) -> Option<String> {
    // Only certain categories provide a follow-up link. We intentionally keep
    // the external GitHub behavior identical while routing internal users to
    // the internal go link.
    match category {
        FeedbackCategory::Bug | FeedbackCategory::BadResult | FeedbackCategory::Other => {
            Some(match feedback_audience {
                FeedbackAudience::OpenAiEmployee => slack_feedback_url(session_id),
                FeedbackAudience::External => {
                    format!("{BASE_BUG_ISSUE_URL}&steps=Uploaded%20session:%20{session_id}")
                }
            })
        }
        FeedbackCategory::GoodResult => None,
    }
}

/// Build the internal follow-up URL.
///
/// We accept a `session_id` so the call site stays symmetric with the external
/// path, but we currently point to a fixed channel without prefilling text.
fn slack_feedback_url(_session_id: &str) -> String {
    SAVFOX_FEEDBACK_INTERNAL_URL.to_owned()
}

// Build the selection popup params for feedback categories.
pub(crate) fn feedback_selection_params(
    app_event_tx: AppEventSender,
) -> super::SelectionViewParams {
    super::SelectionViewParams {
        title: Some("How was this?".to_owned()),
        items: vec![
            make_feedback_item(
                app_event_tx.clone(),
                "bug",
                "Crash, error message, hang, or broken UI/behavior.",
                FeedbackCategory::Bug,
            ),
            make_feedback_item(
                app_event_tx.clone(),
                "bad result",
                "Output was off-target, incorrect, incomplete, or unhelpful.",
                FeedbackCategory::BadResult,
            ),
            make_feedback_item(
                app_event_tx.clone(),
                "good result",
                "Helpful, correct, high‑quality, or delightful result worth celebrating.",
                FeedbackCategory::GoodResult,
            ),
            make_feedback_item(
                app_event_tx,
                "other",
                "Slowness, feature suggestion, UX feedback, or anything else.",
                FeedbackCategory::Other,
            ),
        ],
        ..Default::default()
    }
}

/// Build the selection popup params shown when feedback is disabled.
pub(crate) fn feedback_disabled_params() -> super::SelectionViewParams {
    super::SelectionViewParams {
        title: Some("Sending feedback is disabled".to_owned()),
        subtitle: Some("This action is disabled by configuration.".to_owned()),
        footer_hint: Some(standard_popup_hint_line()),
        items: vec![super::SelectionItem {
            name: "Close".to_owned(),
            dismiss_on_select: true,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn make_feedback_item(
    app_event_tx: AppEventSender,
    name: &str,
    description: &str,
    category: FeedbackCategory,
) -> super::SelectionItem {
    let action: super::SelectionAction = Box::new(move |_sender: &AppEventSender| {
        app_event_tx.send(AppEvent::OpenFeedbackConsent { category });
    });
    super::SelectionItem {
        name: name.to_owned(),
        description: Some(description.to_owned()),
        actions: vec![action],
        dismiss_on_select: true,
        ..Default::default()
    }
}

/// Build the upload consent popup params for a given feedback category.
pub(crate) fn feedback_upload_consent_params(
    app_event_tx: AppEventSender,
    category: FeedbackCategory,
    rollout_path: Option<std::path::PathBuf>,
) -> super::SelectionViewParams {
    use super::popup_consts::standard_popup_hint_line;
    let yes_action: super::SelectionAction = Box::new({
        let tx = app_event_tx.clone();
        move |sender: &AppEventSender| {
            let _ = sender;
            tx.send(AppEvent::OpenFeedbackNote {
                category,
                include_logs: true,
            });
        }
    });

    let no_action: super::SelectionAction = Box::new({
        let tx = app_event_tx;
        move |sender: &AppEventSender| {
            let _ = sender;
            tx.send(AppEvent::OpenFeedbackNote {
                category,
                include_logs: false,
            });
        }
    });

    // Build header listing files that would be sent if user consents.
    let mut header_lines: Vec<Box<dyn crate::render::renderable::Renderable>> = vec![
        Line::from("Upload logs?".bold()).into(),
        Line::from("").into(),
        Line::from("The following files will be sent:".dim()).into(),
        Line::from(vec!["  • ".into(), "savfox-logs.log".into()]).into(),
    ];
    if let Some(path) = rollout_path.as_deref()
        && let Some(name) = path.file_name().map(|s| s.to_string_lossy().to_string())
    {
        header_lines.push(Line::from(vec!["  • ".into(), name.into()]).into());
    }

    super::SelectionViewParams {
        footer_hint: Some(standard_popup_hint_line()),
        items: vec![
            super::SelectionItem {
                name: "Yes".to_owned(),
                description: Some(
                    "Share the current Savfox session logs with the team for troubleshooting.".to_owned(),
                ),
                actions: vec![yes_action],
                dismiss_on_select: true,
                ..Default::default()
            },
            super::SelectionItem {
                name: "No".to_owned(),
                description: Some("".to_owned()),
                actions: vec![no_action],
                dismiss_on_select: true,
                ..Default::default()
            },
        ],
        header: Box::new(crate::render::renderable::ColumnRenderable::with(
            header_lines,
        )),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::app_event_sender::AppEventSender;

    fn render(view: &FeedbackNoteView, width: u16) -> String {
        let height = view.desired_height(width);
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let mut lines: Vec<String> = (0..area.height)
            .map(|row| {
                let mut line = String::new();
                for col in 0..area.width {
                    let symbol = buf[(area.x + col, area.y + row)].symbol();
                    if symbol.is_empty() {
                        line.push(' ');
                    } else {
                        line.push_str(symbol);
                    }
                }
                line.trim_end().to_string()
            })
            .collect();

        while lines.first().is_some_and(|l| l.trim().is_empty()) {
            lines.remove(0);
        }
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    fn make_view(category: FeedbackCategory) -> FeedbackNoteView {
        let (tx_raw, _rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let snapshot = savfox_feedback::SavfoxFeedback::new().snapshot(None);
        FeedbackNoteView::new(
            category,
            snapshot,
            None,
            tx,
            true,
            FeedbackAudience::External,
        )
    }

    #[test]
    fn feedback_view_bad_result() {
        let view = make_view(FeedbackCategory::BadResult);
        let rendered = render(&view, 60);
        insta::assert_snapshot!("feedback_view_bad_result", rendered);
    }

    #[test]
    fn feedback_view_good_result() {
        let view = make_view(FeedbackCategory::GoodResult);
        let rendered = render(&view, 60);
        insta::assert_snapshot!("feedback_view_good_result", rendered);
    }

    #[test]
    fn feedback_view_bug() {
        let view = make_view(FeedbackCategory::Bug);
        let rendered = render(&view, 60);
        insta::assert_snapshot!("feedback_view_bug", rendered);
    }

    #[test]
    fn feedback_view_other() {
        let view = make_view(FeedbackCategory::Other);
        let rendered = render(&view, 60);
        insta::assert_snapshot!("feedback_view_other", rendered);
    }

    #[test]
    fn issue_url_available_for_bug_bad_result_and_other() {
        let bug_url = issue_url_for_category(
            FeedbackCategory::Bug,
            "session-1",
            FeedbackAudience::OpenAiEmployee,
        );
        let expected_slack_url = "http://go/savfox-feedback-internal".to_string();
        assert_eq!(bug_url.as_deref(), Some(expected_slack_url.as_str()));

        let bad_result_url = issue_url_for_category(
            FeedbackCategory::BadResult,
            "session-2",
            FeedbackAudience::OpenAiEmployee,
        );
        assert!(bad_result_url.is_some());

        let other_url = issue_url_for_category(
            FeedbackCategory::Other,
            "session-3",
            FeedbackAudience::OpenAiEmployee,
        );
        assert!(other_url.is_some());

        assert!(
            issue_url_for_category(
                FeedbackCategory::GoodResult,
                "t",
                FeedbackAudience::OpenAiEmployee
            )
            .is_none()
        );
        let bug_url_non_employee =
            issue_url_for_category(FeedbackCategory::Bug, "t", FeedbackAudience::External);
        let expected_external_url = format!("{BASE_BUG_ISSUE_URL}&steps=Uploaded%20session:%20t");
        assert_eq!(
            bug_url_non_employee.as_deref(),
            Some(expected_external_url.as_str())
        );
    }
}
