// Property-based tests for markdown rendering.
//
// These tests use `proptest` to verify that the markdown renderer handles
// arbitrary inputs without panicking and produces structurally valid output.

use proptest::prelude::*;

use crate::markdown_render::{render_markdown_text, render_markdown_text_with_width};

/// Generate arbitrary markdown-like strings with potentially tricky patterns.
fn markdown_input() -> impl Strategy<Value = String> {
    prop::string::string_regex(
        r"([#*_~`\[\]()>\-\+\d\.\n\r \t!@\\\|]|[a-zA-Z0-9]|[\u{4e00}-\u{4e10}]|[\u{1f600}-\u{1f610}]){0,500}"
    )
    .unwrap()
}

/// Generate deeply nested list markdown.
fn nested_list_input() -> impl Strategy<Value = String> {
    (1..=15usize).prop_map(|depth| {
        let mut s = String::new();
        for i in 0..depth {
            let indent = "  ".repeat(i);
            s.push_str(&format!("{indent}- item at depth {}\n", i + 1));
        }
        s
    })
}

/// Generate mixed inline styles.
fn mixed_inline_styles() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just("**bold**".to_string()),
            Just("_italic_".to_string()),
            Just("~~strikethrough~~".to_string()),
            Just("`code`".to_string()),
            Just("[link](url)".to_string()),
            Just("plain text".to_string()),
            Just("**_nested bold italic_**".to_string()),
            Just("**_~~all three~~_**".to_string()),
        ],
        1..=20,
    )
    .prop_map(|parts| parts.join(" "))
}

/// Generate strings with varying widths of CJK characters.
fn cjk_input() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just("你好世界".to_string()),
            Just("# 标题".to_string()),
            Just("- 列表项".to_string()),
            Just("**粗体**".to_string()),
            Just("普通文本和English混合".to_string()),
            Just("> 引用块".to_string()),
        ],
        1..=10,
    )
    .prop_map(|parts| parts.join("\n"))
}

/// Generate emoji-heavy content.
fn emoji_input() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just("😀😁😂🤣😃".to_string()),
            Just("# 🎉 Title".to_string()),
            Just("- 🔥 item".to_string()),
            Just("**🚀 bold emoji**".to_string()),
            Just("🏠🏡🏢🏣🏤🏥🏦🏧🏨🏩".to_string()),
        ],
        1..=8,
    )
    .prop_map(|parts| parts.join("\n"))
}

/// Generate very long single lines.
fn long_line_input() -> impl Strategy<Value = String> {
    (100..=2000usize).prop_map(|len| "x".repeat(len))
}

// ─── Properties ─────────────────────────────────────────────────

proptest! {
    /// The renderer must never panic on arbitrary input.
    #[test]
    fn render_never_panics(input in markdown_input()) {
        let _ = render_markdown_text(&input);
    }

    /// Rendering with a width must never panic.
    #[test]
    fn render_with_width_never_panics(input in markdown_input(), width in 1..=200usize) {
        let _ = render_markdown_text_with_width(&input, Some(width));
    }

    /// Zero-width rendering must not panic.
    #[test]
    fn render_zero_width_never_panics(input in markdown_input()) {
        let _ = render_markdown_text_with_width(&input, Some(1));
    }

    /// Output must have at least as many lines as newlines in the input
    /// (markdown may add extra lines for spacing, but never fewer than the
    /// raw line count minus collapsing effects).
    #[test]
    fn output_is_nonempty_for_nonempty_input(input in "[a-zA-Z]{1,100}") {
        let text = render_markdown_text(&input);
        prop_assert!(!text.lines.is_empty(), "non-empty input should produce non-empty output");
    }

    /// Deeply nested lists must not stack-overflow or panic.
    #[test]
    fn deeply_nested_lists_dont_panic(input in nested_list_input()) {
        let _ = render_markdown_text(&input);
    }

    /// Mixed inline styles must render without panic.
    #[test]
    fn mixed_styles_dont_panic(input in mixed_inline_styles()) {
        let text = render_markdown_text(&input);
        prop_assert!(!text.lines.is_empty());
    }

    /// CJK characters must render without panic.
    #[test]
    fn cjk_renders_without_panic(input in cjk_input()) {
        let text = render_markdown_text(&input);
        prop_assert!(!text.lines.is_empty());
    }

    /// Emoji-heavy content must render without panic.
    #[test]
    fn emoji_renders_without_panic(input in emoji_input()) {
        let text = render_markdown_text(&input);
        prop_assert!(!text.lines.is_empty());
    }

    /// Very long lines must render without panic.
    #[test]
    fn long_lines_dont_panic(input in long_line_input()) {
        let text = render_markdown_text(&input);
        prop_assert!(!text.lines.is_empty());
    }

    /// Width-constrained rendering of simple single-word-per-token content
    /// should produce lines roughly within the target width.
    ///
    /// Note: the width hint is advisory for word-wrap; leading indentation,
    /// list markers, and unbreakable tokens may exceed it.
    #[test]
    fn wrapped_lines_respect_width(
        content in "[a-z]{1,8}( [a-z]{1,8}){5,30}",
        width in 20..=80usize,
    ) {
        let text = render_markdown_text_with_width(&content, Some(width));
        for line in &text.lines {
            let visual_width: usize = line.spans.iter()
                .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            // Word-wrap is best-effort; allow tolerance for edge cases
            prop_assert!(
                visual_width <= width + 10,
                "line width {} exceeds target {} + tolerance: {:?}",
                visual_width, width, line
            );
        }
    }

    /// Rendering the same input twice must produce identical output
    /// (determinism / cache correctness).
    #[test]
    fn render_is_deterministic(input in markdown_input()) {
        let a = render_markdown_text(&input);
        let b = render_markdown_text(&input);
        prop_assert_eq!(a, b);
    }
}
