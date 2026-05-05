use std::collections::HashSet;
use std::io::{self, Write};
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::Value;
use tracing_subscriber::fmt::MakeWriter;

const DEFAULT_SECRET_ENV_VARS: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "DEEPGRAM_API_KEY",
    "GROQ_API_KEY",
    "SLACK_BOT_TOKEN",
    "SLACK_SIGNING_SECRET",
    "DISCORD_TOKEN",
    "TELEGRAM_BOT_TOKEN",
    "SAVFOX_GATEWAY_TOKEN",
    "SAVFOX_TOKEN",
];

static SK_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsk-[A-Za-z0-9_\-]{10,}\b").expect("valid regex"));
static XAI_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bxai-[A-Za-z0-9_\-]{10,}\b").expect("valid regex"));
static GITHUB_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgh[pousr]_[A-Za-z0-9_\-]{10,}\b").expect("valid regex"));
static SLACK_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bxox[a-z]-[A-Za-z0-9\-]{10,}\b").expect("valid regex"));
static GOOGLE_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bAIza[0-9A-Za-z_\-]{20,}\b").expect("valid regex"));
static AWS_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b").expect("valid regex"));
static BEARER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(Bearer)\s+([A-Za-z0-9._\-]{10,})\b").expect("valid regex")
});
static JSON_SECRET_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)("[^"]*(?:api[_-]?key|token|secret|password|passwd|credential)[^"]*"\s*:\s*")([^"]+)(")"#,
    )
    .expect("valid regex")
});
static KV_SECRET_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b([A-Za-z0-9_.\-]*(?:api[_-]?key|token|secret|password|passwd|credential)[A-Za-z0-9_.\-]*)\b\s*([:=])\s*([^\s,;]+)",
    )
    .expect("valid regex")
});
static SECRET_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(api[_-]?key|token|secret|password|passwd|credential)").expect("valid regex")
});

static CUSTOM_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    std::env::var("SAVFOX_REDACTION_PATTERNS")
        .ok()
        .map(|raw| {
            raw.split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .filter_map(|pat| Regex::new(pat).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
});

static SECRET_ENV_VALUES: LazyLock<Vec<String>> = LazyLock::new(collect_secret_env_values);

fn redaction_enabled() -> bool {
    let mode = std::env::var("SAVFOX_REDACTION_MODE")
        .unwrap_or_else(|_| "on".to_owned())
        .to_ascii_lowercase();
    if mode == "off" || mode == "false" || mode == "0" {
        return false;
    }

    match std::env::var("SAVFOX_REDACTION_ENABLED") {
        Ok(v) => {
            let value = v.trim().to_ascii_lowercase();
            !(value == "0" || value == "false" || value == "off")
        }
        Err(_) => true,
    }
}

fn collect_secret_env_values() -> Vec<String> {
    let mut names: HashSet<String> = DEFAULT_SECRET_ENV_VARS
        .iter()
        .map(|s| (*s).to_owned())
        .collect();

    if let Ok(extra) = std::env::var("SAVFOX_REDACTION_SECRET_ENVS") {
        for name in extra.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            names.insert(name.to_owned());
        }
    }

    for (key, _) in std::env::vars() {
        let upper = key.to_ascii_uppercase();
        if upper.contains("API_KEY")
            || upper.contains("TOKEN")
            || upper.contains("SECRET")
            || upper.contains("PASSWORD")
            || upper.contains("PASSWD")
        {
            names.insert(key);
        }
    }

    let mut values = Vec::new();
    for name in names {
        if let Ok(value) = std::env::var(&name) {
            let trimmed = value.trim();
            if trimmed.len() >= 6 {
                values.push(trimmed.to_owned());
            }
        }
    }

    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    values.dedup();
    values
}

