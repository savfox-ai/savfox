//! Shared UI constants for layout and alignment within the TUI.
//!
//! Some values adapt to the terminal width for a responsive layout.

/// Width (in terminal columns) reserved for the left gutter/prefix used by
/// live cells and aligned widgets.
///
/// Semantics:
/// - Chat composer reserves this many columns for the left border + padding.
/// - Status indicator lines begin with this many spaces for alignment.
/// - User history lines account for this many columns (e.g., "▌ ") when wrapping.
pub(crate) const LIVE_PREFIX_COLS: u16 = 2;
pub(crate) const FOOTER_INDENT_COLS: usize = LIVE_PREFIX_COLS as usize;

/// Responsive layout breakpoints.
///
/// These thresholds define how the TUI adapts its layout to different terminal
/// widths. Callers should use the helper functions below instead of comparing
/// against these values directly.
#[allow(dead_code)]
pub(crate) const NARROW_BREAKPOINT: u16 = 60;
#[allow(dead_code)]
pub(crate) const WIDE_BREAKPOINT: u16 = 120;

/// Returns the effective left-prefix column count, adapting to the terminal width.
///
/// On very narrow terminals (<60 cols), the prefix is reduced to 1 column to
/// preserve usable content width.
#[allow(dead_code)]
pub(crate) fn effective_prefix_cols(terminal_width: u16) -> u16 {
    if terminal_width < NARROW_BREAKPOINT {
        1
    } else {
        LIVE_PREFIX_COLS
    }
}

/// Returns the effective footer indent, adapting to the terminal width.
#[allow(dead_code)]
pub(crate) fn effective_footer_indent(terminal_width: u16) -> usize {
    effective_prefix_cols(terminal_width) as usize
}

/// Returns the effective output line limit for exec cells, adapting to width.
///
/// On wider terminals, more output can be shown without overwhelming the UI.
#[allow(dead_code)]
pub(crate) fn effective_output_line_limit(terminal_width: u16) -> usize {
    if terminal_width < NARROW_BREAKPOINT {
        3
    } else if terminal_width >= WIDE_BREAKPOINT {
        10
    } else {
        5
    }
}

/// Whether the footer context (model, provider, cwd) should be shown.
///
/// On very narrow terminals the context is hidden to save space for the
/// primary content.
#[allow(dead_code)]
pub(crate) fn should_show_footer_context(terminal_width: u16) -> bool {
    terminal_width >= NARROW_BREAKPOINT
}
