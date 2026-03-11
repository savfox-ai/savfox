use serde::{Deserialize, Serialize};
use tracing::debug;

// ─── Configuration ───────────────────────────────────────────────────────────

/// When in the processing pipeline a transformer runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TransformStage {
    /// Before the message is sent to the LLM / agent.
    PreProcess,
    /// After the LLM / agent has responded.
    PostProcess,
}

/// The kind of transformation to apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum TransformType {
    /// Prepend a fixed string.
    Prefix { value: String },
    /// Append a fixed string.
    Suffix { value: String },
    /// Replace all occurrences of `from` with `to`.
    Replace { from: String, to: String },
    /// Treat `template` as a format string where `{{input}}` is replaced by
    /// the message.
    Template { template: String },
    /// Strip HTML tags from the message.
    StripHtml,
    /// Truncate the message to at most `max_len` characters.
    Truncate { max_len: usize },
}

/// Persistent configuration for a transformer hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TransformerConfig {
    /// Human-readable name.
    pub name: String,
    /// Pipeline stage.
    pub stage: TransformStage,
    /// The transformation to apply.
    pub transform: TransformType,
}

// ─── Runtime hook ────────────────────────────────────────────────────────────

/// A message transformation hook.
///
/// Transformers modify message text in-place at a given pipeline stage.
#[derive(Debug)]
pub(crate) struct TransformerHook {
    name: String,
    stage: TransformStage,
    transform: TransformType,
}

impl TransformerHook {
    /// Create a `TransformerHook` from its configuration.
    pub(crate) fn from_config(config: &TransformerConfig) -> Self {
        Self {
            name: config.name.clone(),
            stage: config.stage,
            transform: config.transform.clone(),
        }
    }

    /// The pipeline stage this transformer targets.
    pub(crate) fn stage(&self) -> TransformStage {
        self.stage
    }

    /// The human-readable name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Apply the transformation to `input` and return the modified string.
    pub(crate) fn apply(&self, input: &str) -> String {
        let result = match &self.transform {
            TransformType::Prefix { value } => format!("{value}{input}"),
            TransformType::Suffix { value } => format!("{input}{value}"),
            TransformType::Replace { from, to } => input.replace(from, to),
            TransformType::Template { template } => template.replace("{{input}}", input),
            TransformType::StripHtml => strip_html_tags(input),
            TransformType::Truncate { max_len } => truncate(input, *max_len),
        };

        debug!(
            transformer = %self.name,
            stage = ?self.stage,
            input_len = input.len(),
            output_len = result.len(),
            "transformer applied"
        );

        result
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Naively strip HTML tags from a string.
///
/// This is intentionally simple (no full HTML parser dependency). It removes
/// anything between `<` and `>` and collapses runs of whitespace.
fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut inside_tag = false;

    for ch in input.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' if inside_tag => {
                inside_tag = false;
                // Insert a space to avoid words merging across tags.
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            _ if !inside_tag => out.push(ch),
            _ => {} // skip characters inside tags
        }
    }

    // Collapse runs of whitespace.
    let collapsed: String = out.split_whitespace().collect::<Vec<_>>().join(" ");

    collapsed.trim().to_owned()
}

