//! Session branch/rollback history view.
//!
//! Renders a summary of the current session's backtrack history as lines that
//! can be displayed in a pager overlay.  This gives users visibility into which
//! rollback points exist and which one is currently active.
//!
//! Activated via `Ctrl+B` from the main chat screen.

use std::sync::Arc;

use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

use crate::history_cell::{HistoryCell, UserHistoryCell};

/// Compact summary of the session backtrack state.
#[allow(dead_code)]
pub(crate) struct BranchSummary {
    /// Each entry is a user message that is a potential rollback point.
    pub(crate) entries: Vec<BranchEntry>,
    /// Index of the currently highlighted entry, if any.
    pub(crate) active: Option<usize>,
}

#[allow(dead_code)]
pub(crate) struct BranchEntry {
    /// One-line preview of the user message.
    pub(crate) preview: String,
    /// Cell index in the transcript.
    pub(crate) cell_index: usize,
}

/// Build a branch summary from committed transcript cells.
///
/// This scans the transcript for `UserHistoryCell` entries and builds a list
/// of potential rollback points.
#[allow(dead_code)]
pub(crate) fn build_branch_summary(
    cells: &[Arc<dyn HistoryCell>],
    active_user_index: Option<usize>,
) -> BranchSummary {
    let mut entries = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        if let Some(user_cell) = cell.as_any().downcast_ref::<UserHistoryCell>() {
            let preview = truncate_preview(&user_cell.message, 60);
            entries.push(BranchEntry {
                preview,
                cell_index: i,
            });
        }
    }

    let active =
        active_user_index.and_then(|idx| if idx < entries.len() { Some(idx) } else { None });

    BranchSummary { entries, active }
}

/// Render the branch summary as lines for display in a pager overlay.
#[allow(dead_code)]
pub(crate) fn render_branch_lines(summary: &BranchSummary) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        "Session History".bold(),
        " — ".dim(),
        "rollback points".dim(),
    ]));
    lines.push(Line::from(""));

    if summary.entries.is_empty() {
        lines.push(Line::from("  (no user messages yet)".dim().italic()));
        return lines;
    }

    for (i, entry) in summary.entries.iter().enumerate() {
        let is_active = summary.active == Some(i);
        let marker = if is_active { "▸ " } else { "  " };
        let number = format!("{}. ", i + 1);
        let style = if is_active {
            ratatui::style::Style::default().cyan().bold()
        } else {
            ratatui::style::Style::default()
        };

        lines.push(Line::from(vec![
            Span::styled(marker, style),
            Span::styled(number, style),
            Span::styled(entry.preview.clone(), style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(
        "Press Esc to go back, Enter on a message to rollback".dim(),
    ));

    lines
}

fn truncate_preview(message: &str, max_chars: usize) -> String {
    let first_line = message.lines().next().unwrap_or("");
    if first_line.chars().count() <= max_chars {
        first_line.to_string()
    } else {
        let mut preview: String = first_line.chars().take(max_chars - 3).collect();
        preview.push_str("...");
        preview
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_preview_short_message() {
        assert_eq!(truncate_preview("hello", 10), "hello");
    }

    #[test]
    fn truncate_preview_long_message() {
        let msg = "a".repeat(100);
        let result = truncate_preview(&msg, 20);
        assert_eq!(result.len(), 20);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_preview_multiline() {
        let msg = "first line\nsecond line";
        assert_eq!(truncate_preview(msg, 60), "first line");
    }

    #[test]
    fn empty_summary_renders_placeholder() {
        let summary = BranchSummary {
            entries: vec![],
            active: None,
        };
        let lines = render_branch_lines(&summary);
        assert!(lines.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| s.content.contains("no user messages"))
        }));
    }
}
