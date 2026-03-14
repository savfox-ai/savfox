use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{tts_deepgram, tts_edge};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TtsConfig {
    pub(crate) enabled: bool,
    pub(crate) provider: Option<String>,
    pub(crate) default_voice: Option<String>,
    pub(crate) default_model: Option<String>,
    #[serde(default = "default_speed")]
    pub(crate) speed: f64,
    #[serde(default = "default_pitch")]
    pub(crate) pitch: f64,
}

fn default_speed() -> f64 {
    1.0
}
fn default_pitch() -> f64 {
    1.0
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: None,
            default_voice: None,
            default_model: None,
            speed: 1.0,
            pitch: 1.0,
        }
    }
}

fn config_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("tts-config.json")
}

fn output_dir(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("tts-audio")
}

pub(crate) async fn load_tts_config(savfox_home: &Path) -> Result<TtsConfig, String> {
    let path = config_path(savfox_home);
    let data = tokio::fs::read_to_string(path).await.unwrap_or_default();
    if data.trim().is_empty() {
        return Ok(TtsConfig::default());
    }
    serde_json::from_str::<TtsConfig>(&data)
        .map_err(|err| format!("failed to parse tts config: {err}"))
}

pub(crate) async fn save_tts_config(savfox_home: &Path, cfg: &TtsConfig) -> Result<(), String> {
    let path = config_path(savfox_home);
    crate::json_store::save_json(&path, cfg, "TTS config").await
}

pub(crate) async fn status(savfox_home: &Path) -> Result<Value, String> {
    let cfg = load_tts_config(savfox_home).await?;
    let providers = providers();
    let active = cfg.provider.clone().unwrap_or_else(|| "openai".to_string());
    let has_key = provider_key_from_env(&active).is_some();
    Ok(json!({
        "enabled": cfg.enabled,
        "provider": cfg.provider,
        "voice": cfg.default_voice,
        "default_model": cfg.default_model,
        "speed": cfg.speed,
        "pitch": cfg.pitch,
        "available_providers": providers,
        "active_provider_has_key": has_key,
    }))
}

/// Return the list of voices available for a given provider.
pub(crate) fn voices_for_provider(provider: &str) -> Value {
    match provider {
        "openai" => json!({
            "voices": ["alloy", "echo", "fable", "onyx", "nova", "shimmer"]
        }),
        "elevenlabs" => json!({
            "voices": ["EXAVITQu4vr4xnSDxMaL"]
        }),
        "google" => json!({
            "voices": [
                "en-US-Neural2-A", "en-US-Neural2-C", "en-US-Neural2-D", "en-US-Neural2-F",
                "en-US-Wavenet-A", "en-US-Wavenet-C", "en-US-Wavenet-D", "en-US-Wavenet-F",
                "en-US-Standard-A", "en-US-Standard-C", "en-US-Standard-D", "en-US-Standard-F"
            ]
        }),
        "edge" => {
            let edge_voices: Vec<String> = tts_edge::default_voices()
                .iter()
                .map(|v| v.short_name.clone())
                .collect();
            json!({ "voices": edge_voices })
        }
        "deepgram" => {
            let dg_voices: Vec<String> = tts_deepgram::available_voices()
                .iter()
                .map(|v| v.id.clone())
                .collect();
            json!({ "voices": dg_voices })
        }
        _ => json!({ "voices": [] }),
    }
}

/// Set only the voice (without changing provider).
pub(crate) async fn set_voice(savfox_home: &Path, voice: &str) -> Result<Value, String> {
    let mut cfg = load_tts_config(savfox_home).await?;
    cfg.default_voice = Some(voice.to_owned());
    save_tts_config(savfox_home, &cfg).await?;
    Ok(json!({ "voice": voice, "status": "set" }))
}

/// Update speed/pitch settings.
pub(crate) async fn update_settings(
    savfox_home: &Path,
    speed: Option<f64>,
    pitch: Option<f64>,
) -> Result<Value, String> {
    let mut cfg = load_tts_config(savfox_home).await?;
    if let Some(s) = speed {
        cfg.speed = s.clamp(0.5, 2.0);
    }
    if let Some(p) = pitch {
        cfg.pitch = p.clamp(0.5, 2.0);
    }
    save_tts_config(savfox_home, &cfg).await?;
    Ok(json!({ "speed": cfg.speed, "pitch": cfg.pitch, "status": "updated" }))
}