fn mask_secret(secret: &str) -> String {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return "***".to_owned();
    }
    if trimmed.contains("...") {
        return trimmed.to_owned();
    }

    let suffix: String = trimmed
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let lower = trimmed.to_ascii_lowercase();

    if lower.starts_with("sk-") {
        format!("sk-...{suffix}")
    } else if lower.starts_with("xai-") {
        format!("xai-...{suffix}")
    } else if lower.starts_with("ghp_")
        || lower.starts_with("gho_")
        || lower.starts_with("ghu_")
        || lower.starts_with("ghs_")
        || lower.starts_with("ghr_")
    {
        format!("gh...{suffix}")
    } else if lower.starts_with("xox") {
        format!("xox...{suffix}")
    } else if lower.starts_with("aiza") {
        format!("AIza...{suffix}")
    } else if trimmed.len() <= 8 {
        "***".to_owned()
    } else {
        format!("***...{suffix}")
    }
}

fn redact_with_regex(input: String, regex: &Regex) -> String {
    regex
        .replace_all(&input, |caps: &Captures<'_>| {
            let token = caps.get(0).map_or("", |m| m.as_str());
            mask_secret(token)
        })
        .into_owned()
}

fn looks_like_secret_key(key: &str) -> bool {
    SECRET_KEY_RE.is_match(key)
}

pub fn redact_text(input: &str) -> String {
    if !redaction_enabled() || input.is_empty() {
        return input.to_owned();
    }

    let mut output = input.to_owned();

    output = JSON_SECRET_FIELD_RE
        .replace_all(&output, |caps: &Captures<'_>| {
            let prefix = caps.get(1).map_or("", |m| m.as_str());
            let value = caps.get(2).map_or("", |m| m.as_str());
            let suffix = caps.get(3).map_or("", |m| m.as_str());
            format!("{prefix}{}{suffix}", mask_secret(value))
        })
        .into_owned();

    output = KV_SECRET_FIELD_RE
        .replace_all(&output, |caps: &Captures<'_>| {
            let key = caps.get(1).map_or("", |m| m.as_str());
            let sep = caps.get(2).map_or("=", |m| m.as_str());
            let value = caps.get(3).map_or("", |m| m.as_str());
            format!("{key}{sep}{}", mask_secret(value))
        })
        .into_owned();

    output = BEARER_RE
        .replace_all(&output, |caps: &Captures<'_>| {
            let scheme = caps.get(1).map_or("Bearer", |m| m.as_str());
            let token = caps.get(2).map_or("", |m| m.as_str());
            format!("{scheme} {}", mask_secret(token))
        })
        .into_owned();

    output = redact_with_regex(output, &SK_TOKEN_RE);
    output = redact_with_regex(output, &XAI_TOKEN_RE);
    output = redact_with_regex(output, &GITHUB_TOKEN_RE);
    output = redact_with_regex(output, &SLACK_TOKEN_RE);
    output = redact_with_regex(output, &GOOGLE_KEY_RE);
    output = redact_with_regex(output, &AWS_KEY_RE);

    for value in SECRET_ENV_VALUES.iter() {
        if output.contains(value) {
            output = output.replace(value, &mask_secret(value));
        }
    }

    for pattern in CUSTOM_PATTERNS.iter() {
        output = pattern
            .replace_all(&output, |caps: &Captures<'_>| {
                let matched = caps.get(0).map_or("", |m| m.as_str());
                mask_secret(matched)
            })
            .into_owned();
    }

    output
}

#[must_use]
pub fn redact_json(value: Value) -> Value {
    let mut out = value;
    redact_json_in_place(&mut out);
    out
}

