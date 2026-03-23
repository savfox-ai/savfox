use ratatui::prelude::Stylize;
use ratatui::text::Line;

use super::StreamState;
use crate::history_cell::{
    HistoryCell, {self},
};
use crate::render::line_utils::prefix_lines;
use crate::style::proposed_plan_style;

/// Trait that defines how streamed lines are turned into history cells.
///
/// The generic `BaseStreamController<E>` handles the shared push/finalize/tick
/// logic; each `StreamEmitter` implementation only decides how to wrap the
/// accumulated lines into a concrete `HistoryCell`.
pub(crate) trait StreamEmitter {
    /// Wrap `lines` into a history cell.  Return `None` when the lines are
    /// empty and no cell should be emitted.
    fn emit(&mut self, lines: Vec<Line<'static>>) -> Option<Box<dyn HistoryCell>>;
}

/// Generic stream controller that manages newline-gated streaming, commit
/// animation, and delegates cell emission to an `StreamEmitter`.
pub(crate) struct BaseStreamController<E: StreamEmitter> {
    pub(crate) state: StreamState,
    pub(crate) finishing_after_drain: bool,
    pub(crate) emitter: E,
}

impl<E: StreamEmitter> BaseStreamController<E> {
    /// Push a delta; if it contains a newline, commit completed lines and start animation.
    pub(crate) fn push(&mut self, delta: &str) -> bool {
        let state = &mut self.state;
        if !delta.is_empty() {
            state.has_seen_delta = true;
        }
        state.collector.push_delta(delta);
        if delta.contains('\n') {
            let newly_completed = state.collector.commit_complete_lines();
            if !newly_completed.is_empty() {
                state.enqueue(newly_completed);
                return true;
            }
        }
        false
    }

    /// Step animation: commit at most one queued line and handle end-of-drain cleanup.
    pub(crate) fn on_commit_tick(&mut self) -> (Option<Box<dyn HistoryCell>>, bool) {
        let step = self.state.step();
        (self.emitter.emit(step), self.state.is_idle())
    }

    /// Drain all remaining lines and emit them, cleaning up state.
    fn drain_and_emit(&mut self) -> Vec<Line<'static>> {
        let remaining = self.state.collector.finalize_and_drain();
        let mut out_lines = Vec::new();
        if !remaining.is_empty() {
            self.state.enqueue(remaining);
        }
        out_lines.extend(self.state.drain_all());
        self.state.clear();
        self.finishing_after_drain = false;
        out_lines
    }
}

// ---------------------------------------------------------------------------
// Agent message emitter
// ---------------------------------------------------------------------------

pub(crate) struct AgentMessageEmitter {
    header_emitted: bool,
}

impl StreamEmitter for AgentMessageEmitter {
    fn emit(&mut self, lines: Vec<Line<'static>>) -> Option<Box<dyn HistoryCell>> {
        if lines.is_empty() {
            return None;
        }
        Some(Box::new(history_cell::AgentMessageCell::new(lines, {
            let header_emitted = self.header_emitted;
            self.header_emitted = true;
            !header_emitted
        })))
    }
}

/// Controller that manages newline-gated streaming, header emission, and
/// commit animation across streams.
pub(crate) type StreamController = BaseStreamController<AgentMessageEmitter>;

impl StreamController {
    pub(crate) fn new(width: Option<usize>) -> Self {
        Self {
            state: StreamState::new(width),
            finishing_after_drain: false,
            emitter: AgentMessageEmitter {
                header_emitted: false,
            },
        }
    }

    /// Finalize the active stream. Drain and emit now.
    pub(crate) fn finalize(&mut self) -> Option<Box<dyn HistoryCell>> {
        let out_lines = self.drain_and_emit();
        self.emitter.emit(out_lines)
    }
}

// ---------------------------------------------------------------------------
// Plan stream emitter
// ---------------------------------------------------------------------------

pub(crate) struct PlanEmitter {
    header_emitted: bool,
    top_padding_emitted: bool,
}

