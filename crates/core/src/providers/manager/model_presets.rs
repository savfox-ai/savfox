use once_cell::sync::Lazy;
use savfox_protocol::openai_models::{
    ModelPreset, ModelUpgrade, ReasoningEffort, ReasoningEffortPreset, default_input_modalities,
};

use crate::auth::AuthMode;

pub const HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG: &str = "hide_gpt5_1_migration_prompt";
pub const HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG: &str =
    "hide_gpt-5.1-codex-max_migration_prompt";

static PRESETS: Lazy<Vec<ModelPreset>> = Lazy::new(|| {
    vec![
        ModelPreset {
            id: "gpt-5.2-savfox".to_owned(),
            slug: "gpt-5.2-savfox".to_owned(),
            name: "gpt-5.2-savfox".to_owned(),
            description: "Latest frontier agentic coding model.".to_owned(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Low,
                    description: "Fast responses with lighter reasoning".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Medium,
                    description: "Balances speed and reasoning depth for everyday tasks".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::High,
                    description: "Greater reasoning depth for complex problems".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::XHigh,
                    description: "Extra high reasoning depth for complex problems".to_owned(),
                },
            ],
            supports_personality: true,
            is_default: true,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
            input_modalities: default_input_modalities(),
        },
        ModelPreset {
            id: "gpt-5.1-codex-max".to_owned(),
            slug: "gpt-5.1-codex-max".to_owned(),
            name: "gpt-5.1-codex-max".to_owned(),
            description: "Codex-optimized flagship for deep and fast reasoning.".to_owned(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Low,
                    description: "Fast responses with lighter reasoning".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Medium,
                    description: "Balances speed and reasoning depth for everyday tasks".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::High,
                    description: "Greater reasoning depth for complex problems".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::XHigh,
                    description: "Extra high reasoning depth for complex problems".to_owned(),
                },
            ],
            supports_personality: false,
            is_default: false,
            upgrade: Some(gpt_52_savfox_upgrade()),
            show_in_picker: true,
            supported_in_api: true,
            input_modalities: default_input_modalities(),
        },
        ModelPreset {
            id: "gpt-5.1-codex-mini".to_owned(),
            slug: "gpt-5.1-codex-mini".to_owned(),
            name: "gpt-5.1-codex-mini".to_owned(),
            description: "Optimized for codex. Cheaper, faster, but less capable.".to_owned(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Medium,
                    description: "Dynamically adjusts reasoning based on the task".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::High,
                    description: "Maximizes reasoning depth for complex or ambiguous problems".to_owned(),
                },
            ],
            supports_personality: false,
            is_default: false,
            upgrade: Some(gpt_52_savfox_upgrade()),
            show_in_picker: true,
            supported_in_api: true,
            input_modalities: default_input_modalities(),
        },
        ModelPreset {
            id: "gpt-5.2".to_owned(),
            slug: "gpt-5.2".to_owned(),
            name: "gpt-5.2".to_owned(),
            description: "Latest frontier model with improvements across knowledge, reasoning and coding".to_owned(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Low,
                    description: "Balances speed with some reasoning; useful for straightforward queries and short explanations".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Medium,
                    description: "Provides a solid balance of reasoning depth and latency for general-purpose tasks".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::High,
                    description: "Maximizes reasoning depth for complex or ambiguous problems".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::XHigh,
                    description: "Extra high reasoning depth for complex problems".to_owned(),
                },
            ],
            supports_personality: false,
            is_default: false,
            upgrade: Some(gpt_52_savfox_upgrade()),
            show_in_picker: true,
            supported_in_api: true,
            input_modalities: default_input_modalities(),
        },
        ModelPreset {
            id: "bengalfox".to_owned(),
            slug: "bengalfox".to_owned(),
            name: "bengalfox".to_owned(),
            description: "bengalfox".to_owned(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Low,
                    description: "Fast responses with lighter reasoning".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Medium,
                    description: "Balances speed and reasoning depth for everyday tasks".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::High,
                    description: "Greater reasoning depth for complex problems".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::XHigh,
                    description: "Extra high reasoning depth for complex problems".to_owned(),
                },
            ],
            supports_personality: true,
            is_default: false,
            upgrade: None,
            show_in_picker: false,
            supported_in_api: true,
            input_modalities: default_input_modalities(),
        },
        ModelPreset {
            id: "boomslang".to_owned(),
            slug: "boomslang".to_owned(),
            name: "boomslang".to_owned(),
            description: "boomslang".to_owned(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Low,
                    description: "Balances speed with some reasoning; useful for straightforward queries and short explanations".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Medium,
                    description: "Provides a solid balance of reasoning depth and latency for general-purpose tasks".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::High,
                    description: "Maximizes reasoning depth for complex or ambiguous problems".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::XHigh,
                    description: "Extra high reasoning depth for complex problems".to_owned(),
                },
            ],
            supports_personality: false,
            is_default: false,
            upgrade: None,
            show_in_picker: false,
            supported_in_api: true,
            input_modalities: default_input_modalities(),
        },
        // Deprecated models.
        ModelPreset {
            id: "gpt-5-codex".to_owned(),
            slug: "gpt-5-codex".to_owned(),
            name: "gpt-5-codex".to_owned(),
            description: "Optimized for codex.".to_owned(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Low,
                    description: "Fastest responses with limited reasoning".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Medium,
                    description: "Dynamically adjusts reasoning based on the task".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::High,
                    description: "Maximizes reasoning depth for complex or ambiguous problems".to_owned(),
                },
            ],
            supports_personality: false,
            is_default: false,
            upgrade: Some(gpt_52_savfox_upgrade()),
            show_in_picker: false,
            supported_in_api: true,
            input_modalities: default_input_modalities(),
        },
        ModelPreset {
            id: "gpt-5-codex-mini".to_owned(),
            slug: "gpt-5-codex-mini".to_owned(),
            name: "gpt-5-codex-mini".to_owned(),
            description: "Optimized for codex. Cheaper, faster, but less capable.".to_owned(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Medium,
                    description: "Dynamically adjusts reasoning based on the task".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::High,
                    description: "Maximizes reasoning depth for complex or ambiguous problems".to_owned(),
                },
            ],
            supports_personality: false,
            is_default: false,
            upgrade: Some(gpt_52_savfox_upgrade()),
            show_in_picker: false,
            supported_in_api: true,
            input_modalities: default_input_modalities(),
        },
        ModelPreset {
            id: "gpt-5.1-codex".to_owned(),
            slug: "gpt-5.1-codex".to_owned(),
            name: "gpt-5.1-codex".to_owned(),
            description: "Optimized for codex.".to_owned(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Low,
                    description: "Fastest responses with limited reasoning".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Medium,
                    description: "Dynamically adjusts reasoning based on the task".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::High,
                    description: "Maximizes reasoning depth for complex or ambiguous problems".to_owned(),
                },
            ],
            supports_personality: false,
            is_default: false,
            upgrade: Some(gpt_52_savfox_upgrade()),
            show_in_picker: false,
            supported_in_api: true,
            input_modalities: default_input_modalities(),
        },
        ModelPreset {
            id: "gpt-5".to_owned(),
            slug: "gpt-5".to_owned(),
            name: "gpt-5".to_owned(),
            description: "Broad world knowledge with strong general reasoning.".to_owned(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Minimal,
                    description: "Fastest responses with little reasoning".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Low,
                    description: "Balances speed with some reasoning; useful for straightforward queries and short explanations".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Medium,
                    description: "Provides a solid balance of reasoning depth and latency for general-purpose tasks".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::High,
                    description: "Maximizes reasoning depth for complex or ambiguous problems".to_owned(),
                },
            ],
            supports_personality: false,
            is_default: false,
            upgrade: Some(gpt_52_savfox_upgrade()),
            show_in_picker: false,
            supported_in_api: true,
            input_modalities: default_input_modalities(),
        },
        ModelPreset {
            id: "gpt-5.1".to_owned(),
            slug: "gpt-5.1".to_owned(),
            name: "gpt-5.1".to_owned(),
            description: "Broad world knowledge with strong general reasoning.".to_owned(),
            default_reasoning_effort: ReasoningEffort::Medium,
            supported_reasoning_efforts: vec![
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Low,
                    description: "Balances speed with some reasoning; useful for straightforward queries and short explanations".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::Medium,
                    description: "Provides a solid balance of reasoning depth and latency for general-purpose tasks".to_owned(),
                },
                ReasoningEffortPreset {
                    effort: ReasoningEffort::High,
                    description: "Maximizes reasoning depth for complex or ambiguous problems".to_owned(),
                },
            ],
            supports_personality: false,
            is_default: false,
            upgrade: Some(gpt_52_savfox_upgrade()),
            show_in_picker: false,
            supported_in_api: true,
            input_modalities: default_input_modalities(),
        },
    ]
});

