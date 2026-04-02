pub fn clamp_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        text.to_string()
    } else if max > 3 {
        format!("{}...", &text[..max - 3])
    } else {
        text[..max].to_string()
    }
}

pub fn truncate_text(text: &str, max: usize) -> TruncatedText {
    let total = text.chars().count();
    if total <= max {
        TruncatedText {
            text: text.to_string(),
            truncated: false,
            total,
        }
    } else {
        let truncated: String = text.chars().take(max).collect();
        TruncatedText {
            text: format!("{}...", truncated),
            truncated: true,
            total,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TruncatedText {
    pub text: String,
    pub truncated: bool,
    pub total: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDirection {
    Ltr,
    Rtl,
    Mixed,
    Neutral,
}

pub fn detect_text_direction(text: &str) -> TextDirection {
    let mut saw_ltr = false;
    let mut saw_rtl = false;

    for ch in text.chars() {
        if is_rtl_char(ch) {
            saw_rtl = true;
        } else if ch.is_alphabetic() {
            saw_ltr = true;
        }

        if saw_ltr && saw_rtl {
            return TextDirection::Mixed;
        }
    }

    match (saw_ltr, saw_rtl) {
        (true, false) => TextDirection::Ltr,
        (false, true) => TextDirection::Rtl,
        (true, true) => TextDirection::Mixed,
        (false, false) => TextDirection::Neutral,
    }
}

pub fn dir_attr_for_text(text: &str) -> &'static str {
    match detect_text_direction(text) {
        TextDirection::Rtl => "rtl",
        TextDirection::Ltr => "ltr",
        TextDirection::Mixed | TextDirection::Neutral => "auto",
    }
}

fn is_rtl_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0590..=0x05FF // Hebrew
            | 0x0600..=0x06FF // Arabic
            | 0x0750..=0x077F // Arabic Supplement
            | 0x08A0..=0x08FF // Arabic Extended-A
            | 0xFB1D..=0xFDFF // Hebrew/Arabic presentation forms
            | 0xFE70..=0xFEFF // Arabic presentation forms-B
            | 0x200F // Right-to-left mark
    )
}

pub fn strip_thinking_tags(text: &str) -> String {
    let mut result = text.to_string();

    let patterns = [
        ("<think|thinking>", ""),
        ("</think|thinking>", ""),
        ("<think", ""),
        ("</think", ""),
        ("<thinking>", ""),
        ("</thinking>", ""),
    ];

    for (pattern, replacement) in patterns {
        result = result.replace(pattern, replacement);
    }

    result
}

pub fn format_list(items: &[String]) -> String {
    items.join(", ")
}

pub fn format_list_with_limit(items: &[String], limit: usize) -> String {
    if items.len() <= limit {
        items.join(", ")
    } else {
        let displayed = &items[..limit];
        format!(
            "{}, ... and {} more",
            displayed.join(", "),
            items.len() - limit
        )
    }
}

pub fn parse_list(input: &str) -> Vec<String> {
    input
        .split([',', '\n'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn to_number(value: &str, fallback: i64) -> i64 {
    value.trim().parse().unwrap_or(fallback)
}

pub fn to_number_f64(value: &str, fallback: f64) -> f64 {
    value.trim().parse().unwrap_or(fallback)
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub fn format_cost(cost: f64) -> String {
    if cost >= 1.0 {
        format!("${:.2}", cost)
    } else if cost >= 0.01 {
        format!("${:.3}", cost)
    } else if cost >= 0.0001 {
        format!("${:.5}", cost)
    } else {
        format!("${:.7}", cost)
    }
}

pub fn format_number(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

pub fn humanize(s: &str) -> String {
    s.replace('_', " ")
        .split_whitespace()
        .map(capitalize_first)
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn shorten_path(path: &str, home: Option<&str>) -> String {
    if let Some(h) = home {
        if path.starts_with(h) {
            return path.replacen(h, "~", 1);
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::{TextDirection, detect_text_direction, dir_attr_for_text};

    #[test]
    fn detects_ltr_text() {
        assert_eq!(detect_text_direction("hello world"), TextDirection::Ltr);
    }

    #[test]
    fn detects_arabic_text_as_rtl() {
        assert_eq!(detect_text_direction("مرحبا بالعالم"), TextDirection::Rtl);
    }

    #[test]
    fn detects_hebrew_text_as_rtl() {
        assert_eq!(detect_text_direction("שלום עולם"), TextDirection::Rtl);
    }

    #[test]
    fn detects_mixed_ltr_rtl_text() {
        assert_eq!(detect_text_direction("hello مرحبا"), TextDirection::Mixed);
    }

    #[test]
    fn mixed_and_neutral_default_to_auto_direction() {
        assert_eq!(dir_attr_for_text("hello مرحبا"), "auto");
        assert_eq!(dir_attr_for_text("12345 ???"), "auto");
    }

    #[test]
    fn rtl_and_ltr_map_to_expected_dir_attr() {
        assert_eq!(dir_attr_for_text("hello world"), "ltr");
        assert_eq!(dir_attr_for_text("שלום"), "rtl");
    }
}
