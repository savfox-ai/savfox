//! Shared model metadata types exchanged between Savfox services and clients.
//!
//! These types are serialized across core, TUI, app-server, and SDK boundaries, so field defaults
//! are used to preserve compatibility when older payloads omit newly introduced attributes.

use std::collections::{HashMap, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;
use strum_macros::{Display, EnumIter};
use tracing::warn;
use ts_rs::TS;

use crate::config_types::{Personality, Verbosity};

const PERSONALITY_PLACEHOLDER: &str = "{{ personality }}";

/// See https://platform.openai.com/docs/guides/reasoning?api-mode=responses#get-started-with-reasoning
#[derive(
    Debug,
    Serialize,
    Deserialize,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Display,
    JsonSchema,
    TS,
    EnumIter,
    Hash,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
}

/// Canonical user-input modality tags advertised by a model.
#[derive(
    Debug,
    Serialize,
    Deserialize,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Display,
    JsonSchema,
    TS,
    EnumIter,
    Hash,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum InputModality {
    /// Plain text turns and tool payloads.
    Text,
    /// Image attachments included in user turns.
    Image,
}

/// Backward-compatible default when `input_modalities` is omitted on the wire.
///
/// Legacy payloads predate modality metadata, so we conservatively assume both text and images are
/// accepted unless a preset explicitly narrows support.
#[must_use]
pub fn default_input_modalities() -> Vec<InputModality> {
    vec![InputModality::Text, InputModality::Image]
}

/// A reasoning effort option that can be surfaced for a model.
#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema, PartialEq, Eq)]
pub struct ReasoningEffortPreset {
    /// Effort level that the model supports.
    pub effort: ReasoningEffort,
    /// Short human description shown next to the effort in UIs.
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema, PartialEq)]
pub struct ModelUpgrade {
    pub id: String,
    pub reasoning_effort_mapping: Option<HashMap<ReasoningEffort, ReasoningEffort>>,
    pub migration_config_key: String,
    pub model_link: Option<String>,
    pub upgrade_copy: Option<String>,
    pub migration_markdown: Option<String>,
}

/// Metadata describing a Savfox-supported model.
#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema, PartialEq)]
pub struct ModelPreset {
    /// Stable identifier for the preset.
    pub id: String,
    /// Model slug (e.g., "gpt-5").
    #[serde(alias = "model")]
    pub slug: String,
    /// Display name shown in UIs.
    pub name: String,
    /// Short human description shown in UIs.
    pub description: String,
    /// Reasoning effort applied when none is explicitly chosen.
    pub default_reasoning_effort: ReasoningEffort,
    /// Supported reasoning effort options.
    pub supported_reasoning_efforts: Vec<ReasoningEffortPreset>,
    /// Whether this model supports personality-specific instructions.
    #[serde(default)]
    pub supports_personality: bool,
    /// Whether this is the default model for new users.
    pub is_default: bool,
    /// recommended upgrade model
    pub upgrade: Option<ModelUpgrade>,
    /// Whether this preset should appear in the picker UI.
    pub show_in_picker: bool,
    /// whether this model is supported in the api
    pub supported_in_api: bool,
    /// Input modalities accepted when composing user turns for this preset.
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<InputModality>,
}

/// Visibility of a model in the picker or APIs.
#[derive(
    Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema, EnumIter, Display,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ModelVisibility {
    List,
    Hide,
    None,
}

/// Shell execution capability for a model.
#[derive(
    Debug,
    Serialize,
    Deserialize,
    Clone,
    Copy,
    PartialEq,
    Eq,
    TS,
    JsonSchema,
    EnumIter,
    Display,
    Hash,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ConfigShellToolType {
    Default,
    Local,
    UnifiedExec,
    Disabled,
    ShellCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPatchToolType {
    Freeform,
    Function,
}

/// Server-provided truncation policy metadata for a model.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TruncationMode {
    Bytes,
    Tokens,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema)]
