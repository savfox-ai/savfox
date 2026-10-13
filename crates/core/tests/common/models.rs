//! Explicit model metadata for integration tests, independent of provider catalogs.

use std::path::Path;

use savfox_protocol::config_types::Verbosity;
use savfox_protocol::openai_models::{
    ApplyPatchToolType, ConfigShellToolType, ModelInfo, ModelVisibility, ReasoningEffort,
    TruncationPolicyConfig,
};

pub const DEFAULT_TEST_MODEL: &str = "gpt-5.1-savfox-max";

pub fn test_catalog() -> Vec<ModelInfo> {
    [
        DEFAULT_TEST_MODEL,
        "gpt-5.1-savfox",
        "gpt-5.1",
        "gpt-5",
        "gpt-5.2",
        "gpt-5.2-savfox",
        "savfox-mini-latest",
        "test-gpt-5-savfox",
        "test-gpt-5.1-savfox",
        "exp-5.1",
    ]
    .into_iter()
    .enumerate()
    .map(|(priority, slug)| {
        let mut model = savfox_model::find_model_info_for_slug(slug);
        model.used_fallback_model_metadata = false;
        model.visibility = ModelVisibility::List;
        model.priority = i32::try_from(priority).expect("small test catalog");
        if slug.starts_with("gpt-5.1") {
            model.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
            model.shell_type = ConfigShellToolType::ShellCommand;
            model.default_reasoning_level = Some(ReasoningEffort::Medium);
            if slug.contains("savfox") {
                model.truncation_policy = TruncationPolicyConfig::tokens(10_000);
            } else if slug == "gpt-5.1" {
                model.support_verbosity = true;
                model.default_verbosity = Some(Verbosity::Low);
            }
        }
        if slug == "exp-5.1" {
            model.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
            model.shell_type = ConfigShellToolType::UnifiedExec;
        }
        if slug.starts_with("test-") {
            model.experimental_supported_tools =
                ["test_sync_tool", "read_file", "grep_files", "list_dir"]
                    .map(str::to_owned)
                    .to_vec();
        }
        model
    })
    .collect()
}

pub fn write_test_catalog(home: &Path) -> std::io::Result<()> {
    let cache_path = home.join("models_cache.json");
    // A test may have already supplied metadata tailored to its assertions.
    if cache_path.exists() {
        return Ok(());
    }
    let cache = serde_json::json!({
        "fetched_at": chrono::Utc::now(),
        "etag": null,
        "client_version": savfox_core::models_manager::client_version_to_whole(),
        "models": test_catalog(),
    });
    std::fs::write(cache_path, serde_json::to_vec(&cache)?)
}
