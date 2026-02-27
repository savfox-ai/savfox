use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::debug;

// ─── Configuration ───────────────────────────────────────────────────────────

/// When in the pipeline the validator runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ValidationStage {
    /// Validate inbound user messages before processing.
    Input,
    /// Validate outbound responses before delivery.
    Output,
}

/// A single validation rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum ValidationRule {
    /// Message must not exceed this character count.
    MaxLength { max: usize },
    /// Message must be at least this many characters.
    MinLength { min: usize },
    /// Message must match the given regex pattern.
    RegexMatch { pattern: String },
    /// Message must *not* match the given regex pattern.
    RegexReject { pattern: String },
    /// Message must contain *all* of the specified strings.
    ContainsRequired { required: Vec<String> },
    /// Message must *not* contain any of the specified keywords (case-insensitive).
    BlockKeywords { keywords: Vec<String> },
    /// Message must be valid JSON conforming to the given JSON Schema string.
    /// (Stored as an opaque string; full schema validation is a future extension.)
    JsonSchema { schema: String },
}

/// Persistent configuration for a validator hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ValidatorConfig {
    /// Human-readable name.
    pub name: String,
    /// Pipeline stage.
    pub stage: ValidationStage,
    /// Ordered list of rules to evaluate.
    pub rules: Vec<ValidationRule>,
}

// ─── Validation result ───────────────────────────────────────────────────────

/// Outcome of running all validation rules against a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ValidationResult {
    /// All rules passed.
    Pass,
    /// One or more rules failed; carries the list of human-readable reasons.
    Fail(Vec<String>),
}

impl ValidationResult {
    /// Returns `true` when validation passed.
    pub(crate) fn is_pass(&self) -> bool {
        matches!(self, ValidationResult::Pass)
    }
}

// ─── Runtime hook ────────────────────────────────────────────────────────────

/// An input/output validation hook.
///
/// Validators inspect a message against a set of [`ValidationRule`]s and
/// report all violations at once (they do not short-circuit).
#[derive(Debug)]
pub(crate) struct ValidatorHook {
    name: String,
    stage: ValidationStage,
    rules: Vec<ValidationRule>,
}

impl ValidatorHook {
    /// Create a `ValidatorHook` from its configuration.
    pub(crate) fn from_config(config: &ValidatorConfig) -> Self {
        Self {
            name: config.name.clone(),
            stage: config.stage,
            rules: config.rules.clone(),
        }
    }

    /// The pipeline stage this validator targets.
    pub(crate) fn stage(&self) -> ValidationStage {
        self.stage
    }

    /// The human-readable name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Validate `message` against all configured rules.
    ///
    /// All rules are evaluated (no short-circuit) so the caller gets a
    /// complete picture of every violation.
    pub(crate) fn validate(&self, message: &str) -> ValidationResult {
        let mut failures: Vec<String> = Vec::new();

        for rule in &self.rules {
            if let Some(reason) = rule.check(message) {
                failures.push(reason);
            }
        }

        if failures.is_empty() {
            debug!(validator = %self.name, stage = ?self.stage, "validation passed");
            ValidationResult::Pass
        } else {
            debug!(
                validator = %self.name,
                stage = ?self.stage,
                failure_count = failures.len(),
                "validation failed"
            );
            ValidationResult::Fail(failures)
        }
    }
}

// ─── Rule evaluation ─────────────────────────────────────────────────────────