pub fn redact_json_in_place(value: &mut Value) {
    match value {
        Value::String(text) => {
            *text = redact_text(text);
        }
        Value::Array(items) => {
            for item in items {
                redact_json_in_place(item);
            }
        }
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                if looks_like_secret_key(key) {
                    match val {
                        Value::String(text) => *text = mask_secret(text),
                        _ => redact_json_in_place(val),
                    }
                } else {
                    redact_json_in_place(val);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Writer that runs every line of output through [`redact_text`] before
/// forwarding it to the wrapped [`std::io::Write`] sink.
///
/// Buffers a partial line internally so a `write_str` that straddles a newline
/// does not accidentally split a secret across two redaction calls.
pub struct RedactingWriter<W: Write> {
    inner: W,
    pending: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            pending: Vec::with_capacity(256),
        }
    }

    fn flush_complete_lines(&mut self) -> io::Result<()> {
        // Find the last newline; everything up to and including it is flushed.
        if let Some(idx) = self.pending.iter().rposition(|b| *b == b'\n') {
            // Take through the newline.
            let mut tail = self.pending.split_off(idx + 1);
            // `pending` now holds the complete-line region including the trailing
            // '\n'. `tail` holds the leftover partial line.
            std::mem::swap(&mut self.pending, &mut tail);
            // tail is the text we want to emit. Decode lossily for redaction
            // (logs are conventionally UTF-8).
            let text = String::from_utf8_lossy(&tail).into_owned();
            let redacted = redact_text(&text);
            self.inner.write_all(redacted.as_bytes())?;
        }
        Ok(())
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        self.flush_complete_lines()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            let text = String::from_utf8_lossy(&self.pending).into_owned();
            let redacted = redact_text(&text);
            self.inner.write_all(redacted.as_bytes())?;
            self.pending.clear();
        }
        self.inner.flush()
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// `MakeWriter` implementation that builds a fresh [`RedactingWriter`] over
/// stderr per log event. Plug into `tracing_subscriber::fmt::layer()` via
/// `.with_writer(RedactingStderr)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct RedactingStderr;

impl<'a> MakeWriter<'a> for RedactingStderr {
    type Writer = RedactingWriter<io::Stderr>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter::new(io::stderr())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn masks_openai_key_with_last4() {
        let input = "token=sk-abcdefghijklmnopqrstuvwxyz123456";
        let output = redact_text(input);
        assert!(output.contains("sk-...3456"));
        assert!(!output.contains("sk-abcdefghijkl"));
    }

    #[test]
    fn redacts_bearer_tokens() {
        let input = "Authorization: Bearer abcdefghijklmnopqrstuv";
        let output = redact_text(input);
        assert!(output.contains("Bearer ***...stuv"));
    }

    #[test]
    fn redacts_json_secret_fields() {
        let mut value = json!({
            "api_key": "xai-abcdefghijklmnopqrstuvwxyz1234",
            "nested": {
                "password": "supersecret-password",
                "safe": "hello"
            }
        });
        redact_json_in_place(&mut value);
        assert_eq!(value["api_key"], "xai-...1234");
        assert_eq!(value["nested"]["password"], "***...word");
        assert_eq!(value["nested"]["safe"], "hello");
    }

    #[test]
    fn safe_text_round_trips_unchanged() {
        let inputs = [
            "hello world",
            "the quick brown fox jumps over the lazy dog",
            "user logged in: alice, request_id=42",
            "{\"foo\":1,\"bar\":\"baz\"}",
        ];
        for text in inputs {
            assert_eq!(redact_text(text), text, "input was: {text}");
        }
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(redact_text(""), "");
    }

    #[test]
    fn redacting_writer_emits_only_complete_lines_and_redacts() {
        // The buffered writer should hold partial lines until '\n' arrives.
        let buf: Vec<u8> = Vec::new();
        let mut w = RedactingWriter::new(buf);
        w.write_all(b"hello sk-abcdefghijklmnopqrstuvwxyz1234")
            .unwrap();
        // No newline yet — nothing should be in the inner buffer.
        assert!(w.inner.is_empty());
        w.write_all(b" continued\n").unwrap();
        // Now the line is complete and should be redacted.
        let written = String::from_utf8(w.inner.clone()).unwrap();
        assert!(written.ends_with(" continued\n"));
        assert!(written.contains("sk-...1234"));
        assert!(!written.contains("sk-abcdefghijkl"));
    }

    #[test]
    fn redacting_writer_flush_emits_partial_line() {
        let buf: Vec<u8> = Vec::new();
        let mut w = RedactingWriter::new(buf);
        w.write_all(b"trailing fragment").unwrap();
        assert!(w.inner.is_empty());
        w.flush().unwrap();
        assert_eq!(w.inner, b"trailing fragment");
    }
}