pub(crate) fn providers() -> Value {
    json!([
        {
            "id": "openai",
            "name": "OpenAI TTS",
            "requires_key_env": "OPENAI_API_KEY"
        },
        {
            "id": "elevenlabs",
            "name": "ElevenLabs",
            "requires_key_env": "ELEVENLABS_API_KEY"
        },
        {
            "id": "google",
            "name": "Google Cloud TTS",
            "requires_key_env": "GOOGLE_CLOUD_TTS_API_KEY",
            "voice_families": ["WaveNet", "Neural2", "Standard"],
            "supports_ssml": true,
            "supports_locale": true,
        },
        tts_edge::provider_info(),
        tts_deepgram::provider_info(),
    ])
}

/// Return providers enriched with per-provider status (voice count,
/// API-key configured, active flag).
pub(crate) async fn providers_with_status(savfox_home: &Path) -> Result<Value, String> {
    let cfg = load_tts_config(savfox_home).await?;
    let active_id = cfg.provider.clone().unwrap_or_else(|| "openai".to_string());
    let raw = providers();
    let arr = raw.as_array().cloned().unwrap_or_default();

    let enriched: Vec<Value> = arr
        .into_iter()
        .map(|mut p| {
            let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let is_active = id == active_id && cfg.enabled;
            let is_configured = provider_key_from_env(&id).is_some();
            let voices = voices_for_provider(&id);
            let voice_count = voices
                .get("voices")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if let Some(obj) = p.as_object_mut() {
                obj.insert("is_active".to_string(), json!(is_active));
                obj.insert("is_configured".to_string(), json!(is_configured));
                obj.insert("voice_count".to_string(), json!(voice_count));
            }
            p
        })
        .collect();

    Ok(json!({ "providers": enriched }))
}

pub(crate) async fn enable(
    savfox_home: &Path,
    provider: Option<&str>,
    voice: Option<&str>,
    model: Option<&str>,
) -> Result<Value, String> {
    let mut cfg = load_tts_config(savfox_home).await?;
    cfg.enabled = true;
    if let Some(p) = provider {
        cfg.provider = Some(p.to_owned());
    }
    if let Some(v) = voice {
        cfg.default_voice = Some(v.to_owned());
    }
    if let Some(m) = model {
        cfg.default_model = Some(m.to_owned());
    }
    save_tts_config(savfox_home, &cfg).await?;
    Ok(json!({
        "status": "enabled",
        "provider": cfg.provider,
        "default_voice": cfg.default_voice,
        "default_model": cfg.default_model
    }))
}

pub(crate) async fn disable(savfox_home: &Path) -> Result<Value, String> {
    let mut cfg = load_tts_config(savfox_home).await?;
    cfg.enabled = false;
    save_tts_config(savfox_home, &cfg).await?;
    Ok(json!({ "status": "disabled" }))
}

pub(crate) async fn set_provider(
    savfox_home: &Path,
    provider: &str,
    voice: Option<&str>,
    model: Option<&str>,
) -> Result<Value, String> {
    const SUPPORTED: &[&str] = &["openai", "elevenlabs", "google", "edge", "deepgram"];
    if !SUPPORTED.contains(&provider) {
        return Err(format!("unsupported provider: {provider}"));
    }
    let mut cfg = load_tts_config(savfox_home).await?;
    cfg.provider = Some(provider.to_owned());
    if let Some(v) = voice {
        cfg.default_voice = Some(v.to_owned());
    }
    if let Some(m) = model {
        cfg.default_model = Some(m.to_owned());
    }
    save_tts_config(savfox_home, &cfg).await?;
    Ok(json!({
        "provider": cfg.provider,
        "default_voice": cfg.default_voice,
        "default_model": cfg.default_model,
        "status": "set"
    }))
}

