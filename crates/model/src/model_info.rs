use savfox_protocol::config_types::Verbosity;
use savfox_protocol::openai_models::{
    ApplyPatchToolType, ConfigShellToolType, ModelInfo, ModelInstructionsVariables, ModelMessages,
    ModelVisibility, ReasoningEffort, TruncationPolicyConfig, default_input_modalities,
};

pub const BASE_INSTRUCTIONS: &str = include_str!("../prompt.md");

const COMPACT_INSTRUCTIONS: &str = include_str!("../compact_prompt.md");
const COMPACT_INSTRUCTIONS_TEMPLATE: &str =
    include_str!("../templates/model_instructions/instructions_template.md");

const PERSONALITY_FRIENDLY: &str = include_str!("../templates/personalities/friendly.md");
const PERSONALITY_PRAGMATIC: &str = include_str!("../templates/personalities/pragmatic.md");

const CONTEXT_WINDOW_272K: i64 = 272_000;

macro_rules! model_info {
    (
        $slug:expr $(, $key:ident : $value:expr )* $(,)?
    ) => {{
        #[allow(unused_mut)]
        let mut model = ModelInfo {
            slug: $slug.to_string(),
            name: $slug.to_string(),
            description: None,
            // This is primarily used when remote metadata is available. When running
            // offline, core generally omits the effort field unless explicitly
            // configured by the user.
            default_reasoning_level: None,
            supported_reasoning_levels: Vec::new(),
            shell_type: ConfigShellToolType::Default,
            visibility: ModelVisibility::None,
            supported_in_api: true,
            priority: 99,
            upgrade: None,
            base_instructions: BASE_INSTRUCTIONS.to_string(),
            model_messages: None,
            supports_reasoning_summaries: false,
            support_verbosity: false,
            default_verbosity: None,
            apply_patch_tool_type: None,
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
            supports_parallel_tool_calls: false,
            context_window: Some(CONTEXT_WINDOW_272K),
            // Use the registry's resolved fallback (context_window/2 capped
            // at 64K) — see `ModelInfo::resolved_max_output_tokens`. Concrete
            // models can override by setting this field via the macro.
            max_output_tokens: None,
            auto_compact_token_limit: None,
            effective_context_window_percent: 95,
            experimental_supported_tools: Vec::new(),
            input_modalities: default_input_modalities(),
        };

        $(
            model.$key = $value;
        )*
        model
    }};
}

#[must_use]
pub fn find_model_info_for_slug(slug: &str) -> ModelInfo {
    if slug.starts_with("exp-savfox") || slug.starts_with("savfox-1p") {
        model_info!(
            slug,
            base_instructions: COMPACT_INSTRUCTIONS.to_owned(),
            model_messages: Some(ModelMessages {
                instructions_template: Some(COMPACT_INSTRUCTIONS_TEMPLATE.to_owned()),
                instructions_variables: Some(ModelInstructionsVariables {
                    personality_default: Some("".to_owned()),
                    personality_friendly: Some(PERSONALITY_FRIENDLY.to_owned()),
                    personality_pragmatic: Some(PERSONALITY_PRAGMATIC.to_owned()),
                }),
            }),
            apply_patch_tool_type: Some(ApplyPatchToolType::Freeform),
            shell_type: ConfigShellToolType::ShellCommand,
            supports_parallel_tool_calls: true,
            supports_reasoning_summaries: true,
            support_verbosity: false,
            truncation_policy: TruncationPolicyConfig::tokens(10_000),
            context_window: Some(CONTEXT_WINDOW_272K),
        )
    } else if slug.starts_with("exp-5.1") {
        // exp-5.1 defaults to the unified exec shell tool variant and ships
        // apply_patch enabled.
        model_info!(
            slug,
            apply_patch_tool_type: Some(ApplyPatchToolType::Freeform),
            shell_type: ConfigShellToolType::UnifiedExec,
            supports_parallel_tool_calls: true,
            supports_reasoning_summaries: true,
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
        )
    } else if slug.starts_with("gpt-5.1-savfox") {
        model_info!(
            slug,
            apply_patch_tool_type: Some(ApplyPatchToolType::Freeform),
            default_reasoning_level: Some(ReasoningEffort::Medium),
            shell_type: ConfigShellToolType::ShellCommand,
            supports_parallel_tool_calls: true,
            supports_reasoning_summaries: true,
            truncation_policy: TruncationPolicyConfig::tokens(10_000),
        )
    } else if slug == "gpt-5.1" {
        model_info!(
            slug,
            apply_patch_tool_type: Some(ApplyPatchToolType::Freeform),
            default_reasoning_level: Some(ReasoningEffort::Medium),
            shell_type: ConfigShellToolType::ShellCommand,
            supports_parallel_tool_calls: true,
            supports_reasoning_summaries: true,
            support_verbosity: true,
            default_verbosity: Some(Verbosity::Low),
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
        )
    } else if slug.starts_with("gpt-5.1") {
        // GPT-5.1 series ship the shell_command tool variant and include the
        // apply_patch tool by default.
        model_info!(
            slug,
            apply_patch_tool_type: Some(ApplyPatchToolType::Freeform),
            default_reasoning_level: Some(ReasoningEffort::Medium),
            shell_type: ConfigShellToolType::ShellCommand,
            supports_parallel_tool_calls: true,
            supports_reasoning_summaries: true,
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
        )
    } else if slug.starts_with("test-") {
        // Internal test models expose experimental tools so test cases can
        // exercise tool-routing without depending on remote metadata.
        model_info!(
            slug,
            supports_parallel_tool_calls: true,
            supports_reasoning_summaries: true,
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
            experimental_supported_tools: vec![
                "test_sync_tool".to_owned(),
                "read_file".to_owned(),
                "grep_files".to_owned(),
                "list_dir".to_owned(),
            ],
        )
    } else {
        // General fallback for any model
        model_info!(
            slug,
            supports_parallel_tool_calls: true,
            supports_reasoning_summaries: true,
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
        )
    }
}
