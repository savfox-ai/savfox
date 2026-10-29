//! Response chunking  - splits long messages for platforms with character limits.

use serde::{Deserialize, Serialize};

/// Platform-specific character limits
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PlatformLimit {
    /// Discord: 2000 characters
    Discord,
    /// Telegram: 4096 characters
    Telegram,
    /// Slack: 40000 characters
    Slack,
    /// WhatsApp: 65536 characters
    WhatsApp,
    /// Default: 4096 characters
    Default,
}

impl PlatformLimit {
    #[must_use]
    pub fn max_chars(self) -> usize {
        match self {
            Self::Discord => 2000,
            Self::Telegram => 4096,
            Self::Slack => 40000,
            Self::WhatsApp => 65536,
            Self::Default => 4096,
        }
    }

    #[must_use]
    pub fn from_channel(channel: &str) -> Self {
        let platform = channel
            .split_once(':')
            .map(|(platform, _)| platform)
            .unwrap_or(channel)
            .trim()
            .to_ascii_lowercase();
        match platform.as_str() {
            "discord" => Self::Discord,
            "telegram" => Self::Telegram,
            "slack" => Self::Slack,
            "whatsapp" => Self::WhatsApp,
            _ => Self::Default,
        }
    }
}

/// A chunk of a split message
#[derive(Debug, Clone)]
pub struct MessageChunk {
    pub text: String,
    pub index: usize,
    pub total: usize,
}

enum Segment {
    Text(String),
    Code(String),
}

#[must_use]
pub fn chunk_message_for_channel(
    text: &str,
    channel: &str,
    max_override: Option<usize>,
    overlap: usize,
) -> Vec<MessageChunk> {
    let platform = PlatformLimit::from_channel(channel);
    let max_chars = max_override
        .filter(|value| *value > 0)
        .unwrap_or_else(|| platform.max_chars());
    chunk_message_with_options(text, max_chars, overlap)
}

/// Split a long message into chunks respecting platform limits.
/// Tries to split at paragraph/sentence boundaries.
#[must_use]
pub fn chunk_message(text: &str, platform: PlatformLimit) -> Vec<MessageChunk> {
    chunk_message_with_options(text, platform.max_chars(), 0)
}

/// Split a long message with explicit max length and optional overlap.
#[must_use]
pub fn chunk_message_with_options(
    text: &str,
    max_chars: usize,
    overlap_chars: usize,
) -> Vec<MessageChunk> {
    let max = max_chars.max(1);
    if char_len(text) <= max {
        return vec![MessageChunk {
            text: text.to_owned(),
            index: 1,
            total: 1,
        }];
    }

    // The total can gain digits when reserving space creates another chunk.
    // Re-split only at digit boundaries until every final page marker fits.
    let mut number_digits = 1;
    let mut numbered = true;
    loop {
        let marker_chars = if numbered { 5 + 2 * number_digits } else { 0 };
        if marker_chars >= max {
            // A tiny custom limit must still make forward progress. Numbering
            // is optional when even one source character would not fit.
            numbered = false;
            continue;
        }
        let chunks = split_message_body(text, max - marker_chars, overlap_chars);
        let total = chunks.len();
        let total_digits = total.to_string().len();
        if numbered && total_digits > number_digits {
            number_digits = total_digits;
            continue;
        }
        return chunks
            .into_iter()
            .enumerate()
            .map(|(i, text)| MessageChunk {
                text: if numbered && total > 1 {
                    format!("{text}\n\n({}/{})", i + 1, total)
                } else {
                    text
                },
                index: i + 1,
                total,
            })
            .collect();
    }
}

