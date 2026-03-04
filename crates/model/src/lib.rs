//! Shared provider registry data used by multiple Savfox crates.

mod bundled_models;
mod model_info;

use std::collections::HashSet;

use once_cell::sync::Lazy;
use savfox_protocol::openai_models::ModelInfo;

pub const DEFAULT_OPENAI_API_BASE_URL: &str = "https://api.openai.com/v1";
pub use bundled_models::{bundled_models, bundled_models_json, bundled_models_response};
pub use model_info::{
    BASE_INSTRUCTIONS, BASE_INSTRUCTIONS_WITH_APPLY_PATCH, find_model_info_for_slug,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    pub base_url: Option<&'static str>,
    pub models: &'static [ModelInfo],
}

const OPENAI_DEFAULT_MODEL_SLUG: &str = "gpt-5.3-codex";
const EMPTY_PROVIDER_MODELS: &[ModelInfo] = &[];

/// Build a `ModelInfo` using model-registry defaults, while requiring only the
/// fields needed for provider model lists.
pub fn model_info_with_defaults(slug: &str, name: &str) -> ModelInfo {
    let mut model = find_model_info_for_slug(slug);
    model.slug = slug.to_string();
    model.name = name.to_string();
    model
}

pub static OPENAI_DEFAULT_MODELS: Lazy<Vec<ModelInfo>> = Lazy::new(|| {
    vec![
        model_info_with_defaults("gpt-5.3-codex", "gpt-5.3-codex"),
        model_info_with_defaults("gpt-5.2-codex", "gpt-5.2-codex"),
        model_info_with_defaults("gpt-5.2", "gpt-5.2"),
        model_info_with_defaults("gpt-5.1-codex-max", "gpt-5.1-codex-max"),
        model_info_with_defaults("gpt-5.1-codex-mini", "gpt-5.1-codex-mini"),
        model_info_with_defaults("o3", "o3"),
        model_info_with_defaults("gpt-4.1", "gpt-4.1"),
    ]
});

pub fn canonical_provider_id(provider_id: &str) -> String {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "chatgpt" | "chat-gpt" => "openai".to_string(),
        "zhipu" | "zhipu-ai" => "zhipuai".to_string(),
        "zhipu-coding-plan" | "zhipu-ai-coding-plan" => "zhipuai-coding-plan".to_string(),
        "together" | "together-ai" => "togetherai".to_string(),
        "gemini" => "google".to_string(),
        "bedrock" => "amazon-bedrock".to_string(),
        "qwen" => "alibaba".to_string(),
        "googlevertex" | "google_vertex" => "google-vertex".to_string(),
        "google_vertex_anthropic" => "google-vertex-anthropic".to_string(),
        other => other.to_string(),
    }
}