fn gpt_52_savfox_upgrade() -> ModelUpgrade {
    ModelUpgrade {
        id: "gpt-5.2-savfox".to_owned(),
        reasoning_effort_mapping: None,
        migration_config_key: "gpt-5.2-savfox".to_owned(),
        model_link: Some("https://openai.com/index/introducing-gpt-5-2-savfox".to_owned()),
        upgrade_copy: Some(
            "Savfox is now powered by gpt-5.2-savfox, our latest frontier agentic coding model. It is smarter and faster than its predecessors and capable of long-running project-scale work.".to_owned(),
        ),
        migration_markdown: Some(
            r#"
                **Savfox just got an upgrade. Introducing {model_to}.**

                Savfox is now powered by gpt-5.2-savfox, our latest frontier agentic coding model. It is smarter and faster than its predecessors and capable of long-running project-scale work. Learn more about {model_to} at https://openai.com/index/introducing-gpt-5-2-savfox

                You can continue using {model_from} if you prefer.
            "#.to_owned(),
        ),
    }
}

pub(super) fn builtin_model_presets(_auth_mode: Option<AuthMode>) -> Vec<ModelPreset> {
    PRESETS.iter().cloned().collect()
}

#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn all_model_presets() -> &'static Vec<ModelPreset> {
    &PRESETS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_default_model_is_configured() {
        let default_models = PRESETS.iter().filter(|preset| preset.is_default).count();
        assert!(default_models == 1);
    }
}