/// Split content within the budget left after numbering. Reserve overlap and
/// its separator before producing pieces, so a full-size piece never overflows
/// when the previous message's tail is prepended. Clamp excessive overlap to
/// half the body budget to keep useful progress on every chunk.
fn split_message_body(text: &str, max: usize, overlap_chars: usize) -> Vec<String> {
    let overlap = overlap_chars.min(max.saturating_sub(1) / 2);
    let overlap_budget = if overlap > 0 { overlap + 1 } else { 0 };
    let piece_max = max - overlap_budget;
    let mut pieces = Vec::new();
    for segment in split_segments(text) {
        let mut segment_pieces = match segment {
            Segment::Text(value) => split_text_segment(&value, piece_max),
            Segment::Code(value) => split_code_segment(&value, piece_max),
        };
        pieces.append(&mut segment_pieces);
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for piece in pieces {
        if char_len(&current) + char_len(&piece) > max
            || (current.contains("```") && piece.contains("```"))
        {
            let previous = std::mem::take(&mut current);
            // Repeating a closing/opening code fence as context would unbalance
            // the next message. Keep fenced segments independently renderable.
            if overlap > 0 && !previous.contains("```") && !piece.contains("```") {
                current.push_str(&tail_chars(&previous, overlap));
                current.push('\n');
            }
            chunks.push(previous);
        }
        current.push_str(&piece);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn split_segments(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut cursor = 0usize;

    while let Some(start) = text[cursor..].find("```") {
        let absolute_start = cursor + start;
        if absolute_start > cursor {
            out.push(Segment::Text(text[cursor..absolute_start].to_string()));
        }

        let code_start = absolute_start;
        let code_end = text[code_start + 3..]
            .find("```")
            .map(|rel| code_start + 3 + rel + 3);
        if let Some(end) = code_end {
            out.push(Segment::Code(text[code_start..end].to_string()));
            cursor = end;
        } else {
            out.push(Segment::Text(text[code_start..].to_string()));
            cursor = text.len();
            break;
        }
    }

    if cursor < text.len() {
        out.push(Segment::Text(text[cursor..].to_string()));
    }

    out
}

fn split_text_segment(text: &str, max: usize) -> Vec<String> {
    if char_len(text) <= max {
        return vec![text.to_owned()];
    }
    let mut out = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if char_len(remaining) <= max {
            out.push(remaining.to_owned());
            break;
        }
        let slice = prefix_by_chars(remaining, max);
        let break_at = find_break_point(slice);
        let (chunk, rest) = remaining.split_at(break_at);
        out.push(chunk.to_owned());
        remaining = rest;
    }

    out
}

fn split_code_segment(code_block: &str, max: usize) -> Vec<String> {
    if char_len(code_block) <= max {
        return vec![code_block.to_owned()];
    }

    let Some(first_line_end) = code_block.find('\n') else {
        return split_text_segment(code_block, max);
    };
    if !code_block.starts_with("```")
        || !code_block.ends_with("```")
        || first_line_end + 3 >= code_block.len()
    {
        return split_text_segment(code_block, max);
    }

    let header = &code_block[..=first_line_end];
    let footer = "```";
    let content = &code_block[first_line_end + 1..code_block.len() - 3];
    // Every synthetic close has its own newline. Keeping that newline separate
    // preserves blank lines, trailing newlines, and long-line content verbatim.
    let available = max.saturating_sub(char_len(header) + char_len(footer) + 1);
    if available == 0 {
        return split_text_segment(code_block, max);
    }
    split_text_segment(content, available)
        .into_iter()
        .map(|piece| format!("{header}{piece}\n{footer}"))
        .collect()
}

/// Find the best break point in text, preferring paragraph > sentence > word boundaries
fn find_break_point(text: &str) -> usize {
    // Try paragraph break (double newline)
    if let Some(pos) = text.rfind("\n\n")
        && pos > text.len() / 2
    {
        return pos + 2;
    }

    // Try single newline
    if let Some(pos) = text.rfind('\n')
        && pos > text.len() / 2
    {
        return pos + 1;
    }

    // Try sentence break (. ! ?)
    for sep in [". ", "! ", "? "] {
        if let Some(pos) = text.rfind(sep)
            && pos > text.len() / 3
        {
            return pos + sep.len();
        }
    }

    // Try word break (space)
    if let Some(pos) = text.rfind(' ') {
        return pos + 1;
    }

    // Hard break at max
    text.len()
}

fn tail_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    text.chars().skip(count - max_chars).collect()
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn nth_char_boundary(text: &str, max_chars: usize) -> usize {
    text.char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len())
}

fn prefix_by_chars(text: &str, max_chars: usize) -> &str {
    let end = nth_char_boundary(text, max_chars);
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_message() {
        let chunks = chunk_message("hello", PlatformLimit::Discord);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "hello");
    }

    #[test]
    fn test_long_message() {
        let long = "a ".repeat(1500);
        let chunks = chunk_message(&long, PlatformLimit::Discord);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.text.chars().count() <= 2000);
        }
    }

    #[test]
    fn preserves_code_fences_across_chunks() {
        let code = "```rust\n".to_owned() + &"let x = 1;\n".repeat(900) + "```";
        let chunks = chunk_message_for_channel(&code, "discord:1", None, 0);
        assert!(chunks.len() > 1);
        for chunk in chunks {
            assert!(chunk.text.contains("```"));
            assert!(chunk.text.chars().count() <= 2000);
        }
    }

    #[test]
    fn supports_overlap_context() {
        let text = "paragraph one.\n\nparagraph two.\n\nparagraph three.".repeat(120);
        let chunks = chunk_message_for_channel(&text, "telegram:1", Some(600), 32);
        assert!(chunks.len() > 1);
        for chunk in chunks.iter().skip(1) {
            assert!(!chunk.text.is_empty());
        }
    }

    #[test]
    fn split_text_segment_preserves_markdown_content() {
        let text =
            "- item 1\n- item 2 with `inline code`\n\n[link](https://example.com) and **bold**";
        let pieces = split_text_segment(text, 20);
        assert!(pieces.len() > 1);
        assert_eq!(pieces.concat(), text);
    }

    #[test]
    fn chunking_handles_unicode_boundaries() {
        let text = "你好，世界。".repeat(700);
        let chunks = chunk_message_for_channel(&text, "discord:1", Some(300), 0);
        assert!(chunks.len() > 1);
        for chunk in chunks {
            assert!(chunk.text.chars().count() <= 300);
        }
    }

    fn without_marker(chunk: &MessageChunk) -> &str {
        let marker = format!("\n\n({}/{})", chunk.index, chunk.total);
        chunk.text.strip_suffix(&marker).unwrap_or(&chunk.text)
    }

    #[test]
    fn numbering_fits_ascii_unicode_and_two_digit_totals_without_losing_body() {
        for text in ["abcde ".repeat(700), "中文🙂\n".repeat(700)] {
            let chunks = chunk_message_with_options(&text, 64, 0);
            assert!(chunks.len() >= 10);
            assert!(chunks.iter().all(|chunk| char_len(&chunk.text) <= 64));
            for (index, chunk) in chunks.iter().enumerate() {
                assert_eq!(chunk.index, index + 1);
                assert_eq!(chunk.total, chunks.len());
                assert!(
                    chunk
                        .text
                        .ends_with(&format!("\n\n({}/{})", index + 1, chunks.len()))
                );
            }
            assert_eq!(chunks.iter().map(without_marker).collect::<String>(), text);
        }
    }

    #[test]
    fn overlap_and_numbering_share_the_platform_budget() {
        let text = "中文🙂 alpha beta.\n".repeat(120);
        for requested_overlap in [16, usize::MAX] {
            let chunks = chunk_message_with_options(&text, 64, requested_overlap);
            assert!(chunks.len() >= 10);
            let marker_chars = 5 + 2 * chunks.len().to_string().len();
            let overlap = requested_overlap.min((64 - marker_chars - 1) / 2);
            let mut reconstructed = String::new();
            let mut previous = "";
            for (index, chunk) in chunks.iter().enumerate() {
                assert!(char_len(&chunk.text) <= 64);
                let body = without_marker(chunk);
                if index == 0 {
                    reconstructed.push_str(body);
                } else {
                    let context = format!("{}\n", tail_chars(previous, overlap));
                    reconstructed.push_str(body.strip_prefix(&context).expect("overlap prefix"));
                }
                previous = body;
            }
            assert_eq!(reconstructed, text);
        }
    }

    #[test]
    fn tiny_limits_preserve_source_and_omit_unfittable_page_markers() {
        let text = "中文🙂abcdefghijklmnopqrstuvwxyz";
        for max in 1..=10 {
            let chunks = chunk_message_with_options(text, max, 0);
            assert!(chunks.iter().all(|chunk| char_len(&chunk.text) <= max));
            assert_eq!(chunks.iter().map(without_marker).collect::<String>(), text);
        }
    }

    #[test]
    fn fenced_code_preserves_blank_lines_long_lines_and_trailing_newlines() {
        let body = "let x = 1;\n\n".to_owned() + &"中文🙂".repeat(400) + "\n\nlast();\n";
        let code = format!("```rust\n{body}```");
        for overlap in [0, 16] {
            let chunks = chunk_message_with_options(&code, 128, overlap);
            assert!(chunks.len() >= 10);
            let restored = chunks
                .iter()
                .map(|chunk| {
                    assert!(char_len(&chunk.text) <= 128);
                    let content = without_marker(chunk);
                    assert_eq!(content.matches("```").count(), 2);
                    content
                        .strip_prefix("```rust\n")
                        .unwrap()
                        .strip_suffix("\n```")
                        .unwrap()
                })
                .collect::<String>();
            assert_eq!(restored, body);
        }
    }
}
