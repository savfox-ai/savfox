//! Startup hero for Savfox CLI.
//!
//! This module provides the startup hero screen with ASCII logo display
//! before the first user input. After the user submits their first message,
//! the app transitions to the full session/chat interface.
//!
//! When the terminal supports Kitty, Sixel, or iTerm2 graphics protocols the
//! embedded `savfox.png` icon is rendered to the left of the ASCII art text
//! for a high-resolution logo.  Otherwise a half-block fallback is used.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};

use crate::ascii_logo::{AsciiLogo, LOGO_HEIGHT, LOGO_WIDTH, logo_lines};
use crate::logo_image::LogoImage;
use crate::render::renderable::Renderable;

const WELCOME_TEXT: &str = "Welcome to Savfox";
const SUBTITLE_TEXT: &str = "Your AI coding assistant";

/// Width of the logo image area in terminal columns.
const IMAGE_COLS: u16 = 10;
/// Gap between image and ASCII text in columns.
const IMAGE_GAP: u16 = 1;

pub struct StartupHero {
    fg_color: Color,
    logo: AsciiLogo,
    logo_image: Option<LogoImage>,
    tooltip: Option<String>,
}

impl StartupHero {
    #[allow(dead_code)]
    pub fn new(fg_color: Color, shadow_color: Color) -> Self {
        let logo = AsciiLogo::new(fg_color, shadow_color)
            .bold(true)
            .center(true);
        let logo_image = LogoImage::new();
        Self {
            fg_color,
            logo,
            logo_image,
            tooltip: None,
        }
    }

    pub fn with_tooltip(fg_color: Color, shadow_color: Color, tooltip: Option<String>) -> Self {
        let logo = AsciiLogo::new(fg_color, shadow_color)
            .bold(true)
            .center(true);
        let logo_image = LogoImage::new();
        Self {
            fg_color,
            logo,
            logo_image,
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

        let logo_height = LOGO_HEIGHT.min(bottom.saturating_sub(y));
        if logo_height > 0 {
            if let Some(ref logo_image) = self.logo_image {
                // ── Image + ASCII text side by side, horizontally centred ──
                let combined_width = IMAGE_COLS + IMAGE_GAP + LOGO_WIDTH;
                let start_x = if combined_width < area.width {
                    area.x + (area.width - combined_width) / 2
                } else {
                    area.x
                };

                // Render the PNG icon on the left.
                let img_area = Rect::new(start_x, y, IMAGE_COLS, logo_height);
                logo_image.render(img_area, buf);

                // Render the fire-gradient ASCII text to the right of the icon.
                let text_x = start_x + IMAGE_COLS + IMAGE_GAP;
                let lines = logo_lines(self.fg_color, Color::DarkGray, true);
                for (idx, line) in lines.iter().enumerate() {
                    let ly = y + idx as u16;
                    if ly >= y + logo_height {
                        break;
                    }
                    let max_w =
                        LOGO_WIDTH.min(area.width.saturating_sub(text_x.saturating_sub(area.x)));
                    buf.set_line(text_x, ly, line, max_w);
                }
            } else {
                // Fallback: render ASCII logo centred (original behaviour).
                let logo_area = Rect::new(area.x, y, area.width, logo_height);
                self.logo.render(logo_area, buf);
            }
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
        let hero = StartupHero {
            fg_color: Color::White,
            logo: AsciiLogo::new(Color::White, Color::DarkGray)
                .bold(true)
                .center(true),
            logo_image: None, // skip image in tests
            tooltip: None,
        };
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        hero.render(area, &mut buf);

        let content = buf.content.iter().map(|c| c.symbol()).collect::<String>();
        assert!(content.contains("Welcome"));
    }

    #[test]
    fn test_startup_hero_with_tooltip() {
        let hero = StartupHero {
            fg_color: Color::White,
            logo: AsciiLogo::new(Color::White, Color::DarkGray)
                .bold(true)
                .center(true),
            logo_image: None,
            tooltip: Some("Test tooltip".to_string()),
        };
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        hero.render(area, &mut buf);

        let content = buf.content.iter().map(|c| c.symbol()).collect::<String>();
        assert!(content.contains("Test tooltip"));
    }

    #[test]
    fn test_startup_hero_height() {
        let hero = StartupHero {
            fg_color: Color::White,
            logo: AsciiLogo::new(Color::White, Color::DarkGray)
                .bold(true)
                .center(true),
            logo_image: None,
            tooltip: None,
        };
        let height = hero.desired_height(80);
        assert!(height >= LOGO_HEIGHT);
    }
}