pub(crate) async fn convert(
    savfox_home: &Path,
    http_client: &reqwest::Client,
    params: &Value,
) -> Result<Value, String> {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return Err("missing 'text' parameter".to_string());
    }

    let cfg = load_tts_config(savfox_home).await?;
    let provider = params
        .get("provider")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .or(cfg.provider.clone())
        .unwrap_or_else(|| "openai".to_string());
    let voice = params
        .get("voice")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .or(cfg.default_voice.clone());
    let model = params
        .get("model")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .or(cfg.default_model.clone());
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("mp3");

    let audio_bytes = match provider.as_str() {
        "openai" => {
            let key = params
                .get("api_key")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .ok_or_else(|| "OPENAI_API_KEY is not configured".to_string())?;
            let body = json!({
                "model": model.unwrap_or_else(|| "gpt-4o-mini-tts".to_string()),
                "voice": voice.unwrap_or_else(|| "alloy".to_string()),
                "input": text,
                "format": format,
            });
            let resp = http_client
                .post("https://api.openai.com/v1/audio/speech")
                .header("Authorization", format!("Bearer {key}"))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|err| format!("openai tts request failed: {err}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("openai tts error: HTTP {status}: {body}"));
            }
            resp.bytes()
                .await
                .map_err(|err| format!("openai tts read body failed: {err}"))?
                .to_vec()
        }
        "elevenlabs" => {
            let key = params
                .get("api_key")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
                .or_else(|| std::env::var("ELEVENLABS_API_KEY").ok())
                .ok_or_else(|| "ELEVENLABS_API_KEY is not configured".to_string())?;
            let voice_id = voice
                .or_else(|| std::env::var("ELEVENLABS_VOICE_ID").ok())
                .unwrap_or_else(|| "EXAVITQu4vr4xnSDxMaL".to_string());
            let url = format!("https://api.elevenlabs.io/v1/text-to-speech/{voice_id}");
            let body = json!({
                "text": text,
                "model_id": model.unwrap_or_else(|| "eleven_multilingual_v2".to_string()),
                "output_format": "mp3_44100_128"
            });
            let resp = http_client
                .post(url)
                .header("xi-api-key", key)
                .header("Accept", "audio/mpeg")
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|err| format!("elevenlabs tts request failed: {err}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("elevenlabs tts error: HTTP {status}: {body}"));
            }
            resp.bytes()
                .await
                .map_err(|err| format!("elevenlabs tts read body failed: {err}"))?
                .to_vec()
        }
        "edge" => {
            let edge_config = tts_edge::EdgeTtsConfig {
                voice: voice.unwrap_or_else(|| "en-US-AriaNeural".to_string()),
                rate: params
                    .get("rate")
                    .and_then(|v| v.as_str())
                    .unwrap_or("+0%")
                    .to_string(),
                volume: params
                    .get("volume")
                    .and_then(|v| v.as_str())
                    .unwrap_or("+0%")
                    .to_string(),
                pitch: params
                    .get("pitch")
                    .and_then(|v| v.as_str())
                    .unwrap_or("+0Hz")
                    .to_string(),
            };
            tts_edge::synthesize(text, &edge_config).await?
        }
        "deepgram" => {
            let key = params
                .get("api_key")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
                .or_else(|| std::env::var("DEEPGRAM_API_KEY").ok())
                .ok_or_else(|| "DEEPGRAM_API_KEY is not configured".to_string())?;
            let dg_config = tts_deepgram::DeepgramTtsConfig {
                api_key: key,
                model: voice
                    .or(model)
                    .unwrap_or_else(|| "aura-asteria-en".to_string()),
                encoding: params
                    .get("encoding")
                    .and_then(|v| v.as_str())
                    .unwrap_or("mp3")
                    .to_string(),
                sample_rate: params
                    .get("sample_rate")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(24000) as u32,
            };
            tts_deepgram::synthesize(http_client, text, &dg_config).await?
        }
        "google" => {
            let key = params
                .get("api_key")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
                .or_else(|| std::env::var("GOOGLE_CLOUD_TTS_API_KEY").ok())
                .or_else(|| std::env::var("GOOGLE_API_KEY").ok())
                .ok_or_else(|| "GOOGLE_CLOUD_TTS_API_KEY is not configured".to_string())?;
            let locale = params
                .get("language")
                .or_else(|| params.get("locale"))
                .and_then(|v| v.as_str())
                .unwrap_or("en-US");
            let voice_family = model.as_deref().unwrap_or("Neural2");
            let voice_name = voice
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| default_google_voice(locale, voice_family));
            let ssml = params.get("ssml").and_then(|v| v.as_str());
            let input = if let Some(ssml) = ssml {
                json!({ "ssml": ssml })
            } else {
                json!({ "text": text })
            };
            let mut audio_config = json!({
                "audioEncoding": google_audio_encoding(format),
            });
            if let Some(speaking_rate) = params.get("speaking_rate").and_then(|v| v.as_f64()) {
                audio_config["speakingRate"] = json!(speaking_rate);
            }
            if let Some(pitch) = params.get("pitch").and_then(|v| v.as_f64()) {
                audio_config["pitch"] = json!(pitch);
            }
            if let Some(volume_gain_db) = params.get("volume_gain_db").and_then(|v| v.as_f64()) {
                audio_config["volumeGainDb"] = json!(volume_gain_db);
            }
            if let Some(sample_rate_hz) = params.get("sample_rate").and_then(|v| v.as_u64()) {
                audio_config["sampleRateHertz"] = json!(sample_rate_hz);
            }
            let body = json!({
                "input": input,
                "voice": {
                    "languageCode": locale,
                    "name": voice_name,
                },
                "audioConfig": audio_config,
            });
            let url = format!("https://texttospeech.googleapis.com/v1/text:synthesize?key={key}");
            let resp = http_client
                .post(url)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|err| format!("google tts request failed: {err}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("google tts error: HTTP {status}: {body}"));
            }
            let payload = resp
                .json::<Value>()
                .await
                .map_err(|err| format!("google tts parse response failed: {err}"))?;
            let audio_content = payload
                .get("audioContent")
                .and_then(Value::as_str)
                .ok_or_else(|| "google tts response missing audioContent".to_string())?;
            base64::engine::general_purpose::STANDARD
                .decode(audio_content)
                .map_err(|err| format!("google tts decode audio failed: {err}"))?
        }
        _ => return Err(format!("unsupported provider: {provider}")),
    };

    let dir = output_dir(savfox_home);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| format!("failed to create tts output dir: {err}"))?;
    let ext = match format.to_ascii_lowercase().as_str() {
        "wav" => "wav",
        "linear16" | "pcm" => "pcm",
        "ogg" | "opus" => "ogg",
        "flac" => "flac",
        "aac" => "aac",
        _ => "mp3",
    };
    let file_name = format!(
        "tts-{}-{}.{}",
        crate::json_store::now_ms(),
        uuid::Uuid::now_v7(),
        ext
    );
    let path = dir.join(&file_name);
    tokio::fs::write(&path, audio_bytes.as_slice())
        .await
        .map_err(|err| format!("failed to write tts output: {err}"))?;

    Ok(json!({
        "status": "converted",
        "provider": provider,
        "file_name": file_name,
        "audio_path": path.to_string_lossy(),
        "audio_bytes": audio_bytes.len(),
        "text_length": text.len(),
    }))
}