/// Truncate a string to at most `max_len` characters (by `char` count).
fn truncate(input: &str, max_len: usize) -> String {
    if input.chars().count() <= max_len {
        input.to_owned()
    } else {
        input.chars().take(max_len).collect()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hook(transform: TransformType) -> TransformerHook {
        TransformerHook::from_config(&TransformerConfig {
            name: "test".into(),
            stage: TransformStage::PreProcess,
            transform,
        })
    }

    // ── Prefix ───────────────────────────────────────────────────────────

    #[test]
    fn prefix_prepends_value() {
        let hook = make_hook(TransformType::Prefix {
            value: "[system] ".into(),
        });
        assert_eq!(hook.apply("hello"), "[system] hello");
    }

    #[test]
    fn prefix_with_empty_input() {
        let hook = make_hook(TransformType::Prefix {
            value: ">> ".into(),
        });
        assert_eq!(hook.apply(""), ">> ");
    }

    // ── Suffix ───────────────────────────────────────────────────────────

    #[test]
    fn suffix_appends_value() {
        let hook = make_hook(TransformType::Suffix {
            value: " [end]".into(),
        });
        assert_eq!(hook.apply("hello"), "hello [end]");
    }

    #[test]
    fn suffix_with_empty_input() {
        let hook = make_hook(TransformType::Suffix { value: "!".into() });
        assert_eq!(hook.apply(""), "!");
    }

    // ── Replace ──────────────────────────────────────────────────────────

    #[test]
    fn replace_substitutes_all_occurrences() {
        let hook = make_hook(TransformType::Replace {
            from: "foo".into(),
            to: "bar".into(),
        });
        assert_eq!(hook.apply("foo and foo"), "bar and bar");
    }

    #[test]
    fn replace_no_match_returns_original() {
        let hook = make_hook(TransformType::Replace {
            from: "xyz".into(),
            to: "abc".into(),
        });
        assert_eq!(hook.apply("hello world"), "hello world");
    }

    #[test]
    fn replace_with_empty_to_removes_pattern() {
        let hook = make_hook(TransformType::Replace {
            from: "bad".into(),
            to: String::new(),
        });
        assert_eq!(hook.apply("this is bad stuff"), "this is  stuff");
    }

    // ── Template ─────────────────────────────────────────────────────────

    #[test]
    fn template_replaces_input_placeholder() {
        let hook = make_hook(TransformType::Template {
            template: "Context: {{input}}\nPlease summarise.".into(),
        });
        assert_eq!(
            hook.apply("some long text"),
            "Context: some long text\nPlease summarise."
        );
    }

    #[test]
    fn template_without_placeholder_returns_template_as_is() {
        let hook = make_hook(TransformType::Template {
            template: "static prompt".into(),
        });
        assert_eq!(hook.apply("anything"), "static prompt");
    }

    #[test]
    fn template_multiple_placeholders() {
        let hook = make_hook(TransformType::Template {
            template: "{{input}} -- repeated: {{input}}".into(),
        });
        assert_eq!(hook.apply("hi"), "hi -- repeated: hi");
    }

    // ── StripHtml ────────────────────────────────────────────────────────

    #[test]
    fn strip_html_removes_tags() {
        let hook = make_hook(TransformType::StripHtml);
        assert_eq!(hook.apply("<p>Hello <b>world</b></p>"), "Hello world");
    }

    #[test]
    fn strip_html_handles_nested_tags() {
        let hook = make_hook(TransformType::StripHtml);
        assert_eq!(
            hook.apply("<div><span class=\"x\">text</span></div>"),
            "text"
        );
    }

    #[test]
    fn strip_html_preserves_plain_text() {
        let hook = make_hook(TransformType::StripHtml);
        assert_eq!(hook.apply("no html here"), "no html here");
    }

    #[test]
    fn strip_html_empty_input() {
        let hook = make_hook(TransformType::StripHtml);
        assert_eq!(hook.apply(""), "");
    }

    #[test]
    fn strip_html_collapses_whitespace() {
        let hook = make_hook(TransformType::StripHtml);
        assert_eq!(hook.apply("<p>hello</p>   <p>world</p>"), "hello world");
    }

    // ── Truncate ─────────────────────────────────────────────────────────

    #[test]
    fn truncate_shortens_long_text() {
        let hook = make_hook(TransformType::Truncate { max_len: 5 });
        assert_eq!(hook.apply("hello world"), "hello");
    }

    #[test]
    fn truncate_preserves_short_text() {
        let hook = make_hook(TransformType::Truncate { max_len: 100 });
        assert_eq!(hook.apply("short"), "short");
    }

    #[test]
    fn truncate_exact_length() {
        let hook = make_hook(TransformType::Truncate { max_len: 5 });
        assert_eq!(hook.apply("hello"), "hello");
    }

    #[test]
    fn truncate_zero_length() {
        let hook = make_hook(TransformType::Truncate { max_len: 0 });
        assert_eq!(hook.apply("hello"), "");
    }

    #[test]
    fn truncate_handles_multibyte_chars() {
        let hook = make_hook(TransformType::Truncate { max_len: 3 });
        // Each emoji is one char.
        assert_eq!(
            hook.apply("\u{1F600}\u{1F601}\u{1F602}\u{1F603}"),
            "\u{1F600}\u{1F601}\u{1F602}"
        );
    }

    // ── Stage & metadata ─────────────────────────────────────────────────

    #[test]
    fn stage_is_preserved() {
        let hook = TransformerHook::from_config(&TransformerConfig {
            name: "post".into(),
            stage: TransformStage::PostProcess,
            transform: TransformType::StripHtml,
        });
        assert_eq!(hook.stage(), TransformStage::PostProcess);
        assert_eq!(hook.name(), "post");
    }

    // ── Config serialization ─────────────────────────────────────────────

    #[test]
    fn config_serialization_roundtrip() {
        let config = TransformerConfig {
            name: "add-prefix".into(),
            stage: TransformStage::PreProcess,
            transform: TransformType::Prefix { value: "> ".into() },
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: TransformerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "add-prefix");
        assert_eq!(deserialized.stage, TransformStage::PreProcess);
    }

    #[test]
    fn config_deserialize_replace() {
        let json = r#"{
            "name": "censor",
            "stage": "preProcess",
            "transform": { "type": "replace", "from": "bad", "to": "***" }
        }"#;
        let config: TransformerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "censor");
        match &config.transform {
            TransformType::Replace { from, to } => {
                assert_eq!(from, "bad");
                assert_eq!(to, "***");
            }
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[test]
    fn config_deserialize_truncate() {
        let json = r#"{
            "name": "limit",
            "stage": "postProcess",
            "transform": { "type": "truncate", "max_len": 280 }
        }"#;
        let config: TransformerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.stage, TransformStage::PostProcess);
        match &config.transform {
            TransformType::Truncate { max_len } => assert_eq!(*max_len, 280),
            other => panic!("expected Truncate, got {other:?}"),
        }
    }
}
