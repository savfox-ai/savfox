use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ─── Hook decision ───────────────────────────────────────────────────────────

/// The result of evaluating an LLM hook against a message.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HookDecision {
    /// The hook should fire; carries the LLM's reasoning or extracted payload.
    Fire(String),
    /// The hook determined the message does not match; skip it.
    Skip,
    /// The hook could not reach a decision (e.g. LLM error); defer to the
    /// next hook in the chain or to default behaviour.
    Defer,
}

// ─── Configuration ───────────────────────────────────────────────────────────

/// Persistent configuration for an LLM-driven hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LlmHookConfig {
    /// Human-readable name for this hook.
    pub name: String,
    /// Prompt template sent to the LLM.  The placeholder `{{message}}` is
    /// replaced with the incoming message text before evaluation.
    pub prompt: String,
    /// Optional model override (e.g. `"gpt-4o-mini"`).  When `None` the
    /// gateway's default model is used.
    #[serde(default)]
    pub model: Option<String>,
    /// Confidence threshold in `0.0..=1.0`.  The LLM's response must express
    /// a confidence at or above this value for the hook to fire.
    #[serde(default = "default_threshold")]
    pub threshold: f64,
}

fn default_threshold() -> f64 {
    0.5
}

// ─── Runtime hook ────────────────────────────────────────────────────────────

/// An LLM-driven hook that uses a language model to decide whether an incoming
/// message should trigger an action.
///
/// The hook sends the configured prompt (with the message interpolated) to an
/// LLM and inspects the response to produce a [`HookDecision`].
#[derive(Debug)]
pub(crate) struct LlmHook {
    /// The prompt template.  `{{message}}` is replaced at evaluation time.
    prompt_template: String,
    /// Optional model override.
    model_override: Option<String>,
    /// Confidence threshold (`0.0..=1.0`).
    threshold: f64,
}

impl LlmHook {
    /// Create a new `LlmHook` from its configuration.
    pub(crate) fn from_config(config: &LlmHookConfig) -> Self {
        Self {
            prompt_template: config.prompt.clone(),
            model_override: config.model.clone(),
            threshold: config.threshold.clamp(0.0, 1.0),
        }
    }

    /// Return the model override, if any.
    pub(crate) fn model_override(&self) -> Option<&str> {
        self.model_override.as_deref()
    }

    /// Build the full prompt by interpolating `{{message}}` with the actual
    /// message text.
    pub(crate) fn render_prompt(&self, message: &str) -> String {
        self.prompt_template.replace("{{message}}", message)
    }

    /// Evaluate an LLM response string and return a [`HookDecision`].
    ///
    /// The method looks for a JSON-ish structure in the response of the form:
    ///
    /// ```json
    /// { "fire": true, "confidence": 0.85, "reason": "..." }
    /// ```
    ///
    /// If the `confidence` meets or exceeds the configured threshold and
    /// `fire` is `true`, the hook fires with the `reason` as payload.
    ///
    /// When the response cannot be parsed, the hook returns [`HookDecision::Defer`].
    pub(crate) fn evaluate(&self, llm_response: &str) -> HookDecision {
        // Try to extract a JSON object from the response.
        let parsed: Option<LlmResponse> = extract_json_response(llm_response);

        match parsed {
            Some(resp) => {
                let confidence = resp.confidence.unwrap_or(1.0).clamp(0.0, 1.0);
                debug!(
                    fire = resp.fire,
                    confidence = confidence,
                    threshold = self.threshold,
                    "llm hook evaluation"
                );

                if resp.fire && confidence >= self.threshold {
                    let reason = resp
                        .reason
                        .unwrap_or_else(|| "LLM hook triggered".to_owned());
                    HookDecision::Fire(reason)
                } else {
                    HookDecision::Skip
                }
            }
            None => {
                warn!("llm hook: unable to parse LLM response as decision JSON");
                HookDecision::Defer
            }
        }
    }
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Expected shape of the LLM's JSON response.
#[derive(Debug, Deserialize)]
struct LlmResponse {
    fire: bool,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    reason: Option<String>,
}

/// Try to extract a JSON object from a potentially noisy LLM response.
///
/// Looks for the first `{` and last `}` and attempts to deserialise the
/// substring between them.
fn extract_json_response(text: &str) -> Option<LlmResponse> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let json_str = &text[start..=end];
    serde_json::from_str(json_str).ok()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hook(threshold: f64) -> LlmHook {
        LlmHook::from_config(&LlmHookConfig {
            name: "test-hook".into(),
            prompt: "Decide if '{{message}}' is urgent.".into(),
            model: None,
            threshold,
        })
    }

