//! The startup/reconnect session header card rendered at the top of new sessions.

use std::path::{Path, PathBuf};

use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use savfox_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use unicode_width::UnicodeWidthStr;

use super::{HistoryCell, SESSION_HEADER_MAX_INNER_WIDTH, card_inner_width, with_border};
use crate::exec_command::relativize_to_home;

#[derive(Debug)]
pub(crate) struct SessionHeaderHistoryCell {
    version: &'static str,
    model: String,
    model_style: Style,
    reasoning_effort: Option<ReasoningEffortConfig>,
    directory: PathBuf,
    title_kind: SessionHeaderTitleKind,
    center_in_viewport: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum SessionHeaderTitleKind {
    TextTitle,
    AsciiLogoTitle,
}

impl SessionHeaderHistoryCell {
    pub(crate) fn new(
        model: String,
        reasoning_effort: Option<ReasoningEffortConfig>,
        directory: PathBuf,
        version: &'static str,
    ) -> Self {
        Self::new_with_style(
            model,
            Style::default(),
            reasoning_effort,
            directory,
            version,
        )
    }

    pub(crate) fn new_with_style(
        model: String,
        model_style: Style,
        reasoning_effort: Option<ReasoningEffortConfig>,
        directory: PathBuf,
        version: &'static str,
    ) -> Self {
        Self {
            version,
            model,
            model_style,
            reasoning_effort,
            directory,
            title_kind: SessionHeaderTitleKind::TextTitle,
            center_in_viewport: false,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn new_startup_with_style(
        model: String,
        model_style: Style,
        reasoning_effort: Option<ReasoningEffortConfig>,
        directory: PathBuf,
        version: &'static str,
    ) -> Self {
        Self {
            version,
            model,
            model_style,
            reasoning_effort,
            directory,
            title_kind: SessionHeaderTitleKind::AsciiLogoTitle,
            center_in_viewport: true,
        }
    }

    pub(crate) fn format_directory(&self, max_width: Option<usize>) -> String {
        Self::format_directory_inner(&self.directory, max_width)
    }

    pub(crate) fn format_directory_inner(directory: &Path, max_width: Option<usize>) -> String {
        let formatted = if let Some(rel) = relativize_to_home(directory) {
            if rel.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~{}{}", std::path::MAIN_SEPARATOR, rel.display())
            }
        } else {
            directory.display().to_string()
        };

        if let Some(max_width) = max_width {
            if max_width == 0 {
                return String::new();
            }
            if UnicodeWidthStr::width(formatted.as_str()) > max_width {
                return crate::text_formatting::center_truncate_path(&formatted, max_width);
            }
        }

        formatted
    }

    fn reasoning_label(&self) -> Option<&'static str> {
        self.reasoning_effort.map(|effort| match effort {
            ReasoningEffortConfig::Minimal => "minimal",
            ReasoningEffortConfig::Low => "low",
            ReasoningEffortConfig::Medium => "medium",
            ReasoningEffortConfig::High => "high",
            ReasoningEffortConfig::XHigh => "xhigh",
            ReasoningEffortConfig::None => "none",
        })
    }

    fn line_width(line: &Line<'_>) -> usize {
        line.iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum()
    }

    fn center_lines(lines: Vec<Line<'static>>, viewport_width: u16) -> Vec<Line<'static>> {
        let viewport_width = viewport_width as usize;
        if viewport_width == 0 {
            return lines;
        }

        lines
            .into_iter()
            .map(|line| {
                let used_width = Self::line_width(&line);
                if used_width >= viewport_width {
                    return line;
                }

                let left_pad = (viewport_width - used_width) / 2;
                if left_pad == 0 {
                    return line;
                }

                let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 1);
                spans.push(Span::from(" ".repeat(left_pad)));
                spans.extend(line.spans);
                Line::from(spans)
            })
            .collect()
    }
}

impl HistoryCell for SessionHeaderHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(inner_width) = card_inner_width(width, SESSION_HEADER_MAX_INNER_WIDTH) else {
            return Vec::new();
        };

        let make_row = |spans: Vec<Span<'static>>| Line::from(spans);

        const CHANGE_MODEL_HINT_COMMAND: &str = "/model";
        const CHANGE_MODEL_HINT_EXPLANATION: &str = " to change";
        const DIR_LABEL: &str = "directory:";
        let label_width = DIR_LABEL.len();

        let model_label = format!(
            "{model_label:<label_width$}",
            model_label = "model:",
            label_width = label_width
        );
        let reasoning_label = self.reasoning_label();
        let model_spans: Vec<Span<'static>> = {
            let mut spans = vec![
                Span::from(format!("{model_label} ")).dim(),
                Span::styled(self.model.clone(), self.model_style),
            ];
            if let Some(reasoning) = reasoning_label {
                spans.push(Span::from(" "));
                spans.push(Span::from(reasoning));
            }
            spans.push("   ".dim());
            spans.push(CHANGE_MODEL_HINT_COMMAND.cyan());
            spans.push(CHANGE_MODEL_HINT_EXPLANATION.dim());
            spans
        };

        let dir_label = format!("{DIR_LABEL:<label_width$}");
        let dir_prefix = format!("{dir_label} ");
        let dir_prefix_width = UnicodeWidthStr::width(dir_prefix.as_str());
        let dir_max_width = inner_width.saturating_sub(dir_prefix_width);
        let dir = self.format_directory(Some(dir_max_width));
        let dir_spans = vec![Span::from(dir_prefix).dim(), Span::from(dir)];

        let mut lines: Vec<Line<'static>> = match self.title_kind {
            SessionHeaderTitleKind::TextTitle => {
                // Title line rendered inside the box: ">_ Savfox  (vX)"
                let title_spans: Vec<Span<'static>> = vec![
                    Span::from(">_ ").dim(),
                    Span::from("Savfox ").bold(),
                    Span::from(" ").dim(),
                    Span::from(format!("(v{})", self.version)).dim(),
                ];
                vec![make_row(title_spans)]
            }
            SessionHeaderTitleKind::AsciiLogoTitle => {
                let mut logo_lines =
                    crate::ascii_logo::logo_lines(Color::White, Color::DarkGray, true);
                logo_lines.push(Line::from(vec![
                    Span::from("v").dim(),
                    Span::from(self.version.to_string()).dim(),
                ]));
                logo_lines
            }
        };
        lines.push(make_row(Vec::new()));
        lines.push(make_row(model_spans));
        lines.push(make_row(dir_spans));

        let bordered = with_border(lines);
        if self.center_in_viewport {
            Self::center_lines(bordered, width)
        } else {
            bordered
        }
    }
}
