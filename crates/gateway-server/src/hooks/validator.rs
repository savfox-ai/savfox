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
                let value = match serde_json::from_str::<serde_json::Value>(message) {
                    Ok(v) => v,
                    Err(_) => return Some("message is not valid JSON".into()),
                };

                if schema.is_empty() {
                    return None;
                }

                let schema_obj = match serde_json::from_str::<serde_json::Value>(schema) {
                    Ok(s) => s,
                    Err(err) => {
                        return Some(format!("invalid JSON Schema: {err}"));
                    }
                };

                // Validate: type, required, properties (basic subset).
                validate_json_schema(&value, &schema_obj)
            }
        }
    }
}

// ─── Basic JSON Schema validation ─────────────────────────────────────────────

/// Validate a JSON value against a basic JSON Schema object.
///
/// Supports a practical subset of JSON Schema Draft-07:
/// - `type`: "object", "array", "string", "number", "integer", "boolean", "null"
/// - `required`: array of required property names (when type is object)
/// - `properties`: per-property schemas with `type` validation (when type is object)
/// - `minLength` / `maxLength`: string length constraints
/// - `minimum` / `maximum`: number range constraints
/// - `minItems` / `maxItems`: array length constraints
/// - `enum`: allowed value list
fn validate_json_schema(
    value: &serde_json::Value,
    schema: &serde_json::Value,
) -> Option<String> {
    let schema_obj = match schema.as_object() {
        Some(o) => o,
        None => return None, // non-object schemas are treated as pass-through
    };

    let mut errors: Vec<String> = Vec::new();

    // ── type check ──────────────────────────────────────────────────
    if let Some(expected_type) = schema_obj.get("type").and_then(|t| t.as_str()) {
        let ok = match expected_type {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.is_i64() || value.is_u64(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => true,
        };
        if !ok {
            errors.push(format!(
                "expected type '{expected_type}', got {}",
                json_type_name(value)
            ));
        }
    }

    // ── enum check ──────────────────────────────────────────────────
    if let Some(allowed) = schema_obj.get("enum").and_then(|e| e.as_array()) {
        if !allowed.contains(value) {
            errors.push(format!("value not in allowed enum: {value}"));
        }
    }

    // ── string constraints ──────────────────────────────────────────
    if let Some(s) = value.as_str() {
        if let Some(min) = schema_obj.get("minLength").and_then(|v| v.as_u64()) {
            if (s.chars().count() as u64) < min {
                errors.push(format!("string shorter than minLength {min}"));
            }
        }
        if let Some(max) = schema_obj.get("maxLength").and_then(|v| v.as_u64()) {
            if (s.chars().count() as u64) > max {
                errors.push(format!("string longer than maxLength {max}"));
            }
        }
    }

    // ── number constraints ──────────────────────────────────────────
    if let Some(n) = value.as_f64() {
        if let Some(min) = schema_obj.get("minimum").and_then(|v| v.as_f64()) {
            if n < min {
                errors.push(format!("value {n} is less than minimum {min}"));
            }
        }
        if let Some(max) = schema_obj.get("maximum").and_then(|v| v.as_f64()) {
            if n > max {
                errors.push(format!("value {n} is greater than maximum {max}"));
            }
        }
    }

    // ── array constraints ───────────────────────────────────────────
    if let Some(arr) = value.as_array() {
        if let Some(min) = schema_obj.get("minItems").and_then(|v| v.as_u64()) {
            if (arr.len() as u64) < min {
                errors.push(format!("array has {} items, minimum is {min}", arr.len()));
            }
        }
        if let Some(max) = schema_obj.get("maxItems").and_then(|v| v.as_u64()) {
            if (arr.len() as u64) > max {
                errors.push(format!("array has {} items, maximum is {max}", arr.len()));
            }
        }
    }

    // ── object: required + properties ───────────────────────────────
    if let Some(obj) = value.as_object() {
        if let Some(required) = schema_obj.get("required").and_then(|r| r.as_array()) {
            for req in required {
                if let Some(key) = req.as_str() {
                    if !obj.contains_key(key) {
                        errors.push(format!("missing required property '{key}'"));
                    }
                }
            }
        }

        if let Some(props) = schema_obj.get("properties").and_then(|p| p.as_object()) {
            for (key, prop_schema) in props {
                if let Some(prop_value) = obj.get(key) {
                    if let Some(err) = validate_json_schema(prop_value, prop_schema) {
                        errors.push(format!("property '{key}': {err}"));
                    }
                }
            }
        }
    }

    if errors.is_empty() {
        None
    } else {
        Some(format!("JSON Schema validation failed: {}", errors.join("; ")))
    }
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
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

    #[test]
    fn json_schema_type_mismatch() {
        let v = make_validator(vec![ValidationRule::JsonSchema {
            schema: r#"{"type": "object"}"#.into(),
        }]);
        let result = v.validate(r#""just a string""#);
        assert!(matches!(result, ValidationResult::Fail(_)));
    }

    #[test]
    fn json_schema_required_properties() {
        let v = make_validator(vec![ValidationRule::JsonSchema {
            schema: r#"{"type": "object", "required": ["name", "age"]}"#.into(),
        }]);
        // Missing "age"
        let result = v.validate(r#"{"name": "Alice"}"#);
        match result {
            ValidationResult::Fail(reasons) => {
                assert!(reasons[0].contains("age"));
            }
            _ => panic!("expected failure for missing required property"),
        }
    }

    #[test]
    fn json_schema_nested_property_types() {
        let v = make_validator(vec![ValidationRule::JsonSchema {
            schema: r#"{"type": "object", "properties": {"count": {"type": "integer"}}}"#
                .into(),
        }]);
        assert!(v.validate(r#"{"count": 42}"#).is_pass());
        let result = v.validate(r#"{"count": "not a number"}"#);
        assert!(matches!(result, ValidationResult::Fail(_)));
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
                // MinLength + BlockKeywords should fail; MaxLength should pass.
                assert_eq!(reasons.len(), 2, "expected 2 failures, got: {reasons:?}");
                assert!(reasons[0].contains("too short"));
                assert!(reasons[1].contains("blocked keywords"));
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