impl PlanEmitter {
    fn emit_plan(
        &mut self,
        lines: Vec<Line<'static>>,
        include_bottom_padding: bool,
    ) -> Option<Box<dyn HistoryCell>> {
        if lines.is_empty() && !include_bottom_padding {
            return None;
        }

        let mut out_lines: Vec<Line<'static>> = Vec::new();
        let is_stream_continuation = self.header_emitted;
        if !self.header_emitted {
            out_lines.push(vec!["• ".dim(), "Proposed Plan".bold()].into());
            out_lines.push(Line::from(" "));
            self.header_emitted = true;
        }

        let mut plan_lines: Vec<Line<'static>> = Vec::new();
        if !self.top_padding_emitted {
            plan_lines.push(Line::from(" "));
            self.top_padding_emitted = true;
        }
        plan_lines.extend(lines);
        if include_bottom_padding {
            plan_lines.push(Line::from(" "));
        }

        let plan_style = proposed_plan_style();
        let plan_lines = prefix_lines(plan_lines, "  ".into(), "  ".into())
            .into_iter()
            .map(|line| line.style(plan_style))
            .collect::<Vec<_>>();
        out_lines.extend(plan_lines);

        Some(Box::new(history_cell::new_proposed_plan_stream(
            out_lines,
            is_stream_continuation,
        )))
    }
}

impl StreamEmitter for PlanEmitter {
    fn emit(&mut self, lines: Vec<Line<'static>>) -> Option<Box<dyn HistoryCell>> {
        self.emit_plan(lines, false)
    }
}

/// Controller that streams proposed plan markdown into a styled plan block.
pub(crate) type PlanStreamController = BaseStreamController<PlanEmitter>;

impl PlanStreamController {
    pub(crate) fn new(width: Option<usize>) -> Self {
        Self {
            state: StreamState::new(width),
            finishing_after_drain: false,
            emitter: PlanEmitter {
                header_emitted: false,
                top_padding_emitted: false,
            },
        }
    }

    /// Finalize the plan stream, including bottom padding.
    pub(crate) fn finalize(&mut self) -> Option<Box<dyn HistoryCell>> {
        let out_lines = self.drain_and_emit();
        self.emitter.emit_plan(out_lines, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_to_plain_strings(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.clone())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    #[tokio::test]
    async fn controller_loose_vs_tight_with_commit_ticks_matches_full() {
        let mut ctrl = StreamController::new(None);
        let mut lines = Vec::new();

        // Exact deltas from the session log (section: Loose vs. tight list items)
        let deltas = vec![
            "\n\n",
            "Loose",
            " vs",
            ".",
            " tight",
            " list",
            " items",
            ":\n",
            "1",
            ".",
            " Tight",
            " item",
            "\n",
            "2",
            ".",
            " Another",
            " tight",
            " item",
            "\n\n",
            "1",
            ".",
            " Loose",
            " item",
            " with",
            " its",
            " own",
            " paragraph",
            ".\n\n",
            "  ",
            " This",
            " paragraph",
            " belongs",
            " to",
            " the",
            " same",
            " list",
            " item",
            ".\n\n",
            "2",
            ".",
            " Second",
            " loose",
            " item",
            " with",
            " a",
            " nested",
            " list",
            " after",
            " a",
            " blank",
            " line",
            ".\n\n",
            "  ",
            " -",
            " Nested",
            " bullet",
            " under",
            " a",
            " loose",
            " item",
            "\n",
            "  ",
            " -",
            " Another",
            " nested",
            " bullet",
            "\n\n",
        ];

        // Simulate streaming with a commit tick attempt after each delta.
        for d in deltas.iter() {
            ctrl.push(d);
            while let (Some(cell), idle) = ctrl.on_commit_tick() {
                lines.extend(cell.transcript_lines(u16::MAX));
                if idle {
                    break;
                }
            }
        }
        // Finalize and flush remaining lines now.
        if let Some(cell) = ctrl.finalize() {
            lines.extend(cell.transcript_lines(u16::MAX));
        }

        let streamed: Vec<_> = lines_to_plain_strings(&lines)
            .into_iter()
            // skip • and 2-space indentation
            .map(|s| s.chars().skip(2).collect::<String>())
            .collect();

        // Full render of the same source
        let source: String = deltas.iter().copied().collect();
        let mut rendered: Vec<ratatui::text::Line<'static>> = Vec::new();
        crate::markdown::append_markdown(&source, None, &mut rendered);
        let rendered_strs = lines_to_plain_strings(&rendered);

        assert_eq!(streamed, rendered_strs);

        // Also assert exact expected plain strings for clarity.
        let expected = vec![
            "Loose vs. tight list items:".to_string(),
            "".to_string(),
            "1. Tight item".to_string(),
            "2. Another tight item".to_string(),
            "3. Loose item with its own paragraph.".to_string(),
            "".to_string(),
            "   This paragraph belongs to the same list item.".to_string(),
            "4. Second loose item with a nested list after a blank line.".to_string(),
            "    - Nested bullet under a loose item".to_string(),
            "    - Another nested bullet".to_string(),
        ];
        assert_eq!(
            streamed, expected,
            "expected exact rendered lines for loose/tight section"
        );
    }
}
