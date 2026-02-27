use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaCapability {
    Image,
    Audio,
    Video,
    Document,
}

impl MediaCapability {
    pub fn default_max_bytes(&self) -> usize {
        const MB: usize = 1024 * 1024;
        match self {
            MediaCapability::Image => 10 * MB,
            MediaCapability::Audio => 20 * MB,
            MediaCapability::Video => 50 * MB,
            MediaCapability::Document => 25 * MB,
        }
    }

    pub fn default_timeout_seconds(&self) -> u64 {
        match self {
            MediaCapability::Image => 60,
            MediaCapability::Audio => 60,
            MediaCapability::Video => 120,
            MediaCapability::Document => 120,
        }
    }

    pub fn default_prompt(&self) -> &'static str {
        match self {
            MediaCapability::Image => "Describe the image.",
            MediaCapability::Audio => "Transcribe the audio.",
            MediaCapability::Video => "Describe the video.",
            MediaCapability::Document => "Extract and summarize text from this PDF document.",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaAttachment {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<u8>>,
}

impl MediaAttachment {
    pub fn from_bytes(data: Vec<u8>, mime: Option<String>, index: usize) -> Self {
        Self {
            path: None,
            url: None,
            mime,
            index,
            data: Some(data),
        }
    }

    pub fn from_url(url: String, mime: Option<String>, index: usize) -> Self {
        Self {
            path: None,
            url: Some(url),
            mime,
            index,
            data: None,
        }
    }

    pub fn from_path(path: String, mime: Option<String>, index: usize) -> Self {
        Self {
            path: Some(path),
            url: None,
            mime,
            index,
            data: None,
        }
    }

    pub fn capability(&self) -> Option<MediaCapability> {
        let mime = self.mime.as_ref()?;
        if mime.starts_with("image/") {
            Some(MediaCapability::Image)
        } else if mime.starts_with("audio/") {
            Some(MediaCapability::Audio)
        } else if mime.starts_with("video/") {
            Some(MediaCapability::Video)
        } else if mime == "application/pdf" || mime == "application/x-pdf" {
            Some(MediaCapability::Document)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfPageText {
    pub page: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaUnderstandingOutput {
    ImageDescription {
        attachment_index: usize,
        text: String,
        provider: String,
        model: Option<String>,
    },
    AudioTranscription {
        attachment_index: usize,
        text: String,
        provider: String,
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        waveform: Option<Vec<f32>>,
    },
    VideoDescription {
        attachment_index: usize,
        text: String,
        provider: String,
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thumbnail_base64: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        frame_count: Option<usize>,
    },
    DocumentText {
        attachment_index: usize,
        provider: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        pages: Vec<PdfPageText>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total_pages: Option<usize>,
        #[serde(default)]
        truncated: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDecisionStatus {
    Success,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDecision {
    pub attachment_index: usize,
    pub capability: Option<MediaCapability>,
    pub provider: Option<String>,
    pub status: ProviderDecisionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageDescriptionResult {
    pub text: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTranscriptionResult {
    pub text: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoDescriptionResult {
    pub text: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaUnderstandingResult {
    pub outputs: Vec<MediaUnderstandingOutput>,
    pub errors: Vec<String>,
    pub decisions: Vec<ProviderDecision>,
}

impl MediaUnderstandingResult {
    pub fn new() -> Self {
        Self {
            outputs: Vec::new(),
            errors: Vec::new(),
            decisions: Vec::new(),
        }
    }

    pub fn with_output(mut self, output: MediaUnderstandingOutput) -> Self {
        self.outputs.push(output);
        self
    }

    pub fn with_error(mut self, error: String) -> Self {
        self.errors.push(error);
        self
    }

    pub fn with_decision(mut self, decision: ProviderDecision) -> Self {
        self.decisions.push(decision);
        self
    }
}

impl Default for MediaUnderstandingResult {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaUnderstandingConfig {
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub enable_image: bool,
    #[serde(default = "default_true")]
    pub enable_audio: bool,
    #[serde(default = "default_true")]
    pub enable_video: bool,
    #[serde(default = "default_true")]
    pub enable_document: bool,
    #[serde(default = "default_provider_concurrency")]
    pub per_provider_concurrency: usize,
    pub image_provider: Option<String>,
    pub audio_provider: Option<String>,
    pub video_provider: Option<String>,
    #[serde(default = "default_image_max_dimension")]
    pub image_max_dimension: u32,
    #[serde(default = "default_image_quality")]
    pub image_quality: u8,
    pub image_model: Option<String>,
    pub audio_model: Option<String>,
    pub video_model: Option<String>,
    pub max_chars: Option<usize>,
    pub timeout_seconds: Option<u64>,
    #[serde(default = "default_pdf_max_pages")]
    pub pdf_max_pages: usize,
    #[serde(default = "default_video_frame_interval_seconds")]
    pub video_frame_interval_seconds: u64,
    #[serde(default = "default_video_max_frames")]
    pub video_max_frames: usize,
    #[serde(default = "default_true")]
    pub enable_video_thumbnail: bool,
    #[serde(default = "default_true")]
    pub enable_audio_waveform: bool,
}

fn default_true() -> bool {
    true
}

fn default_provider_concurrency() -> usize {
    2
}

fn default_image_max_dimension() -> u32 {
    1024
}

fn default_image_quality() -> u8 {
    85
}

fn default_pdf_max_pages() -> usize {
    50
}

fn default_video_frame_interval_seconds() -> u64 {
    3
}

fn default_video_max_frames() -> usize {
    8
}

impl Default for MediaUnderstandingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            enable_image: true,
            enable_audio: true,
            enable_video: true,
            enable_document: true,
            per_provider_concurrency: default_provider_concurrency(),
            image_provider: Some("openai".to_string()),
            audio_provider: Some("openai".to_string()),
            video_provider: Some("google".to_string()),
            image_max_dimension: default_image_max_dimension(),
            image_quality: default_image_quality(),
            image_model: Some("gpt-4o-mini".to_string()),
            audio_model: Some("whisper-1".to_string()),
            video_model: Some("gemini-2.0-flash".to_string()),
            max_chars: Some(500),
            timeout_seconds: None,
            pdf_max_pages: default_pdf_max_pages(),
            video_frame_interval_seconds: default_video_frame_interval_seconds(),
            video_max_frames: default_video_max_frames(),
            enable_video_thumbnail: true,
            enable_audio_waveform: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MediaAttachment, MediaCapability};

    #[test]
    fn capability_detects_pdf_documents() {
        let attachment =
            MediaAttachment::from_bytes(vec![1, 2, 3], Some("application/pdf".to_string()), 0);
        assert_eq!(attachment.capability(), Some(MediaCapability::Document));
    }
}
