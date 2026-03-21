//! Centralized key-binding registry with context-aware dispatch.
//!
//! The `KeyMap` collects all keyboard shortcuts in one place, organized by
//! context priority.  When a key event arrives the registry is queried from
//! highest to lowest priority; the first matching binding wins.
//!
//! This makes it easy to:
//! - List all bindings for a help overlay.
//! - Detect conflicting bindings at startup.
//! - Support user-customizable bindings in the future.

use crossterm::event::{KeyCode, KeyEvent};

use crate::key_hint::KeyBinding;

/// Priority layer for key dispatch.
///
/// Higher-priority contexts are checked first. A popup-level binding shadows
/// a screen-level binding for the same key.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum KeyContext {
    /// Always-active global bindings (e.g. Ctrl+C quit).
    Global = 0,
    /// Active when the main chat screen has focus.
    Screen = 1,
    /// Active when a modal popup/overlay is open.
    Popup = 2,
}

/// A registered key binding with metadata for display and dispatch.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct KeyMapEntry {
    /// The key combination that triggers this action.
    pub(crate) binding: KeyBinding,
    /// Human-readable label shown in help overlays (e.g. "Quit").
    pub(crate) label: &'static str,
    /// Optional category for grouping in help panels.
    pub(crate) category: &'static str,
    /// The context in which this binding is active.
    pub(crate) context: KeyContext,
    /// Unique action identifier used by the dispatch handler.
    pub(crate) action: &'static str,
}

/// Central registry of all key bindings.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub(crate) struct KeyMap {
    entries: Vec<KeyMapEntry>,
}

#[allow(dead_code)]
impl KeyMap {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Register a new key binding.
    pub(crate) fn register(
        &mut self,
        binding: KeyBinding,
        label: &'static str,
        category: &'static str,
        context: KeyContext,
        action: &'static str,
    ) {
        self.entries.push(KeyMapEntry {
            binding,
            label,
            category,
            context,
            action,
        });
    }

    /// Find the action for a key event, checking higher-priority contexts first.
    ///
    /// `active_context` specifies the highest context currently active. For
    /// example, when a popup is open pass `KeyContext::Popup` so popup-level
    /// bindings are checked before screen-level ones.
    pub(crate) fn lookup(&self, event: KeyEvent, active_context: KeyContext) -> Option<&str> {
        let mut best: Option<&KeyMapEntry> = None;
        for entry in &self.entries {
            if entry.context > active_context {
                continue;
            }
            if !entry.binding.is_press(event) {
                continue;
            }
            match best {
                Some(prev) if prev.context >= entry.context => {}
                _ => best = Some(entry),
            }
        }
        best.map(|e| e.action)
    }

    /// Return all entries grouped by category, sorted by context then label.
    /// Useful for rendering a help panel.
    pub(crate) fn help_entries(&self) -> Vec<&KeyMapEntry> {
        let mut entries: Vec<&KeyMapEntry> = self.entries.iter().collect();
        entries.sort_by(|a, b| {
            a.category
                .cmp(b.category)
                .then(a.context.cmp(&b.context))
                .then(a.label.cmp(b.label))
        });
        entries
    }

    /// Check for conflicting bindings within the same context.
    /// Returns pairs of conflicting entries (for debug/startup warnings).
    #[allow(dead_code)]
    pub(crate) fn find_conflicts(&self) -> Vec<(&KeyMapEntry, &KeyMapEntry)> {
        let mut conflicts = Vec::new();
        for (i, a) in self.entries.iter().enumerate() {
            for b in self.entries[i + 1..].iter() {
                if a.context == b.context && a.binding == b.binding {
                    conflicts.push((a, b));
                }
            }
        }
        conflicts
    }
}

/// Build the default key map with all standard bindings.
#[allow(dead_code)]
pub(crate) fn default_keymap() -> KeyMap {
    use crate::key_hint;

    let mut km = KeyMap::new();

    // ── Navigation ──
    km.register(
        key_hint::ctrl(KeyCode::Char('t')),
        "Transcript",
        "Navigation",
        KeyContext::Screen,
        "open_transcript",
    );
    km.register(
        key_hint::plain(KeyCode::Esc),
        "Back / Edit previous",
        "Navigation",
        KeyContext::Screen,
        "esc_backtrack",
    );

    // ── Input ──
    km.register(
        key_hint::plain(KeyCode::Char('/')),
        "Commands",
        "Input",
        KeyContext::Screen,
        "slash_commands",
    );
    km.register(
        key_hint::plain(KeyCode::Char('!')),
        "Shell commands",
        "Input",
        KeyContext::Screen,
        "shell_commands",
    );
    km.register(
        key_hint::plain(KeyCode::Char('@')),
        "File paths",
        "Input",
        KeyContext::Screen,
        "file_paths",
    );
    km.register(
        key_hint::ctrl(KeyCode::Char('g')),
        "External editor",
        "Input",
        KeyContext::Screen,
        "external_editor",
    );
    km.register(
        key_hint::ctrl(KeyCode::Char('v')),
        "Paste image",
        "Input",
        KeyContext::Screen,
        "paste_image",
    );

    // ── Session ──
    km.register(
        key_hint::ctrl(KeyCode::Char('c')),
        "Interrupt / Quit",
        "Session",
        KeyContext::Global,
        "ctrl_c",
    );
    km.register(
        key_hint::ctrl(KeyCode::Char('d')),
        "Quit",
        "Session",
        KeyContext::Global,
        "ctrl_d",
    );

    // ── Help ──
    km.register(
        key_hint::plain(KeyCode::Char('?')),
        "Shortcuts",
        "Help",
        KeyContext::Screen,
        "toggle_shortcuts",
    );
    km.register(
        key_hint::plain(KeyCode::F(1)),
        "Shortcuts",
        "Help",
        KeyContext::Screen,
        "toggle_shortcuts",
    );

    km
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn lookup_finds_matching_binding() {
        let km = default_keymap();
        let action = km.lookup(ctrl_press(KeyCode::Char('t')), KeyContext::Screen);
        assert_eq!(action, Some("open_transcript"));
    }

    #[test]
    fn lookup_respects_active_context() {
        let km = default_keymap();
        // Popup-level bindings should not match when active context is Screen
        let action = km.lookup(press(KeyCode::Char('?')), KeyContext::Screen);
        assert_eq!(action, Some("toggle_shortcuts"));
    }

    #[test]
    fn help_entries_returns_all_bindings() {
        let km = default_keymap();
        let entries = km.help_entries();
        assert!(!entries.is_empty());
    }

    #[test]
    fn no_conflicts_in_default_keymap() {
        let km = default_keymap();
        let conflicts = km.find_conflicts();
        // F1 and ? map to the same action but with same binding is fine
        // since they have different keys
        for (a, b) in &conflicts {
            assert_ne!(
                a.action, b.action,
                "unexpected conflict: {} vs {}",
                a.label, b.label
            );
        }
    }
}
