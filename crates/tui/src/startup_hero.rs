//! Startup hero for Savfox CLI.
//!
//! This module provides the startup hero screen with ASCII logo display
//! before the first user input. After the user submits their first message,
//! the app transitions to the full session/chat interface.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};

use crate::ascii_logo::{AsciiLogo, LOGO_HEIGHT};
use crate::render::renderable::Renderable;

const WELCOME_TEXT: &str = "Welcome to Savfox";
const SUBTITLE_TEXT: &str = "Your AI coding assistant";

pub struct StartupHero {
    fg_color: Color,
    logo: AsciiLogo,
    tooltip: Option<String>,
}

impl StartupHero {
    pub fn new(fg_color: Color, shadow_color: Color) -> Self {
        let logo = AsciiLogo::new(fg_color, shadow_color)
            .bold(true)
            .center(true);
        Self {
            fg_color,
            logo,
            tooltip: None,
        }
    }

    pub fn with_tooltip(fg_color: Color, shadow_color: Color, tooltip: Option<String>) -> Self {
        let logo = AsciiLogo::new(fg_color, shadow_color)
            .bold(true)
            .center(true);
        Self {
            fg_color,
            logo,
            tooltip,
        }
    }
}

impl Renderable for StartupHero {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        // Clear the hero region each frame so stale glyphs from previous layouts
        // do not leak into the logo/welcome gap.
        Clear.render(area, buf);

        let mut y = area.y;
        let bottom = area.bottom();

        if y >= bottom {
            return;
        }

        let logo_height = self
            .logo
            .desired_height(area.width)
            .min(bottom.saturating_sub(y));
        if logo_height > 0 {
            let logo_area = Rect::new(area.x, y, area.width, logo_height);
            self.logo.render(logo_area, buf);
            y = y.saturating_add(logo_height);
        }

        if y >= bottom {
            return;
        }
        y = y.saturating_add(1);
        if y >= bottom {
            return;
        }

        let welcome_line = Line::styled(WELCOME_TEXT, Style::default().fg(self.fg_color).bold());
        let welcome_para = Paragraph::new(welcome_line).alignment(Alignment::Center);
        welcome_para.render(Rect::new(area.x, y, area.width, 1), buf);
        y = y.saturating_add(1);
        if y >= bottom {
            return;
        }

        let subtitle_line = Line::styled(SUBTITLE_TEXT, Style::default().fg(Color::DarkGray));
        let subtitle_para = Paragraph::new(subtitle_line).alignment(Alignment::Center);
        subtitle_para.render(Rect::new(area.x, y, area.width, 1), buf);
        y = y.saturating_add(2);
        if y >= bottom {
            return;
        }

        if let Some(ref tooltip) = self.tooltip {
            let tip_line = Line::from(vec![
                Span::styled("● ", Style::default().fg(Color::Rgb(255, 165, 0))), // orange dot
                Span::styled("Tip ", Style::default().fg(Color::Rgb(255, 165, 0)).bold()),
                Span::styled(tooltip.as_str(), Style::default().fg(Color::DarkGray).dim()),
            ]);
            let tip_para = Paragraph::new(tip_line).alignment(Alignment::Center);
            tip_para.render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        LOGO_HEIGHT + if self.tooltip.is_some() { 6 } else { 5 }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;

    use super::*;

    #[test]
    fn test_startup_hero_renders() {
        let hero = StartupHero::new(Color::White, Color::DarkGray);
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        hero.render(area, &mut buf);

        let content = buf.content.iter().map(|c| c.symbol()).collect::<String>();
        assert!(content.contains("Welcome"));
    }

    #[test]
    fn test_startup_hero_with_tooltip() {
        let hero = StartupHero::with_tooltip(
            Color::White,
            Color::DarkGray,
            Some("Test tooltip".to_string()),
        );
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        hero.render(area, &mut buf);

        let content = buf.content.iter().map(|c| c.symbol()).collect::<String>();
        assert!(content.contains("Test tooltip"));
    }

    #[test]
    fn test_startup_hero_height() {
        let hero = StartupHero::new(Color::White, Color::DarkGray);
        let height = hero.desired_height(80);
        assert!(height >= LOGO_HEIGHT);
    }
}
