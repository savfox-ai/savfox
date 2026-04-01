//! Plan-related history cells: proposed plans, plan updates, plan streams.

use ratatui::style::{Style, Styled, Stylize};
use ratatui::text::Line;
use savfox_protocol::plan_tool::{PlanItemArg, StepStatus, UpdatePlanArgs};
use unicode_width::UnicodeWidthStr;

use super::HistoryCell;
use crate::markdown::append_markdown;
use crate::render::line_utils::prefix_lines;
use crate::style::proposed_plan_style;

/// Render a user-friendly plan update styled like a checkbox todo list.
pub(crate) fn new_plan_update(update: UpdatePlanArgs) -> PlanUpdateCell {
    let UpdatePlanArgs { explanation, plan } = update;
    PlanUpdateCell { explanation, plan }
}

pub(crate) fn new_proposed_plan(plan_markdown: String) -> ProposedPlanCell {
    ProposedPlanCell { plan_markdown }
}

pub(crate) fn new_proposed_plan_stream(
    lines: Vec<Line<'static>>,
    is_stream_continuation: bool,
) -> ProposedPlanStreamCell {
    ProposedPlanStreamCell {
        lines,
        is_stream_continuation,
    }
}

#[derive(Debug)]
pub(crate) struct ProposedPlanCell {
    plan_markdown: String,
}

#[derive(Debug)]
pub(crate) struct ProposedPlanStreamCell {
    lines: Vec<Line<'static>>,
    is_stream_continuation: bool,
}

impl HistoryCell for ProposedPlanCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(vec!["• ".dim(), "Proposed Plan".bold()].into());
        lines.push(Line::from(" "));

        let mut plan_lines: Vec<Line<'static>> = vec![Line::from(" ")];
        let plan_style = proposed_plan_style();
        let wrap_width = width.saturating_sub(4).max(1) as usize;
        let mut body: Vec<Line<'static>> = Vec::new();
        append_markdown(&self.plan_markdown, Some(wrap_width), &mut body);
        if body.is_empty() {
            body.push(Line::from("(empty)".dim().italic()));
        }
        plan_lines.extend(prefix_lines(body, "  ".into(), "  ".into()));
        plan_lines.push(Line::from(" "));

        lines.extend(plan_lines.into_iter().map(|line| line.style(plan_style)));
        lines
    }
}

impl HistoryCell for ProposedPlanStreamCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }

    fn is_stream_continuation(&self) -> bool {
        self.is_stream_continuation
    }
}

#[derive(Debug)]
pub(crate) struct PlanUpdateCell {
    explanation: Option<String>,
    plan: Vec<PlanItemArg>,
}

impl HistoryCell for PlanUpdateCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let render_note = |text: &str| -> Vec<Line<'static>> {
            let wrap_width = width.saturating_sub(4).max(1) as usize;
            textwrap::wrap(text, wrap_width)
                .into_iter()
                .map(|s| s.to_string().dim().italic().into())
                .collect()
        };

        let render_step = |status: &StepStatus, text: &str| -> Vec<Line<'static>> {
            let (box_str, step_style) = match status {
                StepStatus::Completed => ("✔ ", Style::default().crossed_out().dim()),
                StepStatus::InProgress => ("□ ", Style::default().cyan().bold()),
                StepStatus::Pending => ("□ ", Style::default().dim()),
            };
            let wrap_width = (width as usize)
                .saturating_sub(4)
                .saturating_sub(box_str.width())
                .max(1);
            let parts = textwrap::wrap(text, wrap_width);
            let step_text = parts
                .into_iter()
                .map(|s| s.to_string().set_style(step_style).into())
                .collect();
            prefix_lines(step_text, box_str.into(), "  ".into())
        };

        let mut lines: Vec<Line<'static>> = vec![];
        lines.push(vec!["• ".dim(), "Updated Plan".bold()].into());

        let mut indented_lines = vec![];
        let note = self
            .explanation
            .as_ref()
            .map(|s| s.trim())
            .filter(|t| !t.is_empty());
        if let Some(expl) = note {
            indented_lines.extend(render_note(expl));
        };

        if self.plan.is_empty() {
            indented_lines.push(Line::from("(no steps provided)".dim().italic()));
        } else {
            for PlanItemArg { step, status } in self.plan.iter() {
                indented_lines.extend(render_step(status, step));
            }
        }
        lines.extend(prefix_lines(indented_lines, "  └ ".dim(), "    ".into()));

        lines
    }
}
