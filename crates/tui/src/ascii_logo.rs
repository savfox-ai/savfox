//! ASCII art logo for Savfox CLI startup screen.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::render::renderable::Renderable;

/// Savfox ASCII logo – spells SAVFOX using half-block glyphs for finer detail.
pub const SAVFOX_LOGO: [&str; 5] = [
    "▄█▀▀▀▀█▄  ▄█▀▀█▄  ██    ██ ██████  ▄█▀▀█▄  ██    ██",
    "██▄▄     ██    ██ ██    ██ ██     ██    ██  ▀█▄▄█▀ ",
    " ▀▀▀▀█▄  ████████ ██    ██ █████  ██    ██    ██   ",
    "▄▄    ██ ██    ██  ██  ██  ██     ██    ██  ▄█▀▀█▄ ",
    " ▀████▀  ██    ██   ▀██▀   ██      ▀█▄▄█▀  ██    ██",
];

/// Width of the ASCII logo in columns.
#[allow(dead_code)]
pub const LOGO_WIDTH: u16 = 51;

/// Height of the ASCII logo in rows.
pub const LOGO_HEIGHT: u16 = 6;

/// Fire-red gradient colors for each letter (S-A-V-F-O-X).
const FIRE_COLORS: [Color; 6] = [
    Color::Rgb(180, 40, 20),  // S – deep crimson
    Color::Rgb(210, 50, 10),  // A – fire red
    Color::Rgb(235, 70, 0),   // V – red-orange
    Color::Rgb(255, 100, 0),  // F – orange
    Color::Rgb(255, 140, 10), // O – amber
    Color::Rgb(255, 175, 30), // X – gold
];

/// Column ranges for each of the 6 letters in SAVFOX.
const LETTER_RANGES: [(usize, usize); 6] = [
    (0, 8),   // S
    (9, 17),  // A
    (18, 26), // V
    (27, 33), // F
    (34, 42), // O
    (43, 51), // X
];

/// Render the ASCII logo as styled lines with fire gradient.
pub fn logo_lines(_fg_color: Color, _shadow_color: Color, bold: bool) -> Vec<Line<'static>> {
    SAVFOX_LOGO
        .iter()
        
        .map(|row_str| {
            let chars: Vec<char> = row_str.chars().collect();
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(14);
            let mut pos = 0;

            for (li, &(start, end)) in LETTER_RANGES.iter().enumerate() {
                // Add gap before this letter
                if pos < start {
                    let gap: String = chars[pos..start].iter().collect();
                    spans.push(Span::from(gap));
                }
                // Add the letter with its fire color
                let content: String = chars[start..end].iter().collect();
                let mut style = Style::default().fg(FIRE_COLORS[li]);
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                spans.push(Span::styled(content, style));
                pos = end;
            }

            // Add any remaining content after the last letter
            if pos < chars.len() {
                let remaining: String = chars[pos..].iter().collect();
                spans.push(Span::from(remaining));
            }

            Line::from(spans)
        })
        .collect()
}

/// Widget for rendering the Savfox ASCII logo.
pub struct AsciiLogo {
    fg_color: Color,
    shadow_color: Color,
    bold: bool,
    center: bool,
}

impl AsciiLogo {
    pub fn new(fg_color: Color, shadow_color: Color) -> Self {
        Self {
            fg_color,
            shadow_color,
            bold: true,
            center: true,
        }
    }

    pub fn bold(mut self, bold: bool) -> Self {
        self.bold = bold;
        self
    }

    pub fn center(mut self, center: bool) -> Self {
        self.center = center;
        self
    }

    fn render_content(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let lines = logo_lines(self.fg_color, self.shadow_color, self.bold);

        for (idx, line) in lines.iter().enumerate() {
            let y = area.y + idx as u16;
            if y >= area.y + area.height {
                break;
            }

            let x = if self.center {
                let line_width = line.width() as u16;
                if line_width < area.width {
                    area.x + (area.width - line_width) / 2
                } else {
                    area.x
                }
            } else {
                area.x
            };

            let max_width = area.width.saturating_sub(x.saturating_sub(area.x));
            let line_area = Rect::new(x, y, max_width.min(line.width() as u16 + 1), 1);
            line.render(line_area, buf);
        }
    }
}

impl Renderable for AsciiLogo {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_content(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        LOGO_HEIGHT
    }
}

/// Set the terminal window title using OSC 0 escape sequence.
pub fn set_terminal_title(title: &str) {
    let sequence = format!("\x1b]0;{title}\x07");
    let _ = std::io::Write::write_all(&mut std::io::stdout(), sequence.as_bytes());
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// Clear the terminal window title.
pub fn clear_terminal_title() {
    set_terminal_title("");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logo_lines_count() {
        let lines = logo_lines(Color::White, Color::DarkGray, true);
        assert_eq!(lines.len(), 5);
    }
}
