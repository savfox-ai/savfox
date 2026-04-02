use std::collections::VecDeque;

use ratatui::text::Line;

use crate::markdown_stream::MarkdownStreamCollector;
pub(crate) mod controller;

/// How many lines the streaming animation commits per tick.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub(crate) enum StreamingSpeed {
    /// Default: one line per tick.
    #[default]
    Normal,
    /// Two to three lines per tick for faster output.
    Fast,
    /// No animation: drain all queued lines immediately.
    Instant,
}


#[allow(dead_code)]
impl StreamingSpeed {
    /// Parse from a config string (case-insensitive).
    pub(crate) fn from_str_loose(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "fast" => Self::Fast,
            "instant" | "off" | "none" => Self::Instant,
            _ => Self::Normal,
        }
    }
}

pub(crate) struct StreamState {
    pub(crate) collector: MarkdownStreamCollector,
    queued_lines: VecDeque<Line<'static>>,
    pub(crate) has_seen_delta: bool,
    speed: StreamingSpeed,
}

impl StreamState {
    pub(crate) fn new(width: Option<usize>) -> Self {
        Self::with_speed(width, StreamingSpeed::default())
    }

    pub(crate) fn with_speed(width: Option<usize>, speed: StreamingSpeed) -> Self {
        Self {
            collector: MarkdownStreamCollector::new(width),
            queued_lines: VecDeque::new(),
            has_seen_delta: false,
            speed,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.collector.clear();
        self.queued_lines.clear();
        self.has_seen_delta = false;
    }

    /// Pop one or more lines from the queue depending on the configured speed.
    pub(crate) fn step(&mut self) -> Vec<Line<'static>> {
        match self.speed {
            StreamingSpeed::Normal => self.queued_lines.pop_front().into_iter().collect(),
            StreamingSpeed::Fast => {
                let n = 3.min(self.queued_lines.len());
                self.queued_lines.drain(..n).collect()
            }
            StreamingSpeed::Instant => self.queued_lines.drain(..).collect(),
        }
    }

    pub(crate) fn drain_all(&mut self) -> Vec<Line<'static>> {
        self.queued_lines.drain(..).collect()
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.queued_lines.is_empty()
    }

    pub(crate) fn enqueue(&mut self, lines: Vec<Line<'static>>) {
        self.queued_lines.extend(lines);
    }
}