pub fn provider_default_base_url_entry(provider_id: &str) -> Option<Option<&'static str>> {
    let canonical = canonical_provider_id(provider_id);
    match canonical.as_str() {
        "abacus" => Some(Some("https://routellm.abacus.ai/v1")),
        "aihubmix" => Some(Some("https://aihubmix.com/v1")),
        "alibaba" => Some(Some(
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        )),
        "alibaba-cn" => Some(Some("https://dashscope.aliyuncs.com/compatible-mode/v1")),
        "amazon-bedrock" => Some(Some("https://bedrock-runtime.us-east-1.amazonaws.com")),
        "anthropic" => Some(Some("https://api.anthropic.com")),
        "azure" => Some(Some(
            "https://${AZURE_RESOURCE_NAME}.openai.azure.com/openai",
        )),
        "azure-cognitive-services" => Some(Some(
            "https://${AZURE_COGNITIVE_SERVICES_RESOURCE_NAME}.cognitiveservices.azure.com/openai",
        )),
        "bailing" => Some(Some("https://api.tbox.cn/api/llm/v1/chat/completions")),
        "baseten" => Some(Some("https://inference.baseten.co/v1")),
        "cerebras" => Some(Some("https://api.cerebras.ai/v1")),
        "chutes" => Some(Some("https://llm.chutes.ai/v1")),
        "cloudflare-ai-gateway" => Some(Some(
            "https://gateway.ai.cloudflare.com/v1/${CLOUDFLARE_ACCOUNT_ID}/${CLOUDFLARE_GATEWAY_ID}/openai",
        )),
        "cloudflare-workers-ai" => Some(Some(
            "https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/ai/v1",
        )),
        "cohere" => Some(Some("https://api.cohere.com/v2")),
        "cortecs" => Some(Some("https://api.cortecs.ai/v1")),
        "deepinfra" => Some(Some("https://api.deepinfra.com/v1/openai")),
        "deepseek" => Some(Some("https://api.deepseek.com")),
        "fastrouter" => Some(Some("https://go.fastrouter.ai/api/v1")),
        "fireworks-ai" => Some(Some("https://api.fireworks.ai/inference/v1")),
        "friendli" => Some(Some("https://api.friendli.ai/serverless/v1")),
        "github-copilot" => Some(Some("https://api.githubcopilot.com")),
        "github-copilot-enterprise" => Some(Some("https://api.githubcopilot.com")),
        "github-models" => Some(Some("https://models.github.ai/inference")),
        "gitlab" => Some(Some("https://cloud.gitlab.com/ai/v1/proxy/openai/v1")),
        "google" => Some(Some(
            "https://generativelanguage.googleapis.com/v1beta/openai",
        )),
        "google-vertex" => Some(Some(
            "https://${GOOGLE_VERTEX_LOCATION}-aiplatform.googleapis.com/v1beta1/projects/${GOOGLE_VERTEX_PROJECT}/locations/${GOOGLE_VERTEX_LOCATION}/publishers/google",
        )),
        "google-vertex-anthropic" => Some(Some(
            "https://${GOOGLE_VERTEX_LOCATION}-aiplatform.googleapis.com/v1/projects/${GOOGLE_VERTEX_PROJECT}/locations/${GOOGLE_VERTEX_LOCATION}/publishers/anthropic/models",
        )),
        "groq" => Some(Some("https://api.groq.com/openai/v1")),
        "helicone" => Some(Some("https://ai-gateway.helicone.ai/v1")),
        "huggingface" => Some(Some("https://router.huggingface.co/v1")),
        "iflowcn" => Some(Some("https://apis.iflow.cn/v1")),
        "inception" => Some(Some("https://api.inceptionlabs.ai/v1")),
        "inference" => Some(Some("https://inference.net/v1")),
        "io-net" => Some(Some("https://api.intelligence.io.solutions/api/v1")),
        "kimi-for-coding" => Some(Some("https://api.kimi.com/coding/v1")),
        "llama" => Some(Some("https://api.llama.com/compat/v1")),
        "lmstudio" => Some(Some("http://localhost:1234/v1")),
        "lucidquery" => Some(Some("https://lucidquery.com/api/v1")),
        "minimax" => Some(Some("https://api.minimax.io/anthropic/v1")),
        "minimax-cn" => Some(Some("https://api.minimaxi.com/anthropic/v1")),
        "mistral" => Some(Some("https://api.mistral.ai/v1")),
        "modelscope" => Some(Some("https://api-inference.modelscope.cn/v1")),
        "moonshotai" => Some(Some("https://api.moonshot.ai/v1")),
        "moonshotai-cn" => Some(Some("https://api.moonshot.cn/v1")),
        "morph" => Some(Some("https://api.morphllm.com/v1")),
        "nano-gpt" => Some(Some("https://nano-gpt.com/api/v1")),
        "nebius" => Some(Some("https://api.tokenfactory.nebius.com/v1")),
        "nvidia" => Some(Some("https://integrate.api.nvidia.com/v1")),
        "ollama" => Some(Some("http://localhost:11434/v1")),
        "ollama-chat" => Some(Some("http://localhost:11434/v1")),
        "ollama-cloud" => Some(Some("https://ollama.com/v1")),
        "openai" => Some(Some(DEFAULT_OPENAI_API_BASE_URL)),
        "opencode" => Some(Some("https://opencode.ai/zen/v1")),
        "openrouter" => Some(Some("https://openrouter.ai/api/v1")),
        "other" => Some(None),
        "ovhcloud" => Some(Some("https://oai.endpoints.kepler.ai.cloud.ovh.net/v1")),
        "perplexity" => Some(Some("https://api.perplexity.ai")),
        "poe" => Some(Some("https://api.poe.com/v1")),
        "requesty" => Some(Some("https://router.requesty.ai/v1")),
        "sap-ai-core" => Some(Some("https://<sap-ai-core-url>")),
        "scaleway" => Some(Some("https://api.scaleway.ai/v1")),
        "siliconflow" => Some(Some("https://api.siliconflow.com/v1")),
        "siliconflow-cn" => Some(Some("https://api.siliconflow.cn/v1")),
        "togetherai" => Some(Some("https://api.together.xyz/v1")),
        "upstage" => Some(Some("https://api.upstage.ai/v1/solar")),
        "v0" => Some(Some("https://api.v0.dev/v1")),
        "venice" => Some(Some("https://api.venice.ai/api/v1")),
        "vercel" => Some(Some("https://ai-gateway.vercel.sh/v3/ai")),
        "vultr" => Some(Some("https://api.vultrinference.com/v1")),
        "wandb" => Some(Some("https://api.inference.wandb.ai/v1")),
        "xai" => Some(Some("https://api.x.ai/v1")),
        "xiaomi" => Some(Some("https://api.xiaomimimo.com/v1")),
        "zai" => Some(Some("https://api.z.ai/api/paas/v4")),
        "zai-coding-plan" => Some(Some("https://api.z.ai/api/coding/paas/v4")),
        "zenmux" => Some(Some("https://zenmux.ai/api/anthropic/v1")),
        "zhipuai" => Some(Some("https://open.bigmodel.cn/api/paas/v4")),
        "zhipuai-coding-plan" => Some(Some("https://open.bigmodel.cn/api/coding/paas/v4")),
        _ => None,
    }
}

