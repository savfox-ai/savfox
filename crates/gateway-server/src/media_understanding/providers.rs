use std::collections::HashMap;

use reqwest::Client;
use serde_json::json;

use super::types::{
    AudioTranscriptionResult, ImageDescriptionResult, MediaCapability, VideoDescriptionResult,
};

pub type MediaResult<T> = Result<T, String>;

#[async_trait::async_trait]
pub trait MediaProvider: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> Vec<MediaCapability>;

    async fn describe_image(
        &self,
        _data: &[u8],
        _mime: &str,
        _model: Option<&str>,
        _prompt: Option<&str>,
    ) -> MediaResult<ImageDescriptionResult> {
        Err("Image description not supported".to_string())
    }

    async fn transcribe_audio(
        &self,
        _data: &[u8],
        _mime: &str,
        _model: Option<&str>,
        _language: Option<&str>,
    ) -> MediaResult<AudioTranscriptionResult> {
        Err("Audio transcription not supported".to_string())
    }

    async fn describe_video(
        &self,
        _data: &[u8],
        _mime: &str,
        _model: Option<&str>,
        _prompt: Option<&str>,
    ) -> MediaResult<VideoDescriptionResult> {
        Err("Video description not supported".to_string())
    }
}

pub struct OpenAIMediaProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl OpenAIMediaProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1".to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }
}

#[async_trait::async_trait]
impl MediaProvider for OpenAIMediaProvider {
    fn id(&self) -> &str {
        "openai"
    }

    fn capabilities(&self) -> Vec<MediaCapability> {
        vec![MediaCapability::Image, MediaCapability::Audio]
    }

    async fn describe_image(
        &self,
        data: &[u8],
        _mime: &str,
        model: Option<&str>,
        prompt: Option<&str>,
    ) -> MediaResult<ImageDescriptionResult> {
        let model = model.unwrap_or("gpt-4o-mini");
        let prompt = prompt.unwrap_or("Describe the image.");

        let base64_data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data);
        let image_url = format!("data:image/jpeg;base64,{}", base64_data);

        let body = json!({
            "model": model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": prompt},
                    {"type": "image_url", "image_url": {"url": image_url}}
                ]
            }],
            "max_tokens": 500
        });

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("API error {}: {}", status, text));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let text = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        Ok(ImageDescriptionResult {
            text,
            model: Some(model.to_string()),
        })
    }

    async fn transcribe_audio(
        &self,
        data: &[u8],
        _mime: &str,
        model: Option<&str>,
        language: Option<&str>,
    ) -> MediaResult<AudioTranscriptionResult> {
        let model = model.unwrap_or("whisper-1");

        let mut form = reqwest::multipart::Form::new()
            .text("model", model.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(data.to_vec()).file_name("audio.mp3"),
            );

        if let Some(lang) = language {
            form = form.text("language", lang.to_string());
        }

        let response = self
            .client
            .post(format!("{}/audio/transcriptions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("API error {}: {}", status, text));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let text = json["text"].as_str().unwrap_or("").to_string();

        Ok(AudioTranscriptionResult {
            text,
            model: Some(model.to_string()),
        })
    }
}

pub struct AnthropicMediaProvider {
    client: Client,
    api_key: String,
}

impl AnthropicMediaProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }
}

#[async_trait::async_trait]
impl MediaProvider for AnthropicMediaProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn capabilities(&self) -> Vec<MediaCapability> {
        vec![MediaCapability::Image]
    }

    async fn describe_image(
        &self,
        data: &[u8],
        mime: &str,
        model: Option<&str>,
        prompt: Option<&str>,
    ) -> MediaResult<ImageDescriptionResult> {
        let model = model.unwrap_or("claude-3-5-sonnet-latest");
        let prompt = prompt.unwrap_or("Describe the image.");

        let base64_data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data);
        let media_type = if mime.contains("png") {
            "image/png"
        } else {
            "image/jpeg"
        };

        let body = json!({
            "model": model,
            "max_tokens": 500,
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": base64_data
                        }
                    },
                    {"type": "text", "text": prompt}
                ]
            }]
        });

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("API error {}: {}", status, text));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let text = json["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();

        Ok(ImageDescriptionResult {
            text,
            model: Some(model.to_string()),
        })
    }
}

pub struct GoogleMediaProvider {
    client: Client,
    api_key: String,
}

impl GoogleMediaProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }
}

#[async_trait::async_trait]
impl MediaProvider for GoogleMediaProvider {
    fn id(&self) -> &str {
        "google"
    }

    fn capabilities(&self) -> Vec<MediaCapability> {
        vec![MediaCapability::Image, MediaCapability::Video]
    }

    async fn describe_image(
        &self,
        data: &[u8],
        _mime: &str,
        model: Option<&str>,
        prompt: Option<&str>,
    ) -> MediaResult<ImageDescriptionResult> {
        let model = model.unwrap_or("gemini-2.0-flash");
        let prompt = prompt.unwrap_or("Describe the image.");

        let base64_data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data);