pub struct TruncationPolicyConfig {
    pub mode: TruncationMode,
    pub limit: i64,
}

impl TruncationPolicyConfig {
    #[must_use]
    pub const fn bytes(limit: i64) -> Self {
        Self {
            mode: TruncationMode::Bytes,
            limit,
        }
    }

    #[must_use]
    pub const fn tokens(limit: i64) -> Self {
        Self {
            mode: TruncationMode::Tokens,
            limit,
        }
    }
}

/// Semantic version triple encoded as an array in JSON (e.g. [0, 62, 0]).
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema)]
pub struct ClientVersion(pub i32, pub i32, pub i32);

const fn default_effective_context_window_percent() -> i64 {
    95
}

fn default_supported_reasoning_levels() -> Vec<ReasoningEffortPreset> {
    Vec::new()
}

const fn default_shell_type() -> ConfigShellToolType {
    ConfigShellToolType::Default
}

const fn default_visibility() -> ModelVisibility {
    ModelVisibility::None
}

const fn default_supported_in_api() -> bool {
    true
}

const fn default_priority() -> i32 {
    99
}

fn default_base_instructions() -> String {
    String::new()
}

const fn default_supports_reasoning_summaries() -> bool {
    false
}

const fn default_support_verbosity() -> bool {
    false
}

fn default_truncation_policy() -> TruncationPolicyConfig {
    TruncationPolicyConfig::bytes(10_000)
}

const fn default_supports_parallel_tool_calls() -> bool {
    false
}

fn default_experimental_supported_tools() -> Vec<String> {
    Vec::new()
}

/// Model metadata returned by the Savfox backend `/models` endpoint.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct ModelInfo {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_level: Option<ReasoningEffort>,
    #[serde(default = "default_supported_reasoning_levels")]
    pub supported_reasoning_levels: Vec<ReasoningEffortPreset>,
    #[serde(default = "default_shell_type")]
    pub shell_type: ConfigShellToolType,
    #[serde(default = "default_visibility")]
    pub visibility: ModelVisibility,
    #[serde(default = "default_supported_in_api")]
    pub supported_in_api: bool,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub upgrade: Option<ModelInfoUpgrade>,
    #[serde(default = "default_base_instructions")]
    pub base_instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_messages: Option<ModelMessages>,
    #[serde(default = "default_supports_reasoning_summaries")]
    pub supports_reasoning_summaries: bool,
    #[serde(default = "default_support_verbosity")]
    pub support_verbosity: bool,
    #[serde(default)]
    pub default_verbosity: Option<Verbosity>,
    #[serde(default)]
    pub apply_patch_tool_type: Option<ApplyPatchToolType>,
    #[serde(default = "default_truncation_policy")]
    pub truncation_policy: TruncationPolicyConfig,
    #[serde(default = "default_supports_parallel_tool_calls")]
    pub supports_parallel_tool_calls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    /// Maximum tokens this model is willing to emit on a single response.
    /// When omitted callers should fall back to a conservative derivation
    /// from `context_window` (see [`ModelInfo::resolved_max_output_tokens`]).
    /// Setting this explicitly lets a model with a small context window but
    /// large output budget (e.g. Claude Sonnet 4 — 200K in / 64K out)
    /// expose its full output capacity rather than being clipped to
    /// `context_window / 4`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    /// Token threshold for automatic compaction. When omitted, core derives it
    /// from `context_window` (90%).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_token_limit: Option<i64>,
    /// Percentage of the context window considered usable for inputs, after
    /// reserving headroom for system prompts, tool overhead, and model output.
    #[serde(default = "default_effective_context_window_percent")]
    pub effective_context_window_percent: i64,
    #[serde(default = "default_experimental_supported_tools")]
    pub experimental_supported_tools: Vec<String>,
    /// Input modalities accepted by the backend for this model.
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<InputModality>,
}

