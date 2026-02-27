use salvo::prelude::*;
use serde_json::json;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

static TTS_STATE: OnceLock<RwLock<TtsState>> = OnceLock::new();

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TtsState {
    enabled: bool,
    provider: String,
    voice: String,
    rate: f32,
    max_chars: usize,
    summarize: bool,
}

impl Default for TtsState {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "edge".to_string(),
            voice: "en-US-AriaNeural".to_string(),
            rate: 1.0,
            max_chars: 500,
            summarize: true,
        }
    }
}

fn tts_state() -> &'static RwLock<TtsState> {
    TTS_STATE.get_or_init(|| RwLock::new(TtsState::default()))
}

#[derive(Debug, Clone, serde::Serialize)]
struct TtsProvider {
    id: String,
    name: String,
    voices: Vec<String>,
}

fn get_providers() -> Vec<TtsProvider> {
    vec![
        TtsProvider {
            id: "edge".to_string(),
            name: "Microsoft Edge TTS".to_string(),
            voices: vec![
                "en-US-AriaNeural".to_string(),
                "en-US-GuyNeural".to_string(),
                "en-GB-SoniaNeural".to_string(),
                "en-AU-NatashaNeural".to_string(),
            ],
        },
        TtsProvider {
            id: "openai".to_string(),
            name: "OpenAI TTS".to_string(),
            voices: vec![
                "alloy".to_string(),
                "echo".to_string(),
                "fable".to_string(),
                "onyx".to_string(),
                "nova".to_string(),
                "shimmer".to_string(),
            ],
        },
        TtsProvider {
            id: "elevenlabs".to_string(),
            name: "ElevenLabs".to_string(),
            voices: vec![
                "rachel".to_string(),
                "domi".to_string(),
                "bella".to_string(),
                "adam".to_string(),
            ],
        },
    ]
}

#[handler]
pub async fn tts_status_handler(res: &mut Response) {
    let state = tts_state().read().await.clone();
    res.render(Text::Json(json!({ "tts": state }).to_string()));
}

#[handler]
pub async fn tts_enable_handler(res: &mut Response) {
    let mut state = tts_state().write().await;
    state.enabled = true;
    res.render(Text::Json(
        json!({ "status": "ok", "enabled": true, "message": "TTS enabled" }).to_string(),
    ));
}

#[handler]
pub async fn tts_disable_handler(res: &mut Response) {
    let mut state = tts_state().write().await;
    state.enabled = false;
    res.render(Text::Json(
        json!({ "status": "ok", "enabled": false, "message": "TTS disabled" }).to_string(),
    ));
}

#[handler]
pub async fn tts_providers_handler(res: &mut Response) {
    let providers = get_providers();
    res.render(Text::Json(json!({ "providers": providers }).to_string()));
}

#[handler]
pub async fn tts_set_provider_handler(req: &mut Request, res: &mut Response) {
    let body = match req.parse_json::<serde_json::Value>().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({ "error": format!("invalid JSON: {err}") }).to_string(),
            ));
            return;
        }
    };

    let provider = body.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    let voice = body.get("voice").and_then(|v| v.as_str());

    if provider.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "provider is required" }).to_string(),
        ));
        return;
    }

    let providers = get_providers();
    let valid_ids: Vec<&str> = providers.iter().map(|p| p.id.as_str()).collect();
    if !valid_ids.contains(&provider) {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": format!("invalid provider: {}", provider) }).to_string(),
        ));
        return;
    }

    let mut state = tts_state().write().await;
    state.provider = provider.to_string();
    if let Some(v) = voice {
        state.voice = v.to_string();
    }

    res.render(Text::Json(
        json!({
            "status": "ok",
            "provider": state.provider,
            "voice": state.voice,
            "message": "TTS provider updated",
        })
        .to_string(),
    ));
}

#[handler]
pub async fn tts_convert_handler(req: &mut Request, res: &mut Response) {
    let body = match req.parse_json::<serde_json::Value>().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({ "error": format!("invalid JSON: {err}") }).to_string(),
            ));
            return;
        }
    };

    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");

    if text.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "text is required" }).to_string(),
        ));
        return;
    }

    res.render(Text::Json(
        json!({
            "status": "ok",
            "message": "TTS conversion placeholder - would return audio URL",
            "text_length": text.len(),
        })
        .to_string(),
    ));
}