        let body = json!({
            "contents": [{
                "parts": [
                    {"text": prompt},
                    {"inline_data": {"mime_type": "image/jpeg", "data": base64_data}}
                ]
            }]
        });

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            model, self.api_key
        );

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("API error {}: {}", status, text));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let text = json["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();

        Ok(ImageDescriptionResult {
            text,
            model: Some(model.to_string()),
        })
    }

    async fn describe_video(
        &self,
        data: &[u8],
        _mime: &str,
        model: Option<&str>,
        prompt: Option<&str>,
    ) -> MediaResult<VideoDescriptionResult> {
        let model = model.unwrap_or("gemini-2.0-flash");
        let prompt = prompt.unwrap_or("Describe the video.");

        let base64_data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data);

        let body = json!({
            "contents": [{
                "parts": [
                    {"text": prompt},
                    {"inline_data": {"mime_type": "video/mp4", "data": base64_data}}
                ]
            }]
        });

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            model, self.api_key
        );

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("API error {}: {}", status, text));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let text = json["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();

        Ok(VideoDescriptionResult {
            text,
            model: Some(model.to_string()),
        })
    }
}

pub struct DeepgramMediaProvider {
    client: Client,
    api_key: String,
}

impl DeepgramMediaProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }
}

#[async_trait::async_trait]
impl MediaProvider for DeepgramMediaProvider {
    fn id(&self) -> &str {
        "deepgram"
    }

    fn capabilities(&self) -> Vec<MediaCapability> {
        vec![MediaCapability::Audio]
    }

    async fn transcribe_audio(
        &self,
        data: &[u8],
        mime: &str,
        model: Option<&str>,
        language: Option<&str>,
    ) -> MediaResult<AudioTranscriptionResult> {
        let model = model.unwrap_or("nova-3");
        let mut url = format!(
            "https://api.deepgram.com/v1/listen?model={}&smart_format=true",
            model
        );
        if let Some(lang) = language {
            url.push_str(&format!("&language={lang}"));
        }

        let response = self
            .client
            .post(url)
            .header("Authorization", format!("Token {}", self.api_key))
            .header("Content-Type", mime)
            .body(data.to_vec())
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("API error {}: {}", status, text));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let text = json["results"]["channels"][0]["alternatives"][0]["transcript"]
            .as_str()
            .unwrap_or("")
            .to_string();

        Ok(AudioTranscriptionResult {
            text,
            model: Some(model.to_string()),
        })
    }
}

pub struct GroqMediaProvider {
    client: Client,
    api_key: String,
}

impl GroqMediaProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }
}

#[async_trait::async_trait]
impl MediaProvider for GroqMediaProvider {
    fn id(&self) -> &str {
        "groq"
    }

    fn capabilities(&self) -> Vec<MediaCapability> {
        vec![MediaCapability::Audio]
    }

    async fn transcribe_audio(
        &self,
        data: &[u8],
        _mime: &str,
        model: Option<&str>,
        language: Option<&str>,
    ) -> MediaResult<AudioTranscriptionResult> {
        let model = model.unwrap_or("whisper-large-v3-turbo");

        let mut form = reqwest::multipart::Form::new()
            .text("model", model.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(data.to_vec()).file_name("audio.wav"),
            );

        if let Some(lang) = language {
            form = form.text("language", lang.to_string());
        }

        let response = self
            .client
            .post("https://api.groq.com/openai/v1/audio/transcriptions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("API error {}: {}", status, text));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let text = json["text"].as_str().unwrap_or("").to_string();

        Ok(AudioTranscriptionResult {
            text,
            model: Some(model.to_string()),
        })
    }
}

pub struct MediaProviders {
    providers: HashMap<String, Box<dyn MediaProvider>>,
}

impl MediaProviders {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    pub fn register(&mut self, provider: Box<dyn MediaProvider>) {
        self.providers.insert(provider.id().to_string(), provider);
    }

    pub fn get(&self, id: &str) -> Option<&dyn MediaProvider> {
        self.providers.get(id).map(|p| p.as_ref())
    }

    pub fn get_for_capability(&self, capability: MediaCapability) -> Option<&dyn MediaProvider> {
        self.providers
            .values()
            .find(|p| p.capabilities().contains(&capability))
            .map(|p| p.as_ref())
    }

    pub fn list(&self) -> Vec<&dyn MediaProvider> {
        self.providers.values().map(|p| p.as_ref()).collect()
    }
}

impl Default for MediaProviders {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_capabilities_surface_expected_features() {
        let openai = OpenAIMediaProvider::new("test".to_string());
        assert_eq!(openai.id(), "openai");
        assert!(openai.capabilities().contains(&MediaCapability::Image));
        assert!(openai.capabilities().contains(&MediaCapability::Audio));

        let anthropic = AnthropicMediaProvider::new("test".to_string());
        assert_eq!(anthropic.id(), "anthropic");
        assert_eq!(anthropic.capabilities(), vec![MediaCapability::Image]);

        let google = GoogleMediaProvider::new("test".to_string());
        assert_eq!(google.id(), "google");
        assert!(google.capabilities().contains(&MediaCapability::Image));
        assert!(google.capabilities().contains(&MediaCapability::Video));

        let deepgram = DeepgramMediaProvider::new("test".to_string());
        assert_eq!(deepgram.id(), "deepgram");
        assert_eq!(deepgram.capabilities(), vec![MediaCapability::Audio]);

        let groq = GroqMediaProvider::new("test".to_string());
        assert_eq!(groq.id(), "groq");
        assert_eq!(groq.capabilities(), vec![MediaCapability::Audio]);
    }
}
