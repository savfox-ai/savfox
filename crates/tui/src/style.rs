use ratatui::style::{Color, Style};

use crate::color::{blend, is_light};
use crate::terminal_palette::{best_color, default_bg};

pub fn user_message_style() -> Style {
    user_message_style_for(default_bg())
}

pub fn proposed_plan_style() -> Style {
    proposed_plan_style_for(default_bg())
}

/// Returns the style for a user-authored message using the provided terminal background.
pub fn user_message_style_for(terminal_bg: Option<(u8, u8, u8)>) -> Style {
    match terminal_bg {
        Some(bg) => Style::default().bg(user_message_bg(bg)),
        None => Style::default(),
    }
}

pub fn proposed_plan_style_for(terminal_bg: Option<(u8, u8, u8)>) -> Style {
    match terminal_bg {
        Some(bg) => Style::default().bg(proposed_plan_bg(bg)),
        None => Style::default(),
    }
}

#[allow(clippy::disallowed_methods)]
pub fn user_message_bg(terminal_bg: (u8, u8, u8)) -> Color {
    let (top, alpha) = if is_light(terminal_bg) {
        ((0, 0, 0), 0.04)
    } else {
        ((255, 255, 255), 0.12)
    };
    best_color(blend(top, terminal_bg, alpha))
}

#[allow(clippy::disallowed_methods)]
pub fn proposed_plan_bg(terminal_bg: (u8, u8, u8)) -> Color {
    user_message_bg(terminal_bg)
}

pub fn input_bg(terminal_bg: (u8, u8, u8)) -> Color {
    // Match opencode's backgroundElement: slightly elevated from base.
    let bg = if is_light(terminal_bg) {
        (245, 245, 245) // #f5f5f5 – light mode
    } else {
        (30, 30, 30) // #1e1e1e – dark mode
    };
    best_color(bg)
}

pub fn cursor_fg(terminal_bg: (u8, u8, u8)) -> Color {
    if is_light(terminal_bg) {
        Color::Rgb(59, 125, 216)
    } else {
        Color::Rgb(250, 178, 131)
    }
}
