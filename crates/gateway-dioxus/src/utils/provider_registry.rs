// Pure id/url helpers live in savfox-utils so native and WASM share the same
// source of truth.
pub use savfox_utils::provider_id::{canonical_provider_id, provider_default_base_url};

const POPULAR_PROVIDER_ORDER: [&str; 7] = [
    "opencode",
    "anthropic",
    "github-copilot",
    "openai",
    "google",
    "openrouter",
    "vercel",
];

// Synced with opencode's provider icon registry plus a few savfox-specific extras.
const KNOWN_PROVIDER_IDS: [&str; 79] = [
    "abacus",
    "aihubmix",
    "alibaba",
    "alibaba-cn",
    "amazon-bedrock",
    "anthropic",
    "azure",
    "azure-cognitive-services",
    "bailing",
    "baseten",
    "cerebras",
    "chutes",
    "cloudflare-ai-gateway",
    "cloudflare-workers-ai",
    "cohere",
    "cortecs",
    "deepinfra",
    "deepseek",
    "fastrouter",
    "fireworks-ai",
    "friendli",
    "github-copilot",
    "github-copilot-enterprise",
    "github-models",
    "gitlab",
    "google",
    "google-vertex",
    "google-vertex-anthropic",
    "groq",
    "helicone",
    "huggingface",
    "iflowcn",
    "inception",
    "inference",
    "io-net",
    "kimi-for-coding",
    "llama",
    "lmstudio",
    "lucidquery",
    "minimax",
    "minimax-cn",
    "mistral",
    "modelscope",
    "moonshotai",
    "moonshotai-cn",
    "morph",
    "nano-gpt",
    "nebius",
    "nvidia",
    "ollama",
    "ollama-chat",
    "ollama-cloud",
    "openai",
    "opencode",
    "openrouter",
    "other",
    "ovhcloud",
    "perplexity",
    "poe",
    "requesty",
    "sap-ai-core",
    "scaleway",
    "siliconflow",
    "siliconflow-cn",
    "togetherai",
    "upstage",
    "v0",
    "venice",
    "vercel",
    "vultr",
    "wandb",
    "xai",
    "xiaomi",
    "zai",
    "zai-coding-plan",
    "zenmux",
    "zhipuai",
    "zhipuai-coding-plan",
    "volcengine",
];

fn to_title_case(token: &str) -> String {
    let mut chars = token.chars();
    if let Some(first) = chars.next() {
        let mut out = String::new();
        out.push(first.to_ascii_uppercase());
        out.push_str(chars.as_str());
        out
    } else {
        String::new()
    }
}

fn humanize_token(token: &str) -> String {
    match token {
        "ai" => "AI".to_string(),
        "cn" => "CN".to_string(),
        "io" => "IO".to_string(),
        "github" => "GitHub".to_string(),
        "gpt" => "GPT".to_string(),
        "openai" => "OpenAI".to_string(),
        "opencode" => "OpenCode".to_string(),
        "zhipuai" => "Zhipu AI".to_string(),
        "zai" => "Z.ai".to_string(),
        "togetherai" => "Together AI".to_string(),
        "moonshotai" => "Moonshot AI".to_string(),
        "xai" => "xAI".to_string(),
        "lmstudio" => "LM Studio".to_string(),
        "iflowcn" => "iFlow CN".to_string(),
        "aihubmix" => "AIHubMix".to_string(),
        "minimax" => "MiniMax".to_string(),
        "ovhcloud" => "OVHcloud".to_string(),
        "wandb" => "Weights & Biases".to_string(),
        "v0" => "v0".to_string(),
        "sap" => "SAP".to_string(),
        _ => to_title_case(token),
    }
}

fn humanize_provider_id(provider_id: &str) -> String {
    provider_id
        .split('-')
        .map(humanize_token)
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn known_provider_ids() -> &'static [&'static str] {
    &KNOWN_PROVIDER_IDS
}

pub fn provider_sort_rank(provider_id: &str) -> usize {
    let canonical = canonical_provider_id(provider_id);
    POPULAR_PROVIDER_ORDER
        .iter()
        .position(|entry| *entry == canonical)
        .unwrap_or(usize::MAX)
}

pub fn provider_display_name(provider_id: &str) -> String {
    let canonical = canonical_provider_id(provider_id);
    match canonical.as_str() {
        "opencode" => "OpenCode Zen".to_string(),
        "github-copilot-enterprise" => "GitHub Copilot Enterprise".to_string(),
        "github-copilot" => "GitHub Copilot".to_string(),
        "github-models" => "GitHub Models".to_string(),
        "google-vertex" => "Google Vertex".to_string(),
        "google-vertex-anthropic" => "Google Vertex Anthropic".to_string(),
        "azure-cognitive-services" => "Azure Cognitive Services".to_string(),
        "cloudflare-workers-ai" => "Cloudflare Workers AI".to_string(),
        "cloudflare-ai-gateway" => "Cloudflare AI Gateway".to_string(),
        "fireworks-ai" => "Fireworks AI".to_string(),
        "io-net" => "IO.net".to_string(),
        "kimi-for-coding" => "Kimi for Coding".to_string(),
        "sap-ai-core" => "SAP AI Core".to_string(),
        "siliconflow-cn" => "SiliconFlow (CN)".to_string(),
        "minimax-cn" => "MiniMax (CN)".to_string(),
        "moonshotai-cn" => "Moonshot AI (CN)".to_string(),
        "alibaba-cn" => "Alibaba (CN)".to_string(),
        "zhipuai-coding-plan" => "Zhipu AI Coding Plan".to_string(),
        "zai-coding-plan" => "Z.ai Coding Plan".to_string(),
        "volcengine" => "Volcengine".to_string(),
        _ => humanize_provider_id(&canonical),
    }
}