impl ValidationRule {
    /// Check a single rule against `message`.
    ///
    /// Returns `None` when the rule passes, or `Some(reason)` describing the
    /// violation.
    fn check(&self, message: &str) -> Option<String> {
        match self {
            ValidationRule::MaxLength { max } => {
                let len = message.chars().count();
                if len > *max {
                    Some(format!("message too long: {len} characters (max {max})"))
                } else {
                    None
                }
            }

            ValidationRule::MinLength { min } => {
                let len = message.chars().count();
                if len < *min {
                    Some(format!("message too short: {len} characters (min {min})"))
                } else {
                    None
                }
            }

            ValidationRule::RegexMatch { pattern } => match Regex::new(pattern) {
                Ok(re) => {
                    if re.is_match(message) {
                        None
                    } else {
                        Some(format!(
                            "message does not match required pattern: {pattern}"
                        ))
                    }
                }
                Err(err) => Some(format!("invalid regex pattern '{pattern}': {err}")),
            },

            ValidationRule::RegexReject { pattern } => match Regex::new(pattern) {
                Ok(re) => {
                    if re.is_match(message) {
                        Some(format!("message matches rejected pattern: {pattern}"))
                    } else {
                        None
                    }
                }
                Err(err) => Some(format!("invalid regex pattern '{pattern}': {err}")),
            },

            ValidationRule::ContainsRequired { required } => {
                let missing: Vec<&String> = required
                    .iter()
                    .filter(|r| !message.contains(r.as_str()))
                    .collect();
                if missing.is_empty() {
                    None
                } else {
                    let list = missing
                        .iter()
                        .map(|s| format!("'{s}'"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    Some(format!("message missing required content: {list}"))
                }
            }

            ValidationRule::BlockKeywords { keywords } => {
                let lower = message.to_lowercase();
                let found: Vec<&String> = keywords
                    .iter()
                    .filter(|k| lower.contains(&k.to_lowercase()))
                    .collect();
                if found.is_empty() {
                    None
                } else {
                    let list = found
                        .iter()
                        .map(|s| format!("'{s}'"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    Some(format!("message contains blocked keywords: {list}"))
                }
            }

            ValidationRule::JsonSchema { schema } => {
                // Basic JSON validity check.  Full JSON Schema validation is
                // left as a future extension; here we verify the message is
                // at least valid JSON.
                if serde_json::from_str::<serde_json::Value>(message).is_err() {
                    Some("message is not valid JSON".into())
                } else if schema.is_empty() {
                    // No schema content provided — pass if JSON is valid.
                    None
                } else {
                    // Schema content present — for now we just ensure the
                    // message is valid JSON (schema matching is a future TODO).
                    None
                }
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_validator(rules: Vec<ValidationRule>) -> ValidatorHook {
        ValidatorHook::from_config(&ValidatorConfig {
            name: "test-validator".into(),
            stage: ValidationStage::Input,
            rules,
        })
    }

    // ── MaxLength ────────────────────────────────────────────────────────

    #[test]
    fn max_length_passes_within_limit() {
        let v = make_validator(vec![ValidationRule::MaxLength { max: 10 }]);
        assert!(v.validate("hello").is_pass());
    }

    #[test]
    fn max_length_passes_at_exact_limit() {
        let v = make_validator(vec![ValidationRule::MaxLength { max: 5 }]);
        assert!(v.validate("hello").is_pass());
    }

    #[test]
    fn max_length_fails_over_limit() {
        let v = make_validator(vec![ValidationRule::MaxLength { max: 3 }]);
        let result = v.validate("hello");
        assert_eq!(
            result,
            ValidationResult::Fail(vec!["message too long: 5 characters (max 3)".into()])
        );
    }

    #[test]
    fn max_length_counts_chars_not_bytes() {
        let v = make_validator(vec![ValidationRule::MaxLength { max: 2 }]);
        // Two emoji characters = 2 chars but many bytes.
        assert!(v.validate("\u{1F600}\u{1F601}").is_pass());
    }

    // ── MinLength ────────────────────────────────────────────────────────

    #[test]
    fn min_length_passes_above_minimum() {
        let v = make_validator(vec![ValidationRule::MinLength { min: 3 }]);
        assert!(v.validate("hello").is_pass());
    }

    #[test]
    fn min_length_passes_at_exact_minimum() {
        let v = make_validator(vec![ValidationRule::MinLength { min: 5 }]);
        assert!(v.validate("hello").is_pass());
    }

    #[test]
    fn min_length_fails_below_minimum() {
        let v = make_validator(vec![ValidationRule::MinLength { min: 10 }]);
        let result = v.validate("hi");
        assert_eq!(
            result,
            ValidationResult::Fail(vec!["message too short: 2 characters (min 10)".into()])
        );
    }

    // ── RegexMatch ───────────────────────────────────────────────────────

    #[test]
    fn regex_match_passes_on_match() {
        let v = make_validator(vec![ValidationRule::RegexMatch {
            pattern: r"^\d{3}-\d{4}$".into(),
        }]);
        assert!(v.validate("123-4567").is_pass());
    }

    #[test]
    fn regex_match_fails_on_no_match() {
        let v = make_validator(vec![ValidationRule::RegexMatch {
            pattern: r"^\d+$".into(),
        }]);
        let result = v.validate("abc");
        assert!(matches!(result, ValidationResult::Fail(_)));
    }

    #[test]
    fn regex_match_reports_invalid_pattern() {
        let v = make_validator(vec![ValidationRule::RegexMatch {
            pattern: r"[invalid".into(),
        }]);
        let result = v.validate("test");
        match result {
            ValidationResult::Fail(reasons) => {
                assert!(reasons[0].contains("invalid regex pattern"));
            }
            _ => panic!("expected failure for invalid regex"),
        }
    }

    // ── RegexReject ──────────────────────────────────────────────────────

    #[test]
    fn regex_reject_passes_when_no_match() {
        let v = make_validator(vec![ValidationRule::RegexReject {
            pattern: r"(?i)spam".into(),
        }]);
        assert!(v.validate("hello world").is_pass());
    }

    #[test]
    fn regex_reject_fails_when_matches() {
        let v = make_validator(vec![ValidationRule::RegexReject {
            pattern: r"(?i)spam".into(),
        }]);
        let result = v.validate("this is SPAM");
        assert!(matches!(result, ValidationResult::Fail(_)));
    }

    // ── ContainsRequired ─────────────────────────────────────────────────

    #[test]
    fn contains_required_passes_when_all_present() {
        let v = make_validator(vec![ValidationRule::ContainsRequired {
            required: vec!["hello".into(), "world".into()],
        }]);
        assert!(v.validate("hello beautiful world").is_pass());
    }

    #[test]
    fn contains_required_fails_when_some_missing() {
        let v = make_validator(vec![ValidationRule::ContainsRequired {
            required: vec!["hello".into(), "world".into()],
        }]);
        let result = v.validate("hello there");
        match result {
            ValidationResult::Fail(reasons) => {
                assert!(reasons[0].contains("'world'"));
            }
            _ => panic!("expected failure"),
        }
    }

    #[test]
    fn contains_required_empty_list_always_passes() {
        let v = make_validator(vec![ValidationRule::ContainsRequired { required: vec![] }]);
        assert!(v.validate("anything").is_pass());
    }

    // ── BlockKeywords ────────────────────────────────────────────────────

    #[test]
    fn block_keywords_passes_when_none_found() {
        let v = make_validator(vec![ValidationRule::BlockKeywords {
            keywords: vec!["spam".into(), "scam".into()],
        }]);
        assert!(v.validate("hello world").is_pass());
    }

    #[test]
    fn block_keywords_fails_case_insensitive() {
        let v = make_validator(vec![ValidationRule::BlockKeywords {
            keywords: vec!["spam".into()],
        }]);
        let result = v.validate("This is SPAM!");
        assert!(matches!(result, ValidationResult::Fail(_)));
    }

    #[test]
    fn block_keywords_reports_all_matches() {
        let v = make_validator(vec![ValidationRule::BlockKeywords {
            keywords: vec!["spam".into(), "scam".into()],
        }]);
        let result = v.validate("spam and scam");
        match result {
            ValidationResult::Fail(reasons) => {
                assert!(reasons[0].contains("'spam'"));
                assert!(reasons[0].contains("'scam'"));
            }
            _ => panic!("expected failure"),
        }
    }

    // ── JsonSchema ───────────────────────────────────────────────────────

    #[test]
    fn json_schema_passes_for_valid_json() {
        let v = make_validator(vec![ValidationRule::JsonSchema {
            schema: String::new(),
        }]);
        assert!(v.validate(r#"{"key": "value"}"#).is_pass());
    }

    #[test]
    fn json_schema_fails_for_invalid_json() {
        let v = make_validator(vec![ValidationRule::JsonSchema {
            schema: String::new(),
        }]);
        let result = v.validate("not json at all");
        assert!(matches!(result, ValidationResult::Fail(_)));
    }

    #[test]
    fn json_schema_with_schema_string_still_validates_json() {
        let v = make_validator(vec![ValidationRule::JsonSchema {
            schema: r#"{"type": "object"}"#.into(),
        }]);
        assert!(v.validate(r#"{"foo": 1}"#).is_pass());
    }

    // ── Multiple rules ───────────────────────────────────────────────────

    #[test]
    fn multiple_rules_all_pass() {
        let v = make_validator(vec![
            ValidationRule::MinLength { min: 3 },
            ValidationRule::MaxLength { max: 100 },
            ValidationRule::BlockKeywords {
                keywords: vec!["spam".into()],
            },
        ]);
        assert!(v.validate("hello world").is_pass());
    }

    #[test]
    fn multiple_rules_collect_all_failures() {
        let v = make_validator(vec![
            ValidationRule::MinLength { min: 20 },
            ValidationRule::MaxLength { max: 3 },
            ValidationRule::BlockKeywords {
                keywords: vec!["hi".into()],
            },
        ]);
        let result = v.validate("hi");
        match result {
            ValidationResult::Fail(reasons) => {
                // All three rules should fail.
                assert_eq!(reasons.len(), 3, "expected 3 failures, got: {reasons:?}");
                assert!(reasons[0].contains("too short"));
                assert!(reasons[1].contains("too long"));
                assert!(reasons[2].contains("blocked keywords"));
            }
            _ => panic!("expected failure"),
        }
    }

    #[test]
    fn empty_rules_always_pass() {
        let v = make_validator(vec![]);
        assert!(v.validate("anything at all").is_pass());
    }

    // ── Stage & metadata ─────────────────────────────────────────────────

    #[test]
    fn output_stage_is_preserved() {
        let v = ValidatorHook::from_config(&ValidatorConfig {
            name: "output-check".into(),
            stage: ValidationStage::Output,
            rules: vec![],
        });
        assert_eq!(v.stage(), ValidationStage::Output);
        assert_eq!(v.name(), "output-check");
    }

    // ── Config serialization ─────────────────────────────────────────────

    #[test]
    fn config_serialization_roundtrip() {
        let config = ValidatorConfig {
            name: "input-guard".into(),
            stage: ValidationStage::Input,
            rules: vec![
                ValidationRule::MaxLength { max: 1000 },
                ValidationRule::BlockKeywords {
                    keywords: vec!["badword".into()],
                },
            ],
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ValidatorConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "input-guard");
        assert_eq!(deserialized.stage, ValidationStage::Input);
        assert_eq!(deserialized.rules.len(), 2);
    }

    #[test]
    fn config_deserialize_from_json() {
        let json = r#"{
            "name": "check",
            "stage": "output",
            "rules": [
                { "type": "maxLength", "max": 500 },
                { "type": "regexReject", "pattern": "(?i)password" }
            ]
        }"#;
        let config: ValidatorConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "check");
        assert_eq!(config.stage, ValidationStage::Output);
        assert_eq!(config.rules.len(), 2);
    }

    // ── ValidationResult helper ──────────────────────────────────────────

    #[test]
    fn is_pass_returns_correct_value() {
        assert!(ValidationResult::Pass.is_pass());
        assert!(!ValidationResult::Fail(vec!["oops".into()]).is_pass());
    }
}
