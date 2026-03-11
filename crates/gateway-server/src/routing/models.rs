use salvo::prelude::*;
use serde_json::json;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_streaming: Option<bool>,
}

fn get_builtin_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "gpt-4o".to_string(),
            provider: "openai".to_string(),
            name: "GPT-4o".to_string(),
            context_window: Some(128000),
            supports_vision: Some(true),
            supports_tools: Some(true),
            supports_streaming: Some(true),
        },
        ModelInfo {
            id: "gpt-4o-mini".to_string(),
            provider: "openai".to_string(),
            name: "GPT-4o Mini".to_string(),
            context_window: Some(128000),
            supports_vision: Some(true),
            supports_tools: Some(true),
            supports_streaming: Some(true),
        },
        ModelInfo {
            id: "o1".to_string(),
            provider: "openai".to_string(),
            name: "o1".to_string(),
            context_window: Some(200000),
            supports_vision: Some(true),
            supports_tools: Some(false),
            supports_streaming: Some(false),
        },
        ModelInfo {
            id: "o1-mini".to_string(),
            provider: "openai".to_string(),
            name: "o1 Mini".to_string(),
            context_window: Some(128000),
            supports_vision: Some(false),
            supports_tools: Some(false),
            supports_streaming: Some(false),
        },
        ModelInfo {
            id: "claude-4-sonnet".to_string(),
            provider: "anthropic".to_string(),
            name: "Claude 4 Sonnet".to_string(),
            context_window: Some(200000),
            supports_vision: Some(true),
            supports_tools: Some(true),
            supports_streaming: Some(true),
        },
        ModelInfo {
            id: "claude-3-5-sonnet".to_string(),
            provider: "anthropic".to_string(),
            name: "Claude 3.5 Sonnet".to_string(),
            context_window: Some(200000),
            supports_vision: Some(true),
            supports_tools: Some(true),
            supports_streaming: Some(true),
        },
        ModelInfo {
            id: "claude-3-5-haiku".to_string(),
            provider: "anthropic".to_string(),
            name: "Claude 3.5 Haiku".to_string(),
            context_window: Some(200000),
            supports_vision: Some(true),
            supports_tools: Some(true),
            supports_streaming: Some(true),
        },
        ModelInfo {
            id: "gemini-2.0-flash".to_string(),
            provider: "google".to_string(),
            name: "Gemini 2.0 Flash".to_string(),
            context_window: Some(1000000),
            supports_vision: Some(true),
            supports_tools: Some(true),
            supports_streaming: Some(true),
        },
        ModelInfo {
            id: "gemini-2.0-pro".to_string(),
            provider: "google".to_string(),
            name: "Gemini 2.0 Pro".to_string(),
            context_window: Some(200000),
            supports_vision: Some(true),
            supports_tools: Some(true),
            supports_streaming: Some(true),
        },
        ModelInfo {
            id: "deepseek-chat".to_string(),
            provider: "deepseek".to_string(),
            name: "DeepSeek Chat".to_string(),
            context_window: Some(64000),
            supports_vision: Some(false),
            supports_tools: Some(true),
            supports_streaming: Some(true),
        },
        ModelInfo {
            id: "deepseek-reasoner".to_string(),
            provider: "deepseek".to_string(),
            name: "DeepSeek Reasoner".to_string(),
            context_window: Some(64000),
            supports_vision: Some(false),
            supports_tools: Some(true),
            supports_streaming: Some(true),
        },
        ModelInfo {
            id: "llama3.2".to_string(),
            provider: "ollama".to_string(),
            name: "Llama 3.2".to_string(),
            context_window: Some(128000),
            supports_vision: Some(true),
            supports_tools: Some(true),
            supports_streaming: Some(true),
        },
        ModelInfo {
            id: "qwen2.5".to_string(),
            provider: "ollama".to_string(),
            name: "Qwen 2.5".to_string(),
            context_window: Some(128000),
            supports_vision: Some(false),
            supports_tools: Some(true),
            supports_streaming: Some(true),
        },
    ]
}

#[handler]
pub async fn models_list_handler(req: &mut Request, res: &mut Response) {
    let provider = req.query::<String>("provider");
    let models = get_builtin_models();

    let filtered: Vec<_> = if let Some(p) = provider {
        models.into_iter().filter(|m| m.provider == p).collect()
    } else {
        models
    };

    res.render(Text::Json(
        json!({
            "models": filtered,
            "count": filtered.len(),
        })
        .to_string(),
    ));
}

#[handler]
pub async fn models_providers_handler(res: &mut Response) {
    let providers = vec![
        json!({ "id": "openai", "name": "OpenAI", "models": ["gpt-4o", "gpt-4o-mini", "o1", "o1-mini"] }),
        json!({ "id": "anthropic", "name": "Anthropic", "models": ["claude-4-sonnet", "claude-3-5-sonnet", "claude-3-5-haiku"] }),
        json!({ "id": "google", "name": "Google", "models": ["gemini-2.0-flash", "gemini-2.0-pro"] }),
        json!({ "id": "deepseek", "name": "DeepSeek", "models": ["deepseek-chat", "deepseek-reasoner"] }),
        json!({ "id": "ollama", "name": "Ollama", "models": ["llama3.2", "qwen2.5"] }),
    ];

    res.render(Text::Json(
        json!({
            "providers": providers,
            "count": providers.len(),
        })
        .to_string(),
    ));
}

#[handler]
pub async fn models_get_handler(req: &mut Request, res: &mut Response) {
    let model_id = req.param::<String>("model_id").unwrap_or_default();
    if model_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing model_id" }).to_string(),
        ));
        return;
    }

    let models = get_builtin_models();
    match models.into_iter().find(|m| m.id == model_id) {
        Some(model) => res.render(Text::Json(json!({ "model": model }).to_string())),
        None => {
            res.status_code(StatusCode::NOT_FOUND);
            res.render(Text::Json(
                json!({ "error": "model not found" }).to_string(),
            ));
        }
    }
}