pub fn provider_description(provider_id: &str) -> String {
    let canonical = canonical_provider_id(provider_id);
    match canonical.as_str() {
        "other" => "Custom OpenAI-compatible provider".to_string(),
        "ollama" | "ollama-chat" => "Run open models locally with Ollama".to_string(),
        "lmstudio" => "Run local models with LM Studio server".to_string(),
        "zhipuai-coding-plan" => "Zhipu AI coding plan models and tooling".to_string(),
        "zai-coding-plan" => "Z.ai coding plan models and tooling".to_string(),
        "volcengine" => "Volcengine coding plan models and tooling".to_string(),
        _ => format!("Connect {} models", provider_display_name(&canonical)),
    }
}

pub fn provider_needs_api_key(provider_id: &str) -> bool {
    let canonical = canonical_provider_id(provider_id);
    !matches!(canonical.as_str(), "ollama" | "ollama-chat" | "lmstudio")
}

pub fn provider_api_key_env(provider_id: &str) -> String {
    let canonical = canonical_provider_id(provider_id);
    match canonical.as_str() {
        "other" => "API_KEY".to_string(),
        "openai" => "OPENAI_API_KEY".to_string(),
        "anthropic" => "ANTHROPIC_API_KEY".to_string(),
        "groq" => "GROQ_API_KEY".to_string(),
        "deepseek" => "DEEPSEEK_API_KEY".to_string(),
        "xai" => "XAI_API_KEY".to_string(),
        "mistral" => "MISTRAL_API_KEY".to_string(),
        "togetherai" => "TOGETHER_API_KEY".to_string(),
        "openrouter" => "OPENROUTER_API_KEY".to_string(),
        "google" => "GOOGLE_API_KEY".to_string(),
        "minimax" | "minimax-cn" => "MINIMAX_API_KEY".to_string(),
        "alibaba" | "alibaba-cn" => "DASHSCOPE_API_KEY".to_string(),
        "zhipuai" | "zhipuai-coding-plan" => "ZHIPUAI_API_KEY".to_string(),
        "volcengine" => "ARK_API_KEY".to_string(),
        "github-copilot" | "github-copilot-enterprise" => "GITHUB_TOKEN".to_string(),
        "azure" | "azure-cognitive-services" => "AZURE_OPENAI_API_KEY".to_string(),
        "cohere" => "COHERE_API_KEY".to_string(),
        "perplexity" => "PERPLEXITY_API_KEY".to_string(),
        "cerebras" => "CEREBRAS_API_KEY".to_string(),
        "cloudflare-workers-ai" => "CLOUDFLARE_API_KEY".to_string(),
        "fireworks-ai" => "FIREWORKS_API_KEY".to_string(),
        _ => format!(
            "{}_API_KEY",
            canonical
                .replace('-', "_")
                .replace('.', "_")
                .to_ascii_uppercase()
        ),
    }
}

pub fn provider_api_key_help_url(provider_id: &str) -> Option<&'static str> {
    let canonical = canonical_provider_id(provider_id);
    match canonical.as_str() {
        "openai" => Some("https://platform.openai.com/api-keys"),
        "anthropic" => Some("https://console.anthropic.com/settings/keys"),
        "google" => Some("https://aistudio.google.com/app/apikey"),
        "groq" => Some("https://console.groq.com/keys"),
        "deepseek" => Some("https://platform.deepseek.com"),
        "xai" => Some("https://console.x.ai"),
        "mistral" => Some("https://console.mistral.ai/api-keys"),
        "togetherai" => Some("https://api.together.xyz/settings/api-keys"),
        "openrouter" => Some("https://openrouter.ai/keys"),
        "zhipuai" | "zhipuai-coding-plan" => Some("https://open.bigmodel.cn/"),
        "volcengine" => Some("https://console.volcengine.com/"),
        _ => None,
    }
}

pub fn provider_icon_text(provider_id: &str) -> String {
    let canonical = canonical_provider_id(provider_id);
    match canonical.as_str() {
        "opencode" => "OC".to_string(),
        "openai" => "OA".to_string(),
        "anthropic" => "AN".to_string(),
        "google" => "GO".to_string(),
        "github-copilot" => "GH".to_string(),
        "github-copilot-enterprise" => "GE".to_string(),
        "zhipuai" => "ZH".to_string(),
        "zhipuai-coding-plan" => "ZC".to_string(),
        "volcengine" => "VO".to_string(),
        "zai" => "ZA".to_string(),
        "zai-coding-plan" => "ZP".to_string(),
        _ => {
            let mut text = String::new();
            for ch in canonical.chars() {
                if ch.is_ascii_alphanumeric() {
                    text.push(ch.to_ascii_uppercase());
                }
                if text.len() >= 2 {
                    break;
                }
            }
            if text.is_empty() {
                "...".to_string()
            } else {
                text
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use savfox_utils::provider_id::provider_default_base_url_entry;

    use super::{
        canonical_provider_id, known_provider_ids, provider_api_key_env, provider_default_base_url,
    };

    #[test]
    fn default_base_url_map_covers_all_known_provider_ids() {
        for provider_id in known_provider_ids() {
            assert!(
                provider_default_base_url_entry(provider_id).is_some(),
                "expected default base url mapping entry for known provider id {provider_id}"
            );
        }
    }

    #[test]
    fn volcengine_registry_uses_ark_defaults() {
        assert_eq!(canonical_provider_id("ark"), "volcengine");
        assert_eq!(provider_api_key_env("volcengine"), "ARK_API_KEY");
        assert_eq!(
            provider_default_base_url("volcengine"),
            Some("https://ark.cn-beijing.volces.com/api/coding/v3")
        );
    }
}