pub fn provider_default_base_url(provider_id: &str) -> Option<&'static str> {
    provider_default_base_url_entry(provider_id).flatten()
}

pub fn provider_default_models(provider_id: &str) -> &'static [ModelInfo] {
    let canonical = canonical_provider_id(provider_id);
    match canonical.as_str() {
        "openai" => OPENAI_DEFAULT_MODELS.as_slice(),
        _ => EMPTY_PROVIDER_MODELS,
    }
}

pub fn provider_default_model_slug(provider_id: &str) -> Option<&'static str> {
    let canonical = canonical_provider_id(provider_id);
    match canonical.as_str() {
        "openai" => Some(OPENAI_DEFAULT_MODEL_SLUG),
        _ => None,
    }
}

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

/// Expand a provider's enabled model slug list into detailed model metadata.
///
/// Unknown providers are ignored. Duplicate slugs are removed while preserving
/// the first occurrence order.
pub fn provider_models_from_enabled_slugs(
    provider_id: &str,
    enabled_models: &[String],
) -> Vec<ModelInfo> {
    let mut seen = HashSet::new();
    enabled_models
        .iter()
        .filter_map(|slug| {
            let trimmed = slug.trim();
            if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
                return None;
            }
            provider_model_info(provider_id, trimmed)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_OPENAI_API_BASE_URL, OPENAI_DEFAULT_MODELS, canonical_provider_id,
        provider_default_base_url, provider_default_base_url_entry, provider_default_model_slug,
        provider_default_models, provider_model_info, provider_models_from_enabled_slugs,
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
    fn openai_default_models_exposed() {
        assert_eq!(
            provider_default_models("openai"),
            OPENAI_DEFAULT_MODELS.as_slice()
        );
        assert_eq!(
            provider_default_models("chatgpt"),
            OPENAI_DEFAULT_MODELS.as_slice()
        );
        assert_eq!(
            provider_default_model_slug("openai"),
            Some(OPENAI_DEFAULT_MODELS[0].slug.as_str())
        );
    }

    #[test]
    fn provider_registry_includes_base_url_and_models() {
        let registry = provider_registry("openai").expect("openai should be known");
        assert_eq!(registry.base_url, Some(DEFAULT_OPENAI_API_BASE_URL));
        assert_eq!(registry.models, OPENAI_DEFAULT_MODELS.as_slice());
    }

    #[test]
    fn provider_model_info_resolves_known_provider_and_slug() {
        let model = provider_model_info("chatgpt", "gpt-5.3-codex")
            .expect("chatgpt provider should resolve to openai metadata");
        assert_eq!(model.slug, "gpt-5.3-codex");
    }

    #[test]
    fn provider_model_info_rejects_unknown_provider() {
        assert!(provider_model_info("unknown-provider", "gpt-5.3-codex").is_none());
    }

    #[test]
    fn provider_models_from_enabled_slugs_dedupes_and_preserves_order() {
        let models = provider_models_from_enabled_slugs(
            "openai",
            &[
                "gpt-5.3-codex".to_string(),
                "gpt-5.3-codex".to_string(),
                "gpt-5.2-codex".to_string(),
            ],
        );
        let slugs: Vec<&str> = models.iter().map(|model| model.slug.as_str()).collect();
        assert_eq!(slugs, vec!["gpt-5.3-codex", "gpt-5.2-codex"]);
    }
}
