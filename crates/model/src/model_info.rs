use savfox_protocol::openai_models::{
    ConfigShellToolType, ModelInfo, ModelVisibility, TruncationPolicyConfig,
    default_input_modalities,
};

pub const BASE_INSTRUCTIONS: &str = include_str!("../prompt.md");

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
            used_fallback_model_metadata: false,
        };

        $(
            model.$key = $value;
        )*
        model
    }};
}

/// Conservative metadata for a model no catalog describes.
///
/// Capabilities are never inferred from the slug. A name says nothing about
/// what a model can do, and Savfox talks to a dozen providers whose naming has
/// no relation to OpenAI's — guessing from a prefix silently hands every one of
/// them a GPT-shaped feature set. Real metadata comes from the remote catalog
/// or from the provider store; this is only what is left when neither has an
/// entry, and it is marked as such via `used_fallback_model_metadata` so the
/// difference stays visible downstream.
#[must_use]
pub fn find_model_info_for_slug(slug: &str) -> ModelInfo {
    model_info!(
        slug,
        supports_parallel_tool_calls: true,
        supports_reasoning_summaries: true,
        truncation_policy: TruncationPolicyConfig::bytes(10_000),
        used_fallback_model_metadata: true,
    )
}
