use std::cell::Cell;

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph, WidgetRef, Wrap};

use super::onboarding_screen::StepState;
use crate::onboarding::onboarding_screen::{KeyboardHandler, StepStateProvider};
use crate::tui::FrameRequester;

pub(crate) struct WelcomeWidget {
    pub is_logged_in: bool,
    animations_enabled: bool,
    layout_area: Cell<Option<Rect>>,
}

impl KeyboardHandler for WelcomeWidget {
    fn handle_key_event(&mut self, _key_event: KeyEvent) {
        // No animation controls needed
    }
}

impl WelcomeWidget {
    pub(crate) fn new(
        is_logged_in: bool,
        _request_frame: FrameRequester,
        animations_enabled: bool,
    ) -> Self {
        Self {
            is_logged_in,
            animations_enabled,
            layout_area: Cell::new(None),
        }
    }

    pub(crate) fn update_layout_area(&self, area: Rect) {
        self.layout_area.set(Some(area));
    }
}

impl WidgetRef for &WelcomeWidget {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let _ = self.animations_enabled; // kept for API compatibility

        let lines: Vec<Line> = vec![Line::from(vec![
            "  ".into(),
            "Welcome to ".into(),
            "Savfox".bold(),
            ", command-line coding agent".into(),
        ])];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

impl StepStateProvider for WelcomeWidget {
    fn get_step_state(&self) -> StepState {
        match self.is_logged_in {
            true => StepState::Hidden,
            false => StepState::Complete,
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::WidgetRef;

    use super::*;

    fn row_containing(buf: &Buffer, needle: &str) -> Option<u16> {
        (0..buf.area.height).find(|&y| {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            row.contains(needle)
        })
    }

    #[test]
    fn welcome_renders_welcome_message() {
        let widget = WelcomeWidget::new(false, FrameRequester::test_dummy(), true);
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        (&widget).render_ref(area, &mut buf);

        let welcome_row = row_containing(&buf, "Welcome");
        assert_eq!(welcome_row, Some(0));
    }
}
