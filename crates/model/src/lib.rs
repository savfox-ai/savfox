//! Shared provider registry data used by multiple Savfox crates.

mod model_info;

use std::collections::HashSet;

use savfox_protocol::openai_models::ModelInfo;

// Pure provider-id helpers live in `savfox-utils` so the WASM frontend can
// reuse them without depending on `savfox-protocol`. Re-export here for the
// existing `savfox_model::canonical_provider_id` import paths.
pub use savfox_utils::provider_id::{
    DEFAULT_OPENAI_API_BASE_URL, canonical_provider_id, provider_default_base_url,
    provider_default_base_url_entry,
};

pub use model_info::{BASE_INSTRUCTIONS, find_model_info_for_slug};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    pub base_url: Option<&'static str>,
    pub models: &'static [ModelInfo],
}

const EMPTY_PROVIDER_MODELS: &[ModelInfo] = &[];

/// Build a `ModelInfo` using model-registry defaults, while requiring only the
/// fields needed for provider model lists.
#[must_use]
pub fn model_info_with_defaults(slug: &str, name: &str) -> ModelInfo {
    let mut model = find_model_info_for_slug(slug);
    model.slug = slug.to_owned();
    model.name = name.to_owned();
    model
}

#[must_use]
pub fn provider_default_models(_provider_id: &str) -> &'static [ModelInfo] {
    EMPTY_PROVIDER_MODELS
}

#[must_use]
pub fn provider_default_model_slug(_provider_id: &str) -> Option<&'static str> {
    None
}

#[must_use]
pub fn provider_registry(provider_id: &str) -> Option<Provider> {
    let base_url = provider_default_base_url_entry(provider_id)?;
    Some(Provider {
        base_url,
        models: provider_default_models(provider_id),
    })
}

/// Resolve detailed model metadata from a `(provider_id, model_slug)` pair.
///
/// Returns `None` when the provider is unknown. For known providers that do not
/// expose an explicit default model list yet, this falls back to generic model
/// metadata derived from `model_slug`.
#[must_use]
pub fn provider_model_info(provider_id: &str, model_slug: &str) -> Option<ModelInfo> {
    let slug = model_slug.trim();
    if slug.is_empty() {
        return None;
    }

    let canonical = canonical_provider_id(provider_id);
    provider_default_base_url_entry(&canonical)?;

    if let Some(model) = provider_default_models(&canonical)
        .iter()
        .find(|model| model.slug == slug)
    {
        return Some(model.clone());
    }

    Some(model_info_with_defaults(slug, slug))
}

/// Expand a provider's model slug list into detailed model metadata.
///
/// Unknown providers are ignored. Duplicate slugs are removed while preserving
/// the first occurrence order.
#[must_use]
pub fn provider_models_from_slugs(provider_id: &str, model_slugs: &[String]) -> Vec<ModelInfo> {
    let mut seen = HashSet::new();
    model_slugs
        .iter()
        .filter_map(|slug| {
            let trimmed = slug.trim();
            if trimmed.is_empty() || !seen.insert(trimmed.to_owned()) {
                return None;
            }
            provider_model_info(provider_id, trimmed)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_OPENAI_API_BASE_URL, canonical_provider_id, provider_default_base_url,
        provider_default_base_url_entry, provider_model_info, provider_models_from_slugs,
        provider_registry,
    };

    #[test]
    fn canonical_provider_aliases() {
        assert_eq!(canonical_provider_id("chatgpt"), "openai");
        assert_eq!(canonical_provider_id("zhipu-ai"), "zhipuai");
        assert_eq!(
            canonical_provider_id("zhipu-ai-coding-plan"),
            "zhipuai-coding-plan"
        );
        assert_eq!(canonical_provider_id("ark"), "volcengine");
        assert_eq!(canonical_provider_id("gemini"), "google");
        assert_eq!(canonical_provider_id("qwen"), "alibaba");
    }

    #[test]
    fn default_base_url_includes_openai() {
        assert_eq!(
            provider_default_base_url("openai"),
            Some(DEFAULT_OPENAI_API_BASE_URL)
        );
    }

    #[test]
    fn provider_other_has_entry_but_no_default_url() {
        assert_eq!(provider_default_base_url_entry("other"), Some(None));
        assert_eq!(provider_default_base_url("other"), None);
    }

    #[test]
    fn volcengine_uses_coding_plan_base_url() {
        assert_eq!(
            provider_default_base_url("volcengine"),
            Some("https://ark.cn-beijing.volces.com/api/coding/v3")
        );
    }

    #[test]
    fn provider_registry_resolves_known_provider() {
        let registry = provider_registry("openai").expect("openai should be known");
        assert_eq!(registry.base_url, Some(DEFAULT_OPENAI_API_BASE_URL));
    }

    #[test]
    fn provider_model_info_resolves_known_provider_and_slug() {
        let model = provider_model_info("openai", "some-model")
            .expect("openai provider should resolve model info");
        assert_eq!(model.slug, "some-model");
    }

    #[test]
    fn provider_model_info_rejects_unknown_provider() {
        assert!(provider_model_info("unknown-provider", "some-model").is_none());
    }

    #[test]
    fn provider_models_from_slugs_dedupes_and_preserves_order() {
        let models = provider_models_from_slugs(
            "openai",
            &[
                "model-a".to_owned(),
                "model-a".to_owned(),
                "model-b".to_owned(),
            ],
        );
        let slugs: Vec<&str> = models.iter().map(|model| model.slug.as_str()).collect();
        assert_eq!(slugs, vec!["model-a", "model-b"]);
    }
}
