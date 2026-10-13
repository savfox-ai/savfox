use std::path::Path;

use chrono::{DateTime, Utc};
use savfox_protocol::openai_models::{
    ModelInfo, ModelVisibility, ReasoningEffort, ReasoningEffortPreset,
};
use serde_json::json;

/// The catalog a test sees unless it publishes its own.
///
/// Nothing ships bundled with the binary any more, so tests that merely need
/// *some* models on offer share this fixture instead of reaching for a built-in
/// list that no longer exists. One listed model and one hidden one keep the
/// picker-visibility filter exercised.
#[must_use]
pub fn test_catalog() -> Vec<ModelInfo> {
    vec![
        catalog_model("test-model-primary", ModelVisibility::List, 0),
        catalog_model("test-model-hidden", ModelVisibility::Hide, 1),
    ]
}

fn catalog_model(slug: &str, visibility: ModelVisibility, priority: i32) -> ModelInfo {
    let mut model = savfox_model::find_model_info_for_slug(slug);
    model.name = slug.to_owned();
    model.description = Some(format!("{slug} description"));
    model.default_reasoning_level = Some(ReasoningEffort::Medium);
    model.supported_reasoning_levels = vec![
        ReasoningEffortPreset {
            effort: ReasoningEffort::Low,
            description: "Fast responses with lighter reasoning".to_owned(),
        },
        ReasoningEffortPreset {
            effort: ReasoningEffort::Medium,
            description: "Balances speed and reasoning depth".to_owned(),
        },
    ];
    model.visibility = visibility;
    model.priority = priority;
    // Written straight into a cache file, so it stands in for real catalog
    // metadata rather than the fallback it was built from.
    model.used_fallback_model_metadata = false;
    model
}

/// Write a models_cache.json file to the savfox home directory.
///
/// This keeps `ModelsManager` from making network requests to refresh models:
/// the cache is treated as fresh (within TTL) and used instead of fetching.
pub fn write_models_cache(savfox_home: &Path) -> std::io::Result<()> {
    write_models_cache_with_models(savfox_home, test_catalog())
}

/// Write a models_cache.json file with specific models.
/// Useful when tests need specific models to be available.
pub fn write_models_cache_with_models(
    savfox_home: &Path,
    models: Vec<ModelInfo>,
) -> std::io::Result<()> {
    let cache_path = savfox_home.join("models_cache.json");
    // DateTime<Utc> serializes to RFC3339 format by default with serde
    let fetched_at: DateTime<Utc> = Utc::now();
    let client_version = savfox_core::models_manager::client_version_to_whole();
    let cache = json!({
        "fetched_at": fetched_at,
        "etag": null,
        "client_version": client_version,
        "models": models
    });
    std::fs::write(cache_path, serde_json::to_string_pretty(&cache)?)
}
