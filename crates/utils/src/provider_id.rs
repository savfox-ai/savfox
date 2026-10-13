//! Pure provider-id helpers shared between native crates and the WASM frontend.
//!
//! `canonical_provider_id` collapses common aliases (e.g. `"chatgpt" -> "openai"`,
//! `"gemini" -> "google"`) into the registry id used everywhere else in the
//! codebase. `provider_default_base_url{,_entry}` returns the OpenAI-compatible
//! endpoint we ship for that provider; values containing `${VAR}` placeholders
//! are intended for substitution by the caller.
//!
//! Lives in `savfox-utils` so both `savfox-model` (native) and the Dioxus
//! gateway frontend (wasm32) can share a single source of truth instead of
//! maintaining parallel copies that drift.

pub const DEFAULT_OPENAI_API_BASE_URL: &str = "https://api.openai.com/v1";

#[must_use]
pub fn canonical_provider_id(provider_id: &str) -> String {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "chatgpt" | "chat-gpt" => "openai".to_owned(),
        "zhipu" | "zhipu-ai" => "zhipuai".to_owned(),
        "zhipu-coding-plan" | "zhipu-ai-coding-plan" => "zhipuai-coding-plan".to_owned(),
        "volc" | "volc-engine" | "ark" => "volcengine".to_owned(),
        // Moonshot ships two distinct products: the general Kimi/Moonshot
        // open platform (`platform.moonshot.ai`) and the Kimi Code coding
        // subscription (`api.kimi.com/coding`). A bare `kimi` follows the
        // kimi.com branding and resolves to the coding plan.
        "kimi" | "kimi-code" | "kimi-coding" | "kimi-for-code" | "kimicode" => {
            "kimi-for-coding".to_owned()
        }
        "moonshot" | "moonshot-ai" => "moonshotai".to_owned(),
        "moonshot-cn" | "moonshot-ai-cn" => "moonshotai-cn".to_owned(),
        "together" | "together-ai" => "togetherai".to_owned(),
        "gemini" => "google".to_owned(),
        "bedrock" => "amazon-bedrock".to_owned(),
        "qwen" => "alibaba".to_owned(),
        "googlevertex" | "google_vertex" => "google-vertex".to_owned(),
        "google_vertex_anthropic" => "google-vertex-anthropic".to_owned(),
        other => other.to_owned(),
    }
}

/// Three-state result so callers can distinguish "no entry for this provider"
/// (`None`) from "registered but no default URL" (`Some(None)` — currently only
/// `"other"`, the catch-all custom OpenAI-compatible slot).
#[must_use]
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
        "volcengine" => Some(Some("https://ark.cn-beijing.volces.com/api/coding/v3")),
        _ => None,
    }
}

#[must_use]
pub fn provider_default_base_url(provider_id: &str) -> Option<&'static str> {
    provider_default_base_url_entry(provider_id).flatten()
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_OPENAI_API_BASE_URL, canonical_provider_id, provider_default_base_url,
        provider_default_base_url_entry,
    };

    #[test]
    fn canonical_aliases_collapse() {
        assert_eq!(canonical_provider_id("chatgpt"), "openai");
        assert_eq!(canonical_provider_id("zhipu-ai"), "zhipuai");
        assert_eq!(canonical_provider_id("ark"), "volcengine");
        assert_eq!(canonical_provider_id("gemini"), "google");
        assert_eq!(canonical_provider_id("qwen"), "alibaba");
        assert_eq!(canonical_provider_id("kimi"), "kimi-for-coding");
        assert_eq!(canonical_provider_id("Kimi-For-Code"), "kimi-for-coding");
        assert_eq!(canonical_provider_id("moonshot"), "moonshotai");
        assert_eq!(canonical_provider_id("moonshot-cn"), "moonshotai-cn");
    }

    #[test]
    fn unknown_provider_passthrough() {
        assert_eq!(
            canonical_provider_id("never-heard-of-it"),
            "never-heard-of-it"
        );
        assert_eq!(provider_default_base_url("never-heard-of-it"), None);
        assert_eq!(provider_default_base_url_entry("never-heard-of-it"), None);
    }

    #[test]
    fn other_provider_has_explicit_no_url() {
        assert_eq!(provider_default_base_url_entry("other"), Some(None));
        assert_eq!(provider_default_base_url("other"), None);
    }

    #[test]
    fn openai_default_matches_const() {
        assert_eq!(
            provider_default_base_url("openai"),
            Some(DEFAULT_OPENAI_API_BASE_URL)
        );
    }
}
