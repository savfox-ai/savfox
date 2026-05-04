//! Deepgram TTS provider  - commercial text-to-speech via REST API.
//!
//! Uses `POST https://api.deepgram.com/v1/speak` with query parameters for
//! model, encoding, and sample rate. Requires a `DEEPGRAM_API_KEY` environment
//! variable (or explicit key passed via config).

use serde::{Deserialize, Serialize};
use serde_json::json;

/// Deepgram TTS API endpoint
pub const DEEPGRAM_TTS_URL: &str = "https://api.deepgram.com/v1/speak";

/// Deepgram TTS voice models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepgramVoice {
    pub id: String,
    pub name: String,
    pub language: String,
}

pub fn available_voices() -> Vec<DeepgramVoice> {
    vec![
        DeepgramVoice {
            id: "aura-asteria-en".into(),
            name: "Asteria (Female)".into(),
            language: "en".into(),
        },
        DeepgramVoice {
            id: "aura-luna-en".into(),
            name: "Luna (Female)".into(),
            language: "en".into(),
        },
        DeepgramVoice {
            id: "aura-stella-en".into(),
            name: "Stella (Female)".into(),
            language: "en".into(),
        },
        DeepgramVoice {
            id: "aura-athena-en".into(),
            name: "Athena (Female)".into(),
            language: "en".into(),
        },
        DeepgramVoice {
            id: "aura-hera-en".into(),
            name: "Hera (Female)".into(),
            language: "en".into(),
        },
        DeepgramVoice {
            id: "aura-orion-en".into(),
            name: "Orion (Male)".into(),
            language: "en".into(),
        },
        DeepgramVoice {
            id: "aura-arcas-en".into(),
            name: "Arcas (Male)".into(),
            language: "en".into(),
        },
        DeepgramVoice {
            id: "aura-perseus-en".into(),
            name: "Perseus (Male)".into(),
            language: "en".into(),
        },
        DeepgramVoice {
            id: "aura-angus-en".into(),
            name: "Angus (Male)".into(),
            language: "en".into(),
        },
        DeepgramVoice {
            id: "aura-orpheus-en".into(),
            name: "Orpheus (Male)".into(),
            language: "en".into(),
        },
        DeepgramVoice {
            id: "aura-helios-en".into(),
            name: "Helios (Male)".into(),
            language: "en".into(),
        },
        DeepgramVoice {
            id: "aura-zeus-en".into(),
            name: "Zeus (Male)".into(),
            language: "en".into(),
        },
    ]
}

/// Deepgram TTS configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepgramTtsConfig {
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_encoding")]
    pub encoding: String,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
}

fn default_model() -> String {
    "aura-asteria-en".into()
}
fn default_encoding() -> String {
    "mp3".into()
}
fn default_sample_rate() -> u32 {
    24000
}

impl Default for DeepgramTtsConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: default_model(),
            encoding: default_encoding(),
            sample_rate: default_sample_rate(),
        }
    }
}

/// Build the Deepgram TTS request URL with query parameters.
pub fn build_request_url(config: &DeepgramTtsConfig) -> String {
    format!(
        "{}?model={}&encoding={}&sample_rate={}",
        DEEPGRAM_TTS_URL, config.model, config.encoding, config.sample_rate
    )
}

/// Synthesize text to audio bytes using the Deepgram REST API.
///
/// Sends a `POST` to `https://api.deepgram.com/v1/speak` with query parameters
/// for the voice model, audio encoding, and sample rate. The request body is
/// JSON `{ "text": "<input>" }`. Returns the raw audio bytes on success.
///
/// # Arguments
/// * `http_client` - A shared `reqwest::Client` (connection pooling).
/// * `text` - The text to synthesize.
/// * `config` - Deepgram-specific configuration (API key, model, encoding, sample rate).
///
/// # Errors
/// Returns `Err(String)` if the API key is empty, the HTTP request fails, or
/// the server returns a non-2xx status.
pub async fn synthesize(
    http_client: &reqwest::Client,
    text: &str,
    config: &DeepgramTtsConfig,
) -> Result<Vec<u8>, String> {
    if config.api_key.is_empty() {
        return Err("deepgram api_key is required".to_owned());
    }
    if text.is_empty() {
        return Err("text must not be empty".to_owned());
    }

    let url = build_request_url(config);
    let body = json!({ "text": text });

    let resp = http_client
        .post(&url)
        .header("Authorization", format!("Token {}", config.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|err| format!("deepgram tts request failed: {err}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        return Err(format!("deepgram tts error: HTTP {status}: {err_body}"));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|err| format!("deepgram tts read body failed: {err}"))?;

    Ok(bytes.to_vec())
}

/// Provider info for listing
pub fn provider_info() -> serde_json::Value {
    serde_json::json!({
        "id": "deepgram",
        "name": "Deepgram TTS",
        "requires_api_key": true,
        "requires_key_env": "DEEPGRAM_API_KEY",
        "voices": available_voices(),
        "description": "High-quality text-to-speech using Deepgram's Aura voices."
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_default_config() {
        let mut cfg = DeepgramTtsConfig::default();
        cfg.api_key = "test-key".into();
        let url = build_request_url(&cfg);
        assert!(url.starts_with(DEEPGRAM_TTS_URL));
        assert!(url.contains("model=aura-asteria-en"));
        assert!(url.contains("encoding=mp3"));
        assert!(url.contains("sample_rate=24000"));
    }

    #[test]
    fn build_url_custom_config() {
        let cfg = DeepgramTtsConfig {
            api_key: "key".into(),
            model: "aura-zeus-en".into(),
            encoding: "linear16".into(),
            sample_rate: 48000,
        };
        let url = build_request_url(&cfg);
        assert!(url.contains("model=aura-zeus-en"));
        assert!(url.contains("encoding=linear16"));
        assert!(url.contains("sample_rate=48000"));
    }

    #[test]
    fn provider_info_fields() {
        let info = provider_info();
        assert_eq!(info["id"], "deepgram");
        assert_eq!(info["requires_api_key"], true);
        assert_eq!(info["requires_key_env"], "DEEPGRAM_API_KEY");
    }

    #[tokio::test]
    async fn synthesize_rejects_empty_key() {
        let client = reqwest::Client::new();
        let cfg = DeepgramTtsConfig::default();
        let result = synthesize(&client, "hello", &cfg).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("api_key is required"));
    }

    #[tokio::test]
    async fn synthesize_rejects_empty_text() {
        let client = reqwest::Client::new();
        let mut cfg = DeepgramTtsConfig::default();
        cfg.api_key = "test-key".into();
        let result = synthesize(&client, "", &cfg).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("text must not be empty"));
    }
}