impl Default for ModelInfo {
    fn default() -> Self {
        Self {
            slug: String::new(),
            name: String::new(),
            description: None,
            default_reasoning_level: None,
            supported_reasoning_levels: default_supported_reasoning_levels(),
            shell_type: default_shell_type(),
            visibility: default_visibility(),
            supported_in_api: default_supported_in_api(),
            priority: default_priority(),
            upgrade: None,
            base_instructions: default_base_instructions(),
            model_messages: None,
            supports_reasoning_summaries: default_supports_reasoning_summaries(),
            support_verbosity: default_support_verbosity(),
            default_verbosity: None,
            apply_patch_tool_type: None,
            truncation_policy: default_truncation_policy(),
            supports_parallel_tool_calls: default_supports_parallel_tool_calls(),
            context_window: None,
            max_output_tokens: None,
            auto_compact_token_limit: None,
            effective_context_window_percent: default_effective_context_window_percent(),
            experimental_supported_tools: default_experimental_supported_tools(),
            input_modalities: default_input_modalities(),
        }
    }
}

impl ModelInfo {
    pub fn new(slug: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            name: name.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn auto_compact_token_limit(&self) -> Option<i64> {
        self.auto_compact_token_limit.or_else(|| {
            self.context_window
                .map(|context_window| (context_window * 9) / 10)
        })
    }

    /// Resolve the per-request `max_output_tokens` for this model.
    ///
    /// Preference order:
    /// 1. The explicit `max_output_tokens` field if set in the model registry.
    /// 2. Fall back to `min(context_window / 2, MAX_REASONABLE_OUTPUT)`.
    /// 3. If even `context_window` is missing, return a conservative 8192.
    ///
    /// This replaces the previous hard-coded `(context_window / 4).max(4096)`
    /// heuristic in `client.rs`, which silently clipped Sonnet 4's 64K output
    /// budget to ~50K (S15 in the security/quality review).
    #[must_use]
    pub fn resolved_max_output_tokens(&self) -> i64 {
        const MAX_REASONABLE_OUTPUT: i64 = 64_000;
        const FALLBACK: i64 = 8_192;
        if let Some(explicit) = self.max_output_tokens.filter(|v| *v > 0) {
            return explicit;
        }
        match self.context_window {
            Some(cw) if cw > 0 => (cw / 2).min(MAX_REASONABLE_OUTPUT).max(FALLBACK),
            _ => FALLBACK,
        }
    }

    pub fn supports_personality(&self) -> bool {
        self.model_messages
            .as_ref()
            .is_some_and(ModelMessages::supports_personality)
    }

    pub fn get_model_instructions(&self, personality: Option<Personality>) -> String {
        if let Some(model_messages) = &self.model_messages
            && let Some(template) = &model_messages.instructions_template
        {
            // if we have a template, always use it
            let personality_message = model_messages
                .get_personality_message(personality)
                .unwrap_or_default();
            template.replace(PERSONALITY_PLACEHOLDER, personality_message.as_str())
        } else if let Some(personality) = personality {
            warn!(
                model = %self.slug,
                %personality,
                "Model personality requested but model_messages is missing, falling back to base instructions."
            );
            self.base_instructions.clone()
        } else {
            self.base_instructions.clone()
        }
    }
}

/// A strongly-typed template for assembling model instructions and developer messages. If
/// instructions_* is populated and valid, it will override base_instructions.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct ModelMessages {
    pub instructions_template: Option<String>,
    pub instructions_variables: Option<ModelInstructionsVariables>,
}

impl ModelMessages {
    fn has_personality_placeholder(&self) -> bool {
        self.instructions_template
            .as_ref()
            .map(|spec| spec.contains(PERSONALITY_PLACEHOLDER))
            .unwrap_or(false)
    }

