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

    let mut pieces = Vec::new();
    for segment in split_segments(text) {
        let mut segment_pieces = match segment {
            Segment::Text(value) => split_text_segment(&value, max),
            Segment::Code(value) => split_code_segment(&value, max),
        };
        pieces.append(&mut segment_pieces);
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut previous_sent = String::new();

    for piece in pieces {
        if current.is_empty() {
            if overlap_chars > 0 && !previous_sent.is_empty() {
                let tail = tail_chars(&previous_sent, overlap_chars);
                if !tail.is_empty() {
                    current.push_str(&format!("{tail}\n"));
                }
            }
            current.push_str(piece.as_str());
            continue;
        }

        if char_len(&current) + char_len(&piece) <= max {
            current.push_str(piece.as_str());
            continue;
        }

        if !current.is_empty() {
            previous_sent = current.clone();
            chunks.push(previous_sent.clone());
        }
        current.clear();
        if overlap_chars > 0 && !previous_sent.is_empty() {
            let tail = tail_chars(&previous_sent, overlap_chars);
            if !tail.is_empty() {
                current.push_str(&format!("{tail}\n"));
            }
        }
        current.push_str(piece.as_str());
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        chunks.push(text.to_owned());
    }

    let total = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let t = if total > 1 {
                format!("{text}\n\n({}/{})", i + 1, total)
            } else {
                text
            };
            MessageChunk {
                text: t,
                index: i + 1,
                total,
            }
        })
        .collect()
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
    let available = max.saturating_sub(char_len(header) + char_len(footer) + 1);
    if available < 32 {
        return split_text_segment(code_block, max);
    }

    let mut out = Vec::new();
    let mut current = String::new();
    for line in content.lines() {
        let candidate = if current.is_empty() {
            line.to_owned()
        } else {
            format!("{current}\n{line}")
        };
        if char_len(&candidate) <= available {
            current = candidate;
            continue;
        }

        if !current.is_empty() {
            out.push(format!("{header}{current}\n{footer}"));
            current.clear();
        }

        if char_len(line) <= available {
            current.push_str(line);
            continue;
        }

        let mut tail = line;
        while !tail.is_empty() {
            let split_at = nth_char_boundary(tail, available);
            let (chunk, rest) = tail.split_at(split_at);
            out.push(format!("{header}{chunk}\n{footer}"));
            tail = rest;
        }
    }

    if !current.is_empty() {
        out.push(format!("{header}{current}\n{footer}"));
    }

    out
}

/// Find the best break point in text, preferring paragraph > sentence > word boundaries
fn find_break_point(text: &str) -> usize {
    // Try paragraph break (double newline)
    if let Some(pos) = text.rfind("\n\n")
        && pos > text.len() / 2 {
            return pos + 2;
        }

    // Try single newline
    if let Some(pos) = text.rfind('\n')
        && pos > text.len() / 2 {
            return pos + 1;
        }

    // Try sentence break (. ! ?)
    for sep in [". ", "! ", "? "] {
        if let Some(pos) = text.rfind(sep)
            && pos > text.len() / 3 {
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
            assert!(chunk.text.len() <= 2100); // allow for page indicator
        }
    }

    #[test]
    fn preserves_code_fences_across_chunks() {
        let code = "```rust\n".to_string() + &"let x = 1;\n".repeat(900) + "```";
        let chunks = chunk_message_for_channel(&code, "discord:1", None, 0);
        assert!(chunks.len() > 1);
        for chunk in chunks {
            assert!(chunk.text.contains("```"));
            assert!(chunk.text.len() <= 2100);
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
            assert!(chunk.text.chars().count() <= 340);
        }
    }
}
