use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelInfo {
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
    pub default_reasoning_level: Option<String>,
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
pub struct ModelsResponse {
    pub models: Vec<ModelInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReasoningEffortPreset {
    pub effort: String,
    pub description: String,
}