    #[test]
    fn render_prompt_substitutes_message() {
        let hook = make_hook(0.5);
        let rendered = hook.render_prompt("server is on fire");
        assert_eq!(rendered, "Decide if 'server is on fire' is urgent.");
    }

    #[test]
    fn evaluate_fires_when_above_threshold() {
        let hook = make_hook(0.7);
        let response = r#"{"fire": true, "confidence": 0.9, "reason": "critical alert"}"#;
        assert_eq!(
            hook.evaluate(response),
            HookDecision::Fire("critical alert".into())
        );
    }

    #[test]
    fn evaluate_skips_when_below_threshold() {
        let hook = make_hook(0.8);
        let response = r#"{"fire": true, "confidence": 0.5, "reason": "maybe"}"#;
        assert_eq!(hook.evaluate(response), HookDecision::Skip);
    }

    #[test]
    fn evaluate_skips_when_fire_is_false() {
        let hook = make_hook(0.1);
        let response = r#"{"fire": false, "confidence": 0.99, "reason": "nope"}"#;
        assert_eq!(hook.evaluate(response), HookDecision::Skip);
    }

    #[test]
    fn evaluate_defers_on_unparseable_response() {
        let hook = make_hook(0.5);
        let response = "I'm not sure what to do here.";
        assert_eq!(hook.evaluate(response), HookDecision::Defer);
    }

    #[test]
    fn evaluate_extracts_json_from_noisy_response() {
        let hook = make_hook(0.5);
        let response = r#"Sure! Here is my analysis: {"fire": true, "confidence": 0.8, "reason": "matches pattern"} Hope that helps!"#;
        assert_eq!(
            hook.evaluate(response),
            HookDecision::Fire("matches pattern".into())
        );
    }

    #[test]
    fn evaluate_defaults_confidence_to_one_when_missing() {
        let hook = make_hook(0.5);
        let response = r#"{"fire": true, "reason": "go"}"#;
        assert_eq!(hook.evaluate(response), HookDecision::Fire("go".into()));
    }

    #[test]
    fn evaluate_defaults_reason_when_missing() {
        let hook = make_hook(0.5);
        let response = r#"{"fire": true, "confidence": 0.9}"#;
        assert_eq!(
            hook.evaluate(response),
            HookDecision::Fire("LLM hook triggered".into())
        );
    }

    #[test]
    fn from_config_clamps_threshold() {
        let hook = LlmHook::from_config(&LlmHookConfig {
            name: "clamped".into(),
            prompt: "test".into(),
            model: Some("gpt-4o-mini".into()),
            threshold: 1.5, // above 1.0
        });
        assert!((hook.threshold - 1.0).abs() < f64::EPSILON);
        assert_eq!(hook.model_override(), Some("gpt-4o-mini"));
    }

    #[test]
    fn from_config_clamps_negative_threshold() {
        let hook = LlmHook::from_config(&LlmHookConfig {
            name: "clamped".into(),
            prompt: "test".into(),
            model: None,
            threshold: -0.5,
        });
        assert!((hook.threshold - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn config_serialization_roundtrip() {
        let config = LlmHookConfig {
            name: "urgency-detector".into(),
            prompt: "Is {{message}} urgent?".into(),
            model: Some("claude-3-haiku".into()),
            threshold: 0.75,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: LlmHookConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "urgency-detector");
        assert_eq!(deserialized.model.as_deref(), Some("claude-3-haiku"));
        assert!((deserialized.threshold - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn config_default_threshold_is_half() {
        let json = r#"{"name": "test", "prompt": "test"}"#;
        let config: LlmHookConfig = serde_json::from_str(json).unwrap();
        assert!((config.threshold - 0.5).abs() < f64::EPSILON);
    }
}