    fn supports_personality(&self) -> bool {
        self.has_personality_placeholder()
            && self
                .instructions_variables
                .as_ref()
                .is_some_and(ModelInstructionsVariables::is_complete)
    }

    #[must_use]
    pub fn get_personality_message(&self, personality: Option<Personality>) -> Option<String> {
        self.instructions_variables
            .as_ref()
            .and_then(|variables| variables.get_personality_message(personality))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct ModelInstructionsVariables {
    pub personality_default: Option<String>,
    pub personality_friendly: Option<String>,
    pub personality_pragmatic: Option<String>,
}

impl ModelInstructionsVariables {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.personality_default.is_some()
            && self.personality_friendly.is_some()
            && self.personality_pragmatic.is_some()
    }

    #[must_use]
    pub fn get_personality_message(&self, personality: Option<Personality>) -> Option<String> {
        if let Some(personality) = personality {
            match personality {
                Personality::Friendly => self.personality_friendly.clone(),
                Personality::Pragmatic => self.personality_pragmatic.clone(),
            }
        } else {
            self.personality_default.clone()
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct ModelInfoUpgrade {
    pub model: String,
    pub migration_markdown: String,
}

impl From<&ModelUpgrade> for ModelInfoUpgrade {
    fn from(upgrade: &ModelUpgrade) -> Self {
        Self {
            model: upgrade.id.clone(),
            migration_markdown: upgrade.migration_markdown.clone().unwrap_or_default(),
        }
    }
}

/// Response wrapper for `/models`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema, Default)]
pub struct ModelsResponse {
    pub models: Vec<ModelInfo>,
}

// convert ModelInfo to ModelPreset
impl From<ModelInfo> for ModelPreset {
    fn from(info: ModelInfo) -> Self {
        let supports_personality = info.supports_personality();
        Self {
            id: info.slug.clone(),
            slug: info.slug.clone(),
            name: info.name,
            description: info.description.unwrap_or_default(),
            default_reasoning_effort: info
                .default_reasoning_level
                .unwrap_or(ReasoningEffort::None),
            supported_reasoning_efforts: info.supported_reasoning_levels.clone(),
            supports_personality,
            is_default: false, // default is the highest priority available model
            upgrade: info.upgrade.as_ref().map(|upgrade| ModelUpgrade {
                id: upgrade.model.clone(),
                reasoning_effort_mapping: reasoning_effort_mapping_from_presets(
                    &info.supported_reasoning_levels,
                ),
                migration_config_key: info.slug.clone(),
                // todo(aibrahim): add the model link here.
                model_link: None,
                upgrade_copy: None,
                migration_markdown: Some(upgrade.migration_markdown.clone()),
            }),
            show_in_picker: info.visibility == ModelVisibility::List,
            supported_in_api: info.supported_in_api,
            input_modalities: info.input_modalities,
        }
    }
}

impl ModelPreset {
    /// Filter models based on authentication mode.
    ///
    /// In ChatGPT mode, all models are visible. Otherwise, only API-supported models are shown.
    #[must_use]
    pub fn filter_by_auth(models: Vec<Self>, chatgpt_mode: bool) -> Vec<Self> {
        models
            .into_iter()
            .filter(|model| chatgpt_mode || model.supported_in_api)
            .collect()
    }

    /// Merge remote presets with existing presets, preferring remote when slugs match.
    ///
    /// Remote presets take precedence. Existing presets not in remote are appended with
    /// `is_default` set to false.
    #[must_use]
    pub fn merge(remote_presets: Vec<Self>, existing_presets: Vec<Self>) -> Vec<Self> {
        if remote_presets.is_empty() {
            return existing_presets;
        }

        let remote_slugs: HashSet<&str> = remote_presets
            .iter()
            .map(|preset| preset.slug.as_str())
            .collect();

        let mut merged_presets = remote_presets.clone();
        for mut preset in existing_presets {
            if remote_slugs.contains(preset.slug.as_str()) {
                continue;
            }
            preset.is_default = false;
            merged_presets.push(preset);
        }

        merged_presets
    }
}

fn reasoning_effort_mapping_from_presets(
    presets: &[ReasoningEffortPreset],
) -> Option<HashMap<ReasoningEffort, ReasoningEffort>> {
    if presets.is_empty() {
        return None;
    }

    // Map every canonical effort to the closest supported effort for the new model.
    let supported: Vec<ReasoningEffort> = presets.iter().map(|p| p.effort).collect();
    let mut map = HashMap::new();
    for effort in ReasoningEffort::iter() {
        let nearest = nearest_effort(effort, &supported);
        map.insert(effort, nearest);
    }
    Some(map)
}

fn effort_rank(effort: ReasoningEffort) -> i32 {
    match effort {
        ReasoningEffort::None => 0,
        ReasoningEffort::Minimal => 1,
        ReasoningEffort::Low => 2,
        ReasoningEffort::Medium => 3,
        ReasoningEffort::High => 4,
        ReasoningEffort::XHigh => 5,
    }
}

fn nearest_effort(target: ReasoningEffort, supported: &[ReasoningEffort]) -> ReasoningEffort {
    let target_rank = effort_rank(target);
    supported
        .iter()
        .copied()
        .min_by_key(|candidate| (effort_rank(*candidate) - target_rank).abs())
        .unwrap_or(target)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn test_model(spec: Option<ModelMessages>) -> ModelInfo {
        ModelInfo {
            slug: "test-model".to_owned(),
            name: "Test Model".to_owned(),
            description: None,
            default_reasoning_level: None,
            supported_reasoning_levels: vec![],
            shell_type: ConfigShellToolType::ShellCommand,
            visibility: ModelVisibility::List,
            supported_in_api: true,
            priority: 1,
            upgrade: None,
            base_instructions: "base".to_owned(),
            model_messages: spec,
            supports_reasoning_summaries: false,
            support_verbosity: false,
            default_verbosity: None,
            apply_patch_tool_type: None,
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
            supports_parallel_tool_calls: false,
            context_window: None,
            max_output_tokens: None,
            auto_compact_token_limit: None,
            effective_context_window_percent: 95,
            experimental_supported_tools: vec![],
            input_modalities: default_input_modalities(),
        }
    }

    fn personality_variables() -> ModelInstructionsVariables {
        ModelInstructionsVariables {
            personality_default: Some("default".to_owned()),
            personality_friendly: Some("friendly".to_owned()),
            personality_pragmatic: Some("pragmatic".to_owned()),
        }
    }

    #[test]
    fn get_model_instructions_uses_template_when_placeholder_present() {
        let model = test_model(Some(ModelMessages {
            instructions_template: Some("Hello {{ personality }}".to_owned()),
            instructions_variables: Some(personality_variables()),
        }));

        let instructions = model.get_model_instructions(Some(Personality::Friendly));

        assert_eq!(instructions, "Hello friendly");
    }

    #[test]
    fn get_model_instructions_always_strips_placeholder() {
        let model = test_model(Some(ModelMessages {
            instructions_template: Some("Hello\n{{ personality }}".to_owned()),
            instructions_variables: Some(ModelInstructionsVariables {
                personality_default: None,
                personality_friendly: Some("friendly".to_owned()),
                personality_pragmatic: None,
            }),
        }));
        assert_eq!(
            model.get_model_instructions(Some(Personality::Friendly)),
            "Hello\nfriendly"
        );
        assert_eq!(
            model.get_model_instructions(Some(Personality::Pragmatic)),
            "Hello\n"
        );
        assert_eq!(model.get_model_instructions(None), "Hello\n");

        let model_no_personality = test_model(Some(ModelMessages {
            instructions_template: Some("Hello\n{{ personality }}".to_owned()),
            instructions_variables: Some(ModelInstructionsVariables {
                personality_default: None,
                personality_friendly: None,
                personality_pragmatic: None,
            }),
        }));
        assert_eq!(
            model_no_personality.get_model_instructions(Some(Personality::Friendly)),
            "Hello\n"
        );
        assert_eq!(
            model_no_personality.get_model_instructions(Some(Personality::Pragmatic)),
            "Hello\n"
        );
        assert_eq!(model_no_personality.get_model_instructions(None), "Hello\n");
    }

    #[test]
    fn get_model_instructions_falls_back_when_template_is_missing() {
        let model = test_model(Some(ModelMessages {
            instructions_template: None,
            instructions_variables: Some(ModelInstructionsVariables {
                personality_default: None,
                personality_friendly: None,
                personality_pragmatic: None,
            }),
        }));

        let instructions = model.get_model_instructions(Some(Personality::Friendly));

        assert_eq!(instructions, "base");
    }

    #[test]
    fn get_personality_message_returns_default_when_personality_is_none() {
        let personality_template = personality_variables();
        assert_eq!(
            personality_template.get_personality_message(None),
            Some("default".to_owned())
        );
    }

    #[test]
    fn get_personality_message() {
        let personality_variables = personality_variables();
        assert_eq!(
            personality_variables.get_personality_message(Some(Personality::Friendly)),
            Some("friendly".to_owned())
        );
        assert_eq!(
            personality_variables.get_personality_message(Some(Personality::Pragmatic)),
            Some("pragmatic".to_owned())
        );
        assert_eq!(
            personality_variables.get_personality_message(None),
            Some("default".to_owned())
        );

        let personality_variables = ModelInstructionsVariables {
            personality_default: Some("default".to_owned()),
            personality_friendly: None,
            personality_pragmatic: None,
        };
        assert_eq!(
            personality_variables.get_personality_message(Some(Personality::Friendly)),
            None
        );
        assert_eq!(
            personality_variables.get_personality_message(Some(Personality::Pragmatic)),
            None
        );
        assert_eq!(
            personality_variables.get_personality_message(None),
            Some("default".to_owned())
        );

        let personality_variables = ModelInstructionsVariables {
            personality_default: None,
            personality_friendly: Some("friendly".to_owned()),
            personality_pragmatic: Some("pragmatic".to_owned()),
        };
        assert_eq!(
            personality_variables.get_personality_message(Some(Personality::Friendly)),
            Some("friendly".to_owned())
        );
        assert_eq!(
            personality_variables.get_personality_message(Some(Personality::Pragmatic)),
            Some("pragmatic".to_owned())
        );
        assert_eq!(personality_variables.get_personality_message(None), None);
    }

    #[test]
    fn model_info_new_sets_required_fields_and_defaults_the_rest() {
        let model = ModelInfo::new("gpt-test", "GPT Test");

        assert_eq!(model.slug, "gpt-test");
        assert_eq!(model.name, "GPT Test");
        assert_eq!(model.priority, 99);
        assert!(model.supported_in_api);
        assert_eq!(model.visibility, ModelVisibility::None);
        assert_eq!(model.shell_type, ConfigShellToolType::Default);
    }

    #[test]
    fn deserialize_model_info_with_only_required_fields_uses_defaults() {
        let value = json!({
            "slug": "gpt-test",
            "name": "GPT Test"
        });
        let model: ModelInfo = serde_json::from_value(value).expect("model should deserialize");

        assert_eq!(model.slug, "gpt-test");
        assert_eq!(model.name, "GPT Test");
        assert_eq!(model.priority, 99);
        assert!(model.supported_in_api);
        assert_eq!(
            model.supported_reasoning_levels,
            Vec::<ReasoningEffortPreset>::new()
        );
        assert_eq!(
            model.truncation_policy,
            TruncationPolicyConfig::bytes(10_000)
        );
        assert_eq!(model.input_modalities, default_input_modalities());
    }
}
