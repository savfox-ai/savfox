//! Built-in model preset catalog.
//!
//! The catalog data lives as JSON in `model_presets.json` and is bundled
//! into the binary via `include_str!`. This mirrors the upstream codex
//! pattern (their `models-manager/models.json`) and replaces the previous
//! ~530-line hardcoded `Vec<ModelPreset>` literal — adding or rebranding a
//! model is now a JSON edit, not a Rust struct edit.
//!
//! The legacy migration prompt config keys below are kept for backward
//! compatibility with on-disk configs that reference them.

use once_cell::sync::Lazy;
use savfox_protocol::openai_models::ModelPreset;

use crate::auth::AuthMode;

pub const HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG: &str = "hide_gpt5_1_migration_prompt";
pub const HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG: &str =
    "hide_gpt-5.1-codex-max_migration_prompt";

const PRESETS_JSON: &str = include_str!("model_presets.json");

static PRESETS: Lazy<Vec<ModelPreset>> = Lazy::new(|| {
    serde_json::from_str(PRESETS_JSON).expect("bundled model_presets.json must deserialize")
});

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
    fn presets_json_deserializes() {
        // Forces the Lazy init to run; panics here surface a malformed catalog.
        let presets = &*PRESETS;
        assert!(
            !presets.is_empty(),
            "bundled catalog must have at least one preset"
        );
    }

    #[test]
    fn only_one_default_model_is_configured() {
        let default_models = PRESETS.iter().filter(|preset| preset.is_default).count();
        assert_eq!(
            default_models, 1,
            "exactly one preset must be is_default=true"
        );
    }
}
