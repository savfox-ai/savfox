//! Terminal graphics image widget for the Savfox logo.
//!
//! Renders the embedded `savfox.png` using the best available terminal graphics
//! protocol: Kitty > Sixel > iTerm2 > half-block (fallback).
//!
//! Protocol detection tries [`Picker::from_query_stdio`] first (full terminal
//! query) and falls back to environment-variable heuristics when the query
//! fails (e.g. because the event stream is already running).

use std::sync::Mutex;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::StatefulWidget;
use ratatui_image::StatefulImage;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::render::renderable::Renderable;

/// Embedded PNG logo image data (compile-time).
const LOGO_PNG: &[u8] = include_bytes!("../../../assets/savfox.png");

// ---------------------------------------------------------------------------
// Protocol detection helpers
// ---------------------------------------------------------------------------

/// Heuristically detect the best graphics protocol from well-known
/// environment variables.  This is a fast, non-blocking alternative to
/// [`Picker::from_query_stdio`] for terminals that advertise their identity.
fn detect_protocol_from_env() -> Option<ratatui_image::picker::ProtocolType> {
    use ratatui_image::picker::ProtocolType;

    // Kitty – native Kitty terminal
    if std::env::var_os("KITTY_WINDOW_ID").is_some() {
        return Some(ProtocolType::Kitty);
    }

    // TERM=xterm-kitty (common for Kitty)
    if let Ok(term) = std::env::var("TERM")
        && term.contains("xterm-kitty")
    {
        return Some(ProtocolType::Kitty);
    }

    // TERM_PROGRAM – many modern terminals set this.
    if let Ok(prog) = std::env::var("TERM_PROGRAM") {
        let p = prog.to_lowercase();
        if p.contains("kitty") || p.contains("wezterm") || p.contains("ghostty") {
            return Some(ProtocolType::Kitty);
        }
        if p.contains("iterm") {
            return Some(ProtocolType::Iterm2);
        }
        // foot supports Sixel
        if p.contains("foot") {
            return Some(ProtocolType::Sixel);
        }
    }

    // Windows Terminal (1.22+) supports Sixel.
    if std::env::var_os("WT_SESSION").is_some() {
        return Some(ProtocolType::Sixel);
    }

    None // unknown – caller should fall back to halfblocks
}

/// Build a [`Picker`] using the best detection strategy available.
fn create_picker() -> Picker {
    // Fast path – recognise common terminals via env vars.
    if let Some(protocol) = detect_protocol_from_env() {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(protocol);
        return picker;
    }

    // Slow path – query the terminal directly (may fail in raw-mode).
    if let Ok(picker) = Picker::from_query_stdio() {
        return picker;
    }

    Picker::halfblocks()
}

// ---------------------------------------------------------------------------
// LogoImage widget
// ---------------------------------------------------------------------------

/// A renderable logo image that uses the best available terminal graphics
/// protocol (Kitty, Sixel, iTerm2, or half-block fallback).
pub struct LogoImage {
    state: Mutex<StatefulProtocol>,
}

impl LogoImage {
    /// Load the embedded `savfox.png` and prepare it for rendering.
    ///
    /// Returns `None` if the image cannot be decoded.
    pub fn new() -> Option<Self> {
        let img = image::load_from_memory(LOGO_PNG).ok()?;
        let picker = create_picker();
        let state = picker.new_resize_protocol(img);
        Some(Self {
            state: Mutex::new(state),
        })
    }
}

impl Renderable for LogoImage {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            StatefulImage::new().render(area, buf, &mut *state);
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        // Match the ASCII logo height (5 rows).
        crate::ascii_logo::LOGO_HEIGHT
    }
}
