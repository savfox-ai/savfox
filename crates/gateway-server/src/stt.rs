//! Server-side speech-to-text  - Whisper API integration for audio transcription.

use std::path::Path;

use reqwest::multipart;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::info;

/// STT provider
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SttProvider {
    /// OpenAI Whisper API
    OpenaiWhisper,
    /// Deepgram Nova
    Deepgram,
    /// Groq Whisper (OpenAI-compatible)
    GroqWhisper,
    /// Local whisper.cpp (future)
    LocalWhisper,
}

impl Default for SttProvider {
    fn default() -> Self {
        Self::OpenaiWhisper
    }
}

/// STT configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttConfig {
    #[serde(default)]
    pub provider: SttProvider,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

fn default_model() -> String {
    "whisper-1".into()
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            provider: SttProvider::default(),
            model: default_model(),
            language: None,
            api_key: None,
            base_url: None,
        }
    }
}

/// Transcription request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionRequest {
    /// Audio data as base64 or URL
    pub audio: AudioSource,
    /// Override model
    #[serde(default)]
    pub model: Option<String>,
    /// Override language
    #[serde(default)]
    pub language: Option<String>,
    /// Response format (json, text, srt, verbose_json, vtt)
    #[serde(default = "default_format")]
    pub response_format: String,
}

fn default_format() -> String {
    "json".into()
}

/// Audio source
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AudioSource {
    Base64 {
        audio_base64: String,
        mime_type: String,
    },
    Url {
        audio_url: String,
    },
}

/// Transcription result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub text: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub segments: Vec<TranscriptionSegment>,
}

/// A segment of transcribed text with timing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionSegment {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// Build OpenAI Whisper API URL
pub fn whisper_api_url(config: &SttConfig) -> String {
    let base = config
        .base_url
        .as_deref()
        .unwrap_or("https://api.openai.com/v1");
    format!("{base}/audio/transcriptions")
}

/// STT provider info
pub fn provider_info() -> Value {
    serde_json::json!({
        "providers": [
            {
                "id": "openai_whisper",
                "name": "OpenAI Whisper",
                "requires_api_key": true,
                "models": ["whisper-1"],
                "description": "OpenAI's Whisper model for speech-to-text transcription."
            },
            {
                "id": "deepgram",
                "name": "Deepgram Nova",
                "requires_api_key": true,
                "models": ["nova-3"],
                "description": "Deepgram's Nova model for fast, accurate transcription."
            },
            {
                "id": "groq_whisper",
                "name": "Groq Whisper",
                "requires_api_key": true,
                "models": ["whisper-large-v3", "whisper-large-v3-turbo"],
                "description": "Groq's Whisper endpoint for ultra-fast transcription."
            }
        ]
    })
}

/// Load STT config from disk
pub async fn load_stt_config(savfox_home: &Path) -> SttConfig {
    let path = savfox_home.join("gateway").join("stt-config.json");
    if let Ok(content) = tokio::fs::read_to_string(&path).await {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        SttConfig::default()
    }
}

/// Save STT config to disk
pub async fn save_stt_config(savfox_home: &Path, config: &SttConfig) -> Result<(), String> {
    let dir = savfox_home.join("gateway");
    let _ = tokio::fs::create_dir_all(&dir).await;
    let json = serde_json::to_string_pretty(config).map_err(|e| format!("serialize error: {e}"))?;
    tokio::fs::write(dir.join("stt-config.json"), json)
        .await
        .map_err(|e| format!("write error: {e}"))
}

