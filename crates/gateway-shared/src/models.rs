use std::fmt;

use serde::{Deserialize, Serialize};

/// Reasoning effort level surfaced for a model.
///
/// Wire format: serializes/deserializes as the lowercase variant name (e.g. `"medium"`).
/// Mirrors `savfox_protocol::openai_models::ReasoningEffort` so the JSON wire format
/// is identical across the protocol and gateway-shared boundaries.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
}

impl ReasoningEffort {
    /// Lowercase wire-format name (`"none"`, `"minimal"`, ..., `"xhigh"`).
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AvailableModel {
    pub id: String,
    pub name: Option<String>,
    pub provider: Option<String>,
    pub model_slug: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub max_tokens: Option<i64>,
    pub temperature: Option<f64>,
    pub is_default: Option<bool>,
    pub builtin: Option<bool>,
    pub default_reasoning_level: Option<ReasoningEffort>,
    pub supported_reasoning_levels: Option<Vec<ReasoningEffortPreset>>,
    /// Normalized account slug (e.g. "work-account") for multi-account setups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_slug: Option<String>,
    /// Human-readable provider display name (e.g. "OpenAI").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    /// Human-readable account display name (e.g. "Work").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AvailableModelsResponse {
    pub models: Vec<AvailableModel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningEffortPreset {
    pub effort: ReasoningEffort,
    pub description: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reasoning_effort_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(ReasoningEffort::None).expect("reasoning effort should serialize"),
            json!("none")
        );
        assert_eq!(
            serde_json::to_value(ReasoningEffort::Minimal)
                .expect("reasoning effort should serialize"),
            json!("minimal")
        );
        assert_eq!(
            serde_json::to_value(ReasoningEffort::Low).expect("reasoning effort should serialize"),
            json!("low")
        );
        assert_eq!(
            serde_json::to_value(ReasoningEffort::Medium)
                .expect("reasoning effort should serialize"),
            json!("medium")
        );
        assert_eq!(
            serde_json::to_value(ReasoningEffort::High).expect("reasoning effort should serialize"),
            json!("high")
        );
        assert_eq!(
            serde_json::to_value(ReasoningEffort::XHigh)
                .expect("reasoning effort should serialize"),
            json!("xhigh")
        );
    }

    #[test]
    fn reasoning_effort_round_trip_from_string() {
        let parsed: ReasoningEffort =
            serde_json::from_value(json!("medium")).expect("reasoning effort should deserialize");
        assert_eq!(parsed, ReasoningEffort::Medium);
    }

    #[test]
    fn reasoning_effort_preset_round_trip() {
        let preset = ReasoningEffortPreset {
            effort: ReasoningEffort::Low,
            description: "Quick".to_owned(),
        };
        let value = serde_json::to_value(&preset).expect("preset should serialize");
        assert_eq!(value, json!({"effort": "low", "description": "Quick"}));
        let back: ReasoningEffortPreset =
            serde_json::from_value(value).expect("preset should deserialize");
        assert_eq!(back, preset);
    }
}