fn provider_key_from_env(provider: &str) -> Option<String> {
    match provider {
        "openai" => std::env::var("OPENAI_API_KEY").ok(),
        "elevenlabs" => std::env::var("ELEVENLABS_API_KEY").ok(),
        "google" => std::env::var("GOOGLE_CLOUD_TTS_API_KEY")
            .ok()
            .or_else(|| std::env::var("GOOGLE_API_KEY").ok()),
        "deepgram" => std::env::var("DEEPGRAM_API_KEY").ok(),
        // Edge TTS does not require an API key  - always "available".
        "edge" => Some("not_required".to_string()),
        _ => None,
    }
}

fn default_google_voice(locale: &str, family_hint: &str) -> String {
    let normalized_family = family_hint.to_ascii_lowercase();
    let family = if normalized_family.contains("wavenet") {
        "Wavenet"
    } else if normalized_family.contains("standard") {
        "Standard"
    } else {
        "Neural2"
    };
    format!("{locale}-{family}-D")
}

fn google_audio_encoding(requested_format: &str) -> &'static str {
    match requested_format.to_ascii_lowercase().as_str() {
        "wav" | "linear16" | "pcm" => "LINEAR16",
        "ogg" | "opus" => "OGG_OPUS",
        // Default to MP3 for browser-friendly playback.
        _ => "MP3",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tts_config_roundtrip() {
        let home = std::env::temp_dir().join(format!("savfox-tts-test-{}", uuid::Uuid::now_v7()));
        let cfg = TtsConfig {
            enabled: true,
            provider: Some("openai".to_string()),
            default_voice: Some("alloy".to_string()),
            default_model: Some("gpt-4o-mini-tts".to_string()),
            speed: 1.0,
            pitch: 1.0,
        };
        save_tts_config(&home, &cfg).await.expect("save");
        let loaded = load_tts_config(&home).await.expect("load");
        assert!(loaded.enabled);
        assert_eq!(loaded.provider.as_deref(), Some("openai"));
        let _ = tokio::fs::remove_dir_all(home).await;
    }

    #[test]
    fn google_voice_defaults_track_family_hint() {
        assert_eq!(default_google_voice("en-US", "WaveNet"), "en-US-Wavenet-D");
        assert_eq!(
            default_google_voice("en-US", "Standard"),
            "en-US-Standard-D"
        );
        assert_eq!(default_google_voice("en-US", "Neural2"), "en-US-Neural2-D");
    }

    #[test]
    fn google_encoding_maps_expected_formats() {
        assert_eq!(google_audio_encoding("wav"), "LINEAR16");
        assert_eq!(google_audio_encoding("pcm"), "LINEAR16");
        assert_eq!(google_audio_encoding("ogg"), "OGG_OPUS");
        assert_eq!(google_audio_encoding("mp3"), "MP3");
    }
}