/// Transcribe audio using the configured STT provider.
pub async fn transcribe(
    savfox_home: &Path,
    http_client: &reqwest::Client,
    params: &Value,
) -> Result<Value, String> {
    let config = load_stt_config(savfox_home).await;

    let api_key = params["api_key"]
        .as_str()
        .map(String::from)
        .or_else(|| config.api_key.clone())
        .or_else(|| std::env::var("OPENAI_API_KEY").ok());

    let audio_b64 = params["audio_base64"]
        .as_str()
        .ok_or("missing audio_base64 parameter")?;

    let mime = params["mime_type"].as_str().unwrap_or("audio/wav");
    let model = params["model"].as_str().unwrap_or(&config.model);
    let language = params["language"]
        .as_str()
        .map(String::from)
        .or_else(|| config.language.clone());

    let audio_bytes = base64_decode(audio_b64)?;

    // File extension from MIME
    let ext = match mime {
        "audio/wav" | "audio/wave" | "audio/x-wav" => "wav",
        "audio/mp3" | "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/webm" => "webm",
        "audio/m4a" | "audio/mp4" => "m4a",
        _ => "wav",
    };

    let provider = params["provider"]
        .as_str()
        .map(|s| match s {
            "deepgram" => SttProvider::Deepgram,
            "groq" | "groq_whisper" => SttProvider::GroqWhisper,
            _ => SttProvider::OpenaiWhisper,
        })
        .unwrap_or_else(|| config.provider.clone());

    match provider {
        SttProvider::OpenaiWhisper => {
            let key = api_key.ok_or("OpenAI API key required for Whisper STT")?;
            let url = whisper_api_url(&config);
            transcribe_openai_compat(
                http_client,
                &url,
                &key,
                model,
                language.as_deref(),
                &audio_bytes,
                ext,
            )
            .await
        }
        SttProvider::GroqWhisper => {
            let key = api_key
                .or_else(|| std::env::var("GROQ_API_KEY").ok())
                .ok_or("Groq API key required for Groq Whisper STT")?;
            let groq_model = if model == "whisper-1" {
                "whisper-large-v3-turbo"
            } else {
                model
            };
            let url = "https://api.groq.com/openai/v1/audio/transcriptions";
            transcribe_openai_compat(
                http_client,
                url,
                &key,
                groq_model,
                language.as_deref(),
                &audio_bytes,
                ext,
            )
            .await
        }
        SttProvider::Deepgram => {
            let key = api_key
                .or_else(|| std::env::var("DEEPGRAM_API_KEY").ok())
                .ok_or("Deepgram API key required for Deepgram STT")?;
            transcribe_deepgram(http_client, &key, language.as_deref(), &audio_bytes, mime).await
        }
        SttProvider::LocalWhisper => Err("Local whisper.cpp STT is not yet implemented".into()),
    }
}

/// Transcribe using OpenAI-compatible multipart endpoint (OpenAI, Groq).
async fn transcribe_openai_compat(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    model: &str,
    language: Option<&str>,
    audio: &[u8],
    ext: &str,
) -> Result<Value, String> {
    let file_part = multipart::Part::bytes(audio.to_vec())
        .file_name(format!("audio.{ext}"))
        .mime_str("application/octet-stream")
        .map_err(|e| format!("mime error: {e}"))?;

    let mut form = multipart::Form::new()
        .part("file", file_part)
        .text("model", model.to_string())
        .text("response_format", "verbose_json");

    if let Some(lang) = language {
        form = form.text("language", lang.to_string());
    }

    info!(url = %url, model = %model, "STT transcription request");

    let resp = client
        .post(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("STT request failed: {e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("STT response read error: {e}"))?;

    if !status.is_success() {
        return Err(format!("STT API error ({}): {}", status, body));
    }

    let result: Value =
        serde_json::from_str(&body).map_err(|e| format!("STT response parse error: {e}"))?;

    Ok(serde_json::json!({
        "text": result["text"].as_str().unwrap_or(""),
        "language": result["language"],
        "duration": result["duration"],
        "status": "ok",
    }))
}

/// Transcribe using Deepgram's REST API.
async fn transcribe_deepgram(
    client: &reqwest::Client,
    api_key: &str,
    language: Option<&str>,
    audio: &[u8],
    mime: &str,
) -> Result<Value, String> {
    let mut url = "https://api.deepgram.com/v1/listen?model=nova-3&smart_format=true".to_string();
    if let Some(lang) = language {
        url.push_str(&format!("&language={lang}"));
    }

    info!(model = "nova-3", "Deepgram STT transcription request");

    let resp = client
        .post(&url)
        .header("Authorization", format!("Token {api_key}"))
        .header("Content-Type", mime)
        .body(audio.to_vec())
        .send()
        .await
        .map_err(|e| format!("Deepgram request failed: {e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("Deepgram response read error: {e}"))?;

    if !status.is_success() {
        return Err(format!("Deepgram API error ({}): {}", status, body));
    }

    let result: Value =
        serde_json::from_str(&body).map_err(|e| format!("Deepgram response parse error: {e}"))?;

    // Extract transcript from Deepgram response format
    let text = result["results"]["channels"][0]["alternatives"][0]["transcript"]
        .as_str()
        .unwrap_or("");
    let duration = result["metadata"]["duration"].as_f64();
    let language = result["results"]["channels"][0]["detected_language"]
        .as_str()
        .map(|s| Value::String(s.to_string()))
        .unwrap_or(Value::Null);

    Ok(serde_json::json!({
        "text": text,
        "language": language,
        "duration": duration,
        "status": "ok",
    }))
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| format!("base64 decode error: {e}"))
}

/// Auto-transcription config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoTranscriptionConfig {
    /// Enable auto-transcription of audio messages
    #[serde(default)]
    pub enabled: bool,
    /// STT config to use
    #[serde(default)]
    pub stt: SttConfig,
    /// Maximum audio duration in seconds to auto-transcribe
    #[serde(default = "default_max_duration")]
    pub max_duration_secs: u64,
}

fn default_max_duration() -> u64 {
    300
} // 5 minutes

impl Default for AutoTranscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            stt: SttConfig::default(),
            max_duration_secs: default_max_duration(),
        }
    }
}
