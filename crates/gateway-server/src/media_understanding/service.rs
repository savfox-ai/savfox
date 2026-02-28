use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use futures_util::future::join_all;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageReader};
use lopdf::Document as LopdfDocument;
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use super::providers::{MediaProvider, MediaProviders};
use super::types::{
    MediaAttachment, MediaCapability, MediaUnderstandingConfig, MediaUnderstandingOutput,
    MediaUnderstandingResult, PdfPageText, ProviderDecision, ProviderDecisionStatus,
};
use crate::media_store::MediaStore;

const PDF_PROVIDER_ID: &str = "pdf-local";
const WAVEFORM_BUCKETS: usize = 64;

struct AudioPreflightResult {
    data: Vec<u8>,
    mime: String,
    detected_language: Option<String>,
    waveform: Option<Vec<f32>>,
}

struct ImagePreflightResult {
    data: Vec<u8>,
    mime: String,
    width: Option<u32>,
    height: Option<u32>,
    resized: bool,
    converted: bool,
    orientation_applied: bool,
}

struct VideoPreflightResult {
    frames: Vec<Vec<u8>>,
    thumbnail_base64: Option<String>,
}

pub struct MediaUnderstandingService {
    config: Arc<RwLock<MediaUnderstandingConfig>>,
    providers: Arc<RwLock<MediaProviders>>,
}

impl MediaUnderstandingService {
    pub fn new(config: MediaUnderstandingConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            providers: Arc::new(RwLock::new(MediaProviders::new())),
        }
    }

    pub fn with_providers(mut self, providers: MediaProviders) -> Self {
        self.providers = Arc::new(RwLock::new(providers));
        self
    }

    pub async fn register_provider(&self, provider: Box<dyn MediaProvider>) {
        let mut providers = self.providers.write().await;
        providers.register(provider);
    }

    pub async fn update_config(&self, config: MediaUnderstandingConfig) {
        let mut current = self.config.write().await;
        *current = config;
    }

    pub async fn process(&self, attachments: Vec<MediaAttachment>) -> MediaUnderstandingResult {
        let config = self.config.read().await.clone();

        if !config.enabled {
            return MediaUnderstandingResult::new();
        }

        let providers = self.providers.read().await;
        let mut result = MediaUnderstandingResult::new();
        let mut grouped: HashMap<String, Vec<(MediaAttachment, MediaCapability)>> = HashMap::new();

        for attachment in attachments {
            if attachment.capability() == Some(MediaCapability::Document) {
                if !Self::is_capability_enabled(&config, MediaCapability::Document) {
                    result = result.with_decision(ProviderDecision {
                        attachment_index: attachment.index,
                        capability: Some(MediaCapability::Document),
                        provider: Some(PDF_PROVIDER_ID.to_string()),
                        status: ProviderDecisionStatus::Skipped,
                        reason: Some("Document processing disabled by scope".to_string()),
                    });
                    continue;
                }

                match self.process_pdf_attachment(&attachment, &config).await {
                    Ok(output) => {
                        let mut truncation_warning = None;
                        if let MediaUnderstandingOutput::DocumentText {
                            pages,
                            total_pages: Some(total_pages),
                            truncated: true,
                            ..
                        } = &output
                        {
                            truncation_warning = Some(format!(
                                "Attachment {}: PDF page limit reached ({} of {} pages processed)",
                                attachment.index,
                                pages.len(),
                                total_pages
                            ));
                        }
                        result = result.with_decision(ProviderDecision {
                            attachment_index: attachment.index,
                            capability: Some(MediaCapability::Document),
                            provider: Some(PDF_PROVIDER_ID.to_string()),
                            status: ProviderDecisionStatus::Success,
                            reason: None,
                        });
                        result = result.with_output(output);
                        if let Some(warning) = truncation_warning {
                            result = result.with_error(warning);
                        }
                    }
                    Err(e) => {
                        let skipped = e.contains("Encrypted PDF");
                        result = result.with_decision(ProviderDecision {
                            attachment_index: attachment.index,
                            capability: Some(MediaCapability::Document),
                            provider: Some(PDF_PROVIDER_ID.to_string()),
                            status: if skipped {
                                ProviderDecisionStatus::Skipped
                            } else {
                                ProviderDecisionStatus::Failed
                            },
                            reason: Some(e.clone()),
                        });
                        result =
                            result.with_error(format!("Attachment {}: {}", attachment.index, e));
                    }
                }
                continue;
            }

            match self.resolve_provider_for_attachment(&attachment, &config, &providers) {
                Ok((capability, provider_id)) => {
                    grouped
                        .entry(provider_id)
                        .or_default()
                        .push((attachment, capability));
                }
                Err(e) => {
                    result = result.with_decision(ProviderDecision {
                        attachment_index: attachment.index,
                        capability: attachment.capability(),
                        provider: None,
                        status: ProviderDecisionStatus::Skipped,
                        reason: Some(e.clone()),
                    });
                    result = result.with_error(format!("Attachment {}: {}", attachment.index, e));
                }
            }
        }

        let per_provider_concurrency = config.per_provider_concurrency.max(1);
        for (provider_id, group) in grouped {
            for batch in group.chunks(per_provider_concurrency) {
                let futures = batch.iter().map(|(attachment, capability)| {
                    self.process_attachment_with_provider(
                        attachment,
                        *capability,
                        &provider_id,
                        &config,
                        &providers,
                    )
                });
                let batch_results = join_all(futures).await;

                for ((attachment, capability), item_result) in batch.iter().zip(batch_results) {
                    match item_result {
                        Ok(output) => {
                            result = result.with_decision(ProviderDecision {
                                attachment_index: attachment.index,
                                capability: Some(*capability),
                                provider: Some(provider_id.clone()),
                                status: ProviderDecisionStatus::Success,
                                reason: None,
                            });
                            result = result.with_output(output);
                        }
                        Err(e) => {
                            error!("Failed to process attachment {}: {}", attachment.index, e);
                            result = result.with_decision(ProviderDecision {
                                attachment_index: attachment.index,
                                capability: Some(*capability),
                                provider: Some(provider_id.clone()),
                                status: ProviderDecisionStatus::Failed,
                                reason: Some(e.clone()),
                            });
                            result = result
                                .with_error(format!("Attachment {}: {}", attachment.index, e));
                        }
                    }
                }
            }
        }

        result
    }

    fn resolve_provider_for_attachment(
        &self,
        attachment: &MediaAttachment,
        config: &MediaUnderstandingConfig,
        providers: &MediaProviders,
    ) -> Result<(MediaCapability, String), String> {
        let capability = attachment
            .capability()
            .ok_or_else(|| "Unknown media type".to_string())?;

        if !Self::is_capability_enabled(config, capability) {
            return Err(format!("{:?} processing disabled by scope", capability));
        }

        let provider_id = match capability {
            MediaCapability::Image => config.image_provider.as_deref(),
            MediaCapability::Audio => config.audio_provider.as_deref(),
            MediaCapability::Video => config.video_provider.as_deref(),
            MediaCapability::Document => None,
        };

        let provider = provider_id
            .and_then(|id| providers.get(id))
            .or_else(|| providers.get_for_capability(capability))
            .ok_or_else(|| format!("No provider for {:?}", capability))?;

        if !provider.capabilities().contains(&capability) {
            return Err(format!(
                "Provider {} doesn't support {:?}",
                provider.id(),
                capability
            ));
        }

        Ok((capability, provider.id().to_string()))
    }

    async fn process_attachment_with_provider(
        &self,
        attachment: &MediaAttachment,
        capability: MediaCapability,
        provider_id: &str,
        config: &MediaUnderstandingConfig,
        providers: &MediaProviders,
    ) -> Result<MediaUnderstandingOutput, String> {
        let provider = providers
            .get(provider_id)
            .ok_or_else(|| format!("Provider '{}' is not registered", provider_id))?;

        let data = self.load_attachment_data(attachment).await?;
        let mime = attachment
            .mime
            .as_deref()
            .unwrap_or("application/octet-stream");
        let model = match capability {
            MediaCapability::Image => config.image_model.as_deref(),
            MediaCapability::Audio => config.audio_model.as_deref(),
            MediaCapability::Video => config.video_model.as_deref(),
            MediaCapability::Document => None,
        };
        let prompt = capability.default_prompt();

        match capability {
            MediaCapability::Image => {
                let preflight = self.image_preflight(data, mime, config).await;
                let result = provider
                    .describe_image(&preflight.data, &preflight.mime, model, Some(prompt))
                    .await?;
                let dim_note = match (preflight.width, preflight.height) {
                    (Some(w), Some(h)) => format!("{w}x{h}"),
                    _ => "unknown-size".to_string(),
                };
                info!(
                    "Image described by {} ({} chars, {}, mime: {}, resized: {}, converted: {}, orientation_applied: {})",
                    provider.id(),
                    result.text.len(),
                    dim_note,
                    preflight.mime,
                    preflight.resized,
                    preflight.converted,
                    preflight.orientation_applied
                );
                Ok(MediaUnderstandingOutput::ImageDescription {
                    attachment_index: attachment.index,
                    text: result.text,
                    provider: provider.id().to_string(),
                    model: result.model,
                })
            }
            MediaCapability::Audio => {
                let preflight = self.audio_preflight(attachment, data, mime, config).await;
                let result = provider
                    .transcribe_audio(
                        &preflight.data,
                        &preflight.mime,
                        model,
                        preflight.detected_language.as_deref(),
                    )
                    .await?;
                info!(
                    "Audio transcribed by {} ({} chars)",
                    provider.id(),
                    result.text.len()
                );
                Ok(MediaUnderstandingOutput::AudioTranscription {
                    attachment_index: attachment.index,
                    text: result.text,
                    provider: provider.id().to_string(),
                    model: result.model,
                    waveform: preflight.waveform,
                })
            }
            MediaCapability::Video => {
                let preflight = self.video_preflight(&data, mime, config).await;

                let mut frame_lines = Vec::new();
                if !preflight.frames.is_empty()
                    && provider.capabilities().contains(&MediaCapability::Image)
                {
                    for (idx, frame) in preflight.frames.iter().enumerate() {
                        let frame_prompt = format!("Frame {}: {}", idx + 1, prompt);
                        if let Ok(frame_result) = provider
                            .describe_image(frame, "image/jpeg", model, Some(&frame_prompt))
                            .await
                        {
                            let text = frame_result.text.trim();
                            if !text.is_empty() {
                                frame_lines.push(format!("Frame {}: {text}", idx + 1));
                            }
                        }
                    }
                }

                let (text, model_used) = if !frame_lines.is_empty() {
                    (
                        format!(
                            "Extracted {} key frames ({}s interval):\n{}",
                            frame_lines.len(),
                            config.video_frame_interval_seconds.max(1),
                            frame_lines.join("\n")
                        ),
                        model.map(|m| m.to_string()),
                    )
                } else {
                    let result = provider
                        .describe_video(&data, mime, model, Some(prompt))
                        .await?;
                    (result.text, result.model)
                };

                info!(
                    "Video described by {} ({} chars, {} frame(s))",
                    provider.id(),
                    text.len(),
                    preflight.frames.len()
                );
                Ok(MediaUnderstandingOutput::VideoDescription {
                    attachment_index: attachment.index,
                    text,
                    provider: provider.id().to_string(),
                    model: model_used,
                    thumbnail_base64: preflight.thumbnail_base64,
                    frame_count: (!preflight.frames.is_empty()).then_some(preflight.frames.len()),
                })
            }
            MediaCapability::Document => Err("Document processing is local-only".to_string()),
        }
    }

    async fn process_pdf_attachment(
        &self,
        attachment: &MediaAttachment,
        config: &MediaUnderstandingConfig,
    ) -> Result<MediaUnderstandingOutput, String> {
        let data = self.load_attachment_data(attachment).await?;
        let page_limit = config.pdf_max_pages.max(1);
        let (pages, total_pages, truncated) =
            self.extract_pdf_text_pages(&data, page_limit, config.max_chars)?;
        Ok(MediaUnderstandingOutput::DocumentText {
            attachment_index: attachment.index,
            provider: PDF_PROVIDER_ID.to_string(),
            model: None,
            pages,
            total_pages: Some(total_pages),
            truncated,
        })
    }

    fn extract_pdf_text_pages(
        &self,
        data: &[u8],
        page_limit: usize,
        max_chars: Option<usize>,
    ) -> Result<(Vec<PdfPageText>, usize, bool), String> {
        if Self::looks_like_encrypted_pdf(data) {
            return Err("Encrypted PDF is not supported; skipping document".to_string());
        }

        let doc =
            LopdfDocument::load_mem(data).map_err(|e| format!("Failed to parse PDF: {}", e))?;

        if doc.is_encrypted() {
            return Err("Encrypted PDF is not supported; skipping document".to_string());
        }

        let pages = doc.get_pages();
        if pages.is_empty() {
            return Err("PDF has no pages".to_string());
        }

        let total_pages = pages.len();
        let max_pages = page_limit.max(1);
        let mut extracted = Vec::new();

        for page_number in pages.keys().take(max_pages) {
            let text = doc
                .extract_text(&[*page_number])
                .map_err(|e| format!("Failed to extract text from page {}: {}", page_number, e))?;
            let mut normalized = Self::normalize_page_text(&text);
            if let Some(max_chars) = max_chars {
                normalized = Self::truncate_chars(&normalized, max_chars);
            }
            extracted.push(PdfPageText {
                page: *page_number as usize,
                text: normalized,
            });
        }

        if extracted.iter().all(|page| page.text.trim().is_empty()) {
            return Err("PDF contained no extractable text".to_string());
        }

        Ok((extracted, total_pages, total_pages > max_pages))
    }

    fn looks_like_encrypted_pdf(data: &[u8]) -> bool {
        let marker = b"/Encrypt";
        data.windows(marker.len()).any(|window| window == marker)
    }

    fn normalize_page_text(text: &str) -> String {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn truncate_chars(text: &str, max_len: usize) -> String {
        if max_len == 0 {
            return String::new();
        }
        let count = text.chars().count();
        if count <= max_len {
            return text.to_string();
        }
        if max_len <= 3 {
            return ".".repeat(max_len);
        }
        let mut out = String::new();
        for ch in text.chars().take(max_len - 3) {
            out.push(ch);
        }
        out.push_str("...");
        out
    }

    async fn image_preflight(
        &self,
        data: Vec<u8>,
        mime: &str,
        config: &MediaUnderstandingConfig,
    ) -> ImagePreflightResult {
        let mut normalized_mime = Self::normalize_image_mime(mime);
        let mut prepared_data = data;
        let max_dimension = config.image_max_dimension.max(1);
        let quality = config.image_quality.clamp(1, 100);

        if Self::should_convert_image_with_ffmpeg(&normalized_mime)
            && let Some((converted, converted_mime)) = Self::convert_image_with_ffmpeg(
                &prepared_data,
                &normalized_mime,
                max_dimension,
                quality,
            )
            .await
        {
            prepared_data = converted;
            normalized_mime = converted_mime;
        }

        match Self::transcode_image(&prepared_data, &normalized_mime, max_dimension, quality) {
            Ok((bytes, output_mime, width, height, resized, converted, orientation_applied)) => {
                ImagePreflightResult {
                    data: bytes,
                    mime: output_mime,
                    width: Some(width),
                    height: Some(height),
                    resized,
                    converted,
                    orientation_applied,
                }
            }
            Err(err) => {
                warn!("Image preflight skipped: {}", err);
                ImagePreflightResult {
                    data: prepared_data,
                    mime: normalized_mime,
                    width: None,
                    height: None,
                    resized: false,
                    converted: false,
                    orientation_applied: false,
                }
            }
        }
    }

    fn transcode_image(
        data: &[u8],
        mime: &str,
        max_dimension: u32,
        quality: u8,
    ) -> Result<(Vec<u8>, String, u32, u32, bool, bool, bool), String> {
        let mut reader = if let Ok(format) = image::guess_format(data) {
            ImageReader::with_format(Cursor::new(data), format)
        } else {
            ImageReader::new(Cursor::new(data))
        };
        reader = reader
            .with_guessed_format()
            .map_err(|e| format!("Failed to detect image format: {}", e))?;

        let mut decoder = reader
            .into_decoder()
            .map_err(|e| format!("Failed to initialize image decoder: {}", e))?;
        let orientation = decoder
            .orientation()
            .unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut image = DynamicImage::from_decoder(decoder)
            .map_err(|e| format!("Failed to decode image: {}", e))?;
        let mut orientation_applied = false;
        if orientation != image::metadata::Orientation::NoTransforms {
            image.apply_orientation(orientation);
            orientation_applied = true;
        }

        let (orig_width, orig_height) = image.dimensions();
        let mut resized = false;
        let max_dimension = max_dimension.max(1);
        if orig_width > max_dimension || orig_height > max_dimension {
            image = image.resize(max_dimension, max_dimension, FilterType::Lanczos3);
            resized = true;
        }

        let has_alpha = image.color().has_alpha();
        let target_mime = if has_alpha { "image/png" } else { "image/jpeg" };
        let mut out = Vec::new();

        if target_mime == "image/jpeg" {
            let mut encoder = JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
            encoder
                .encode_image(&image)
                .map_err(|e| format!("Failed to encode JPEG: {}", e))?;
        } else {
            let rgba = image.to_rgba8();
            let encoder = PngEncoder::new(&mut out);
            encoder
                .write_image(
                    rgba.as_raw(),
                    image.width(),
                    image.height(),
                    image::ColorType::Rgba8.into(),
                )
                .map_err(|e| format!("Failed to encode PNG: {}", e))?;
        }

        let output_mime = target_mime.to_string();
        let converted = mime != output_mime || resized || orientation_applied;
        Ok((
            out,
            output_mime,
            image.width(),
            image.height(),
            resized,
            converted,
            orientation_applied,
        ))
    }

    fn normalize_image_mime(mime: &str) -> String {
        match mime {
            "image/jpg" => "image/jpeg".to_string(),
            "image/x-png" => "image/png".to_string(),
            "image/x-webp" => "image/webp".to_string(),
            "image/x-heic" => "image/heic".to_string(),
            "image/x-heif" => "image/heif".to_string(),
            other => other.to_string(),
        }
    }

    fn should_convert_image_with_ffmpeg(mime: &str) -> bool {
        matches!(mime, "image/heic" | "image/heif")
    }

    async fn convert_image_with_ffmpeg(
        data: &[u8],
        mime: &str,
        max_dimension: u32,
        quality: u8,
    ) -> Option<(Vec<u8>, String)> {
        if !Self::ffmpeg_available().await {
            return None;
        }

        let temp_dir = Self::build_temp_dir("image-preflight");
        if tokio::fs::create_dir_all(&temp_dir).await.is_err() {
            return None;
        }

        let input_path = temp_dir.join(format!("input.{}", Self::extension_for_mime(mime)));
        let output_path = temp_dir.join("output.jpg");
        if tokio::fs::write(&input_path, data).await.is_err() {
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            return None;
        }

        let qv = (((100u16.saturating_sub(quality as u16)) * 30) / 100 + 1).clamp(2, 31);
        let max_dimension = max_dimension.max(1);
        let scale_filter = format!(
            "scale='if(gt(iw,ih),min(iw,{0}),-2)':'if(gt(iw,ih),-2,min(ih,{0}))'",
            max_dimension
        );

        let output = Command::new("ffmpeg")
            .arg("-y")
            .arg("-i")
            .arg(&input_path)
            .arg("-vf")
            .arg(scale_filter)
            .arg("-q:v")
            .arg(qv.to_string())
            .arg(&output_path)
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            return None;
        }

        let converted = tokio::fs::read(&output_path).await.ok()?;
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        Some((converted, "image/jpeg".to_string()))
    }

    async fn audio_preflight(
        &self,
        attachment: &MediaAttachment,
        data: Vec<u8>,
        mime: &str,
        config: &MediaUnderstandingConfig,
    ) -> AudioPreflightResult {
        let mut normalized_mime = match mime {
            "audio/x-wav" | "audio/wave" => "audio/wav".to_string(),
            "audio/x-m4a" => "audio/m4a".to_string(),
            "audio/x-caf" => "audio/caf".to_string(),
            other => other.to_string(),
        };
        let mut prepared_data = data;

        if Self::should_convert_audio(&normalized_mime)
            && let Some(converted) =
                Self::convert_audio_to_wav_with_ffmpeg(&prepared_data, &normalized_mime).await
        {
            prepared_data = converted;
            normalized_mime = "audio/wav".to_string();
        }

        let detected_language = Self::detect_language_hint(attachment);
        let waveform = if config.enable_audio_waveform {
            Self::generate_waveform(&prepared_data, &normalized_mime, WAVEFORM_BUCKETS)
        } else {
            None
        };

        AudioPreflightResult {
            data: prepared_data,
            mime: normalized_mime,
            detected_language,
            waveform,
        }
    }

    fn should_convert_audio(mime: &str) -> bool {
        matches!(
            mime,
            "audio/caf" | "audio/x-caf" | "audio/m4a" | "audio/x-m4a" | "audio/mp4" | "audio/aac"
        )
    }

    fn detect_language_hint(attachment: &MediaAttachment) -> Option<String> {
        if let Some(url) = attachment.url.as_deref() {
            if let Some(lang) = Self::extract_lang_from_query(url) {
                return Some(lang);
            }
            if let Some(lang) = Self::extract_lang_from_text(url) {
                return Some(lang);
            }
        }
        if let Some(path) = attachment.path.as_deref()
            && let Some(lang) = Self::extract_lang_from_text(path)
        {
            return Some(lang);
        }
        None
    }

    fn extract_lang_from_query(url: &str) -> Option<String> {
        let query = url.split('?').nth(1)?;
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
            let value = parts.next().unwrap_or_default().trim();
            if (key == "lang" || key == "language")
                && let Some(normalized) = Self::normalize_lang_code(value)
            {
                return Some(normalized);
            }
        }
        None
    }

    fn extract_lang_from_text(value: &str) -> Option<String> {
        for token in value.split(['/', '\\', '?', '&', '=', '.', '_', '-', ':']) {
            if let Some(normalized) = Self::normalize_lang_code(token) {
                return Some(normalized);
            }
        }
        None
    }

    fn normalize_lang_code(raw: &str) -> Option<String> {
        let token = raw.trim();
        if token.len() == 2 && token.chars().all(|c| c.is_ascii_alphabetic()) {
            return Some(token.to_ascii_lowercase());
        }
        if token.len() == 3 && token.chars().all(|c| c.is_ascii_alphabetic()) {
            return Some(token.to_ascii_lowercase());
        }
        if token.len() == 5 {
            let bytes = token.as_bytes();
            if (bytes[2] == b'-' || bytes[2] == b'_')
                && bytes[..2].iter().all(u8::is_ascii_alphabetic)
                && bytes[3..].iter().all(u8::is_ascii_alphabetic)
            {
                let lower = token.to_ascii_lowercase().replace('_', "-");
                return Some(lower);
            }
        }
        None
    }

    fn generate_waveform(data: &[u8], mime: &str, buckets: usize) -> Option<Vec<f32>> {
        if buckets == 0 || data.is_empty() {
            return None;
        }

        if mime == "audio/wav"
            && let Some(samples) = Self::waveform_from_wav(data, buckets)
        {
            return Some(samples);
        }

        Some(Self::waveform_from_bytes(data, buckets))
    }

    fn waveform_from_wav(data: &[u8], buckets: usize) -> Option<Vec<f32>> {
        if data.len() < 44 {
            return None;
        }
        if &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
            return None;
        }

        let mut offset = 12usize;
        let mut pcm_data: Option<&[u8]> = None;
        while offset + 8 <= data.len() {
            let chunk_id = &data[offset..offset + 4];
            let chunk_size = u32::from_le_bytes([
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]) as usize;
            offset += 8;
            if offset + chunk_size > data.len() {
                break;
            }
            if chunk_id == b"data" {
                pcm_data = Some(&data[offset..offset + chunk_size]);
                break;
            }
            offset += chunk_size + (chunk_size % 2);
        }

        let pcm = pcm_data?;
        if pcm.len() < 2 {
            return None;
        }

        let mut amplitudes = Vec::with_capacity(pcm.len() / 2);
        for sample in pcm.chunks_exact(2) {
            let value = i16::from_le_bytes([sample[0], sample[1]]);
            let amp = (i32::from(value).abs() as f32) / (i16::MAX as f32);
            amplitudes.push(amp.min(1.0));
        }
        Self::compress_waveform(&amplitudes, buckets)
    }

    fn waveform_from_bytes(data: &[u8], buckets: usize) -> Vec<f32> {
        let chunk_size = data.len().div_ceil(buckets).max(1);
        let mut out = Vec::new();
        for chunk in data.chunks(chunk_size).take(buckets) {
            if chunk.is_empty() {
                continue;
            }
            let mut energy = 0_f32;
            for b in chunk {
                let centered = (*b as f32) - 128.0;
                energy += centered.abs() / 128.0;
            }
            out.push((energy / chunk.len() as f32).min(1.0));
        }
        if out.is_empty() { vec![0.0] } else { out }
    }

    fn compress_waveform(samples: &[f32], buckets: usize) -> Option<Vec<f32>> {
        if samples.is_empty() || buckets == 0 {
            return None;
        }
        let chunk_size = samples.len().div_ceil(buckets).max(1);
        let mut out = Vec::new();
        for chunk in samples.chunks(chunk_size).take(buckets) {
            let peak = chunk
                .iter()
                .copied()
                .fold(0.0_f32, |acc, value| if value > acc { value } else { acc });
            out.push(peak.min(1.0));
        }
        if out.is_empty() { None } else { Some(out) }
    }

    async fn video_preflight(
        &self,
        data: &[u8],
        mime: &str,
        config: &MediaUnderstandingConfig,
    ) -> VideoPreflightResult {
        let frames = Self::extract_video_frames_with_ffmpeg(
            data,
            mime,
            config.video_frame_interval_seconds.max(1),
            config.video_max_frames.max(1),
        )
        .await
        .unwrap_or_default();

        let thumbnail_base64 = if config.enable_video_thumbnail {
            Self::generate_video_thumbnail_with_ffmpeg(data, mime).await
        } else {
            None
        };

        VideoPreflightResult {
            frames,
            thumbnail_base64,
        }
    }

    async fn extract_video_frames_with_ffmpeg(
        data: &[u8],
        mime: &str,
        interval_seconds: u64,
        max_frames: usize,
    ) -> Result<Vec<Vec<u8>>, String> {
        if !Self::ffmpeg_available().await {
            return Ok(Vec::new());
        }

        let temp_dir = Self::build_temp_dir("video-frames");
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .map_err(|e| format!("Failed to create temp dir: {}", e))?;

        let input_path = temp_dir.join(format!("input.{}", Self::extension_for_mime(mime)));
        tokio::fs::write(&input_path, data)
            .await
            .map_err(|e| format!("Failed to write temp video: {}", e))?;

        let output_pattern = temp_dir.join("frame-%03d.jpg");
        let output = Command::new("ffmpeg")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg(&input_path)
            .arg("-vf")
            .arg(format!("fps=1/{}", interval_seconds.max(1)))
            .arg("-frames:v")
            .arg(max_frames.max(1).to_string())
            .arg(&output_pattern)
            .output()
            .await
            .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;

        if !output.status.success() {
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            return Ok(Vec::new());
        }

        let mut frame_paths = Vec::new();
        let mut entries = tokio::fs::read_dir(&temp_dir)
            .await
            .map_err(|e| format!("Failed to read temp dir: {}", e))?;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("frame-") && name.ends_with(".jpg") {
                frame_paths.push(entry.path());
            }
        }
        frame_paths.sort();

        let mut frames = Vec::new();
        for frame_path in frame_paths {
            if let Ok(bytes) = tokio::fs::read(frame_path).await {
                frames.push(bytes);
            }
        }

        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        Ok(frames)
    }

    async fn generate_video_thumbnail_with_ffmpeg(data: &[u8], mime: &str) -> Option<String> {
        if !Self::ffmpeg_available().await {
            return None;
        }

        let temp_dir = Self::build_temp_dir("video-thumb");
        if tokio::fs::create_dir_all(&temp_dir).await.is_err() {
            return None;
        }
        let input_path = temp_dir.join(format!("input.{}", Self::extension_for_mime(mime)));
        if tokio::fs::write(&input_path, data).await.is_err() {
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            return None;
        }

        let output_path = temp_dir.join("thumbnail.jpg");
        let output = Command::new("ffmpeg")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg(&input_path)
            .arg("-vf")
            .arg("thumbnail,scale=320:-1")
            .arg("-frames:v")
            .arg("1")
            .arg(&output_path)
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            return None;
        }

        let encoded = tokio::fs::read(&output_path)
            .await
            .ok()
            .map(|thumb| base64::engine::general_purpose::STANDARD.encode(thumb));
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        encoded
    }

    async fn convert_audio_to_wav_with_ffmpeg(data: &[u8], mime: &str) -> Option<Vec<u8>> {
        if !Self::ffmpeg_available().await {
            return None;
        }

        let temp_dir = Self::build_temp_dir("audio-convert");
        if tokio::fs::create_dir_all(&temp_dir).await.is_err() {
            return None;
        }

        let input_path = temp_dir.join(format!("input.{}", Self::extension_for_mime(mime)));
        if tokio::fs::write(&input_path, data).await.is_err() {
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            return None;
        }
        let output_path = temp_dir.join("output.wav");

        let output = Command::new("ffmpeg")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg(&input_path)
            .arg("-ar")
            .arg("16000")
            .arg("-ac")
            .arg("1")
            .arg(&output_path)
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            return None;
        }

        let converted = tokio::fs::read(&output_path).await.ok();
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        converted
    }

    async fn ffmpeg_available() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn build_temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("savfox-{prefix}-{}", uuid::Uuid::now_v7()))
    }

    fn extension_for_mime(mime: &str) -> &'static str {
        match mime {
            "image/jpeg" | "image/jpg" => "jpg",
            "image/png" => "png",
            "image/webp" => "webp",
            "image/heic" | "image/heif" => "heic",
            "video/mp4" => "mp4",
            "video/webm" => "webm",
            "video/quicktime" => "mov",
            "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
            "audio/m4a" | "audio/x-m4a" | "audio/mp4" => "m4a",
            "audio/caf" | "audio/x-caf" => "caf",
            _ => "bin",
        }
    }

    fn is_capability_enabled(
        config: &MediaUnderstandingConfig,
        capability: MediaCapability,
    ) -> bool {
        match capability {
            MediaCapability::Image => config.enable_image,
            MediaCapability::Audio => config.enable_audio,
            MediaCapability::Video => config.enable_video,
            MediaCapability::Document => config.enable_document,
        }
    }

    async fn load_attachment_data(&self, attachment: &MediaAttachment) -> Result<Vec<u8>, String> {
        if let Some(ref data) = attachment.data {
            return Ok(data.clone());
        }

        if let Some(ref path) = attachment.path {
            return tokio::fs::read(path)
                .await
                .map_err(|e| format!("Failed to read file {}: {}", path, e));
        }

        if let Some(ref url) = attachment.url {
            let store = MediaStore::from_env_or_default();
            let entry = store
                .fetch_and_store(url, None, attachment.mime.as_deref())
                .await
                .map_err(|e| format!("Failed to fetch URL {}: {}", url, e))?;
            let (_, data) = store
                .read(&entry.id)
                .await
                .map_err(|e| format!("Failed to read stored media {}: {}", entry.id, e))?;
            return Ok(data);
        }

        Err("No data source in attachment".to_string())
    }

    pub async fn format_output(&self, output: &MediaUnderstandingOutput) -> String {
        match output {
            MediaUnderstandingOutput::ImageDescription {
                text,
                provider,
                model,
                ..
            } => {
                let model_str = model
                    .as_ref()
                    .map(|m| format!(" ({})", m))
                    .unwrap_or_default();
                format!("[Image via {}{}]: {}", provider, model_str, text)
            }
            MediaUnderstandingOutput::AudioTranscription {
                text,
                provider,
                model,
                ..
            } => {
                let model_str = model
                    .as_ref()
                    .map(|m| format!(" ({})", m))
                    .unwrap_or_default();
                format!("[Transcript via {}{}]: {}", provider, model_str, text)
            }
            MediaUnderstandingOutput::VideoDescription {
                text,
                provider,
                model,
                ..
            } => {
                let model_str = model
                    .as_ref()
                    .map(|m| format!(" ({})", m))
                    .unwrap_or_default();
                format!("[Video via {}{}]: {}", provider, model_str, text)
            }
            MediaUnderstandingOutput::DocumentText {
                provider,
                pages,
                total_pages,
                truncated,
                ..
            } => {
                let prefix = if *truncated {
                    match total_pages {
                        Some(total) => {
                            format!(
                                "[PDF via {}] ({} / {} pages):",
                                provider,
                                pages.len(),
                                total
                            )
                        }
                        None => format!("[PDF via {}] (truncated):", provider),
                    }
                } else {
                    format!("[PDF via {}]:", provider)
                };

                let text = pages
                    .iter()
                    .map(|page| format!("p{}: {}", page.page, page.text))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{prefix}\n{text}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};

    use super::*;
    use crate::media_understanding::types::{
        AudioTranscriptionResult, ImageDescriptionResult, MediaUnderstandingConfig,
    };

    struct MockImageProvider;

    #[async_trait::async_trait]
    impl MediaProvider for MockImageProvider {
        fn id(&self) -> &str {
            "mock-image"
        }

        fn capabilities(&self) -> Vec<MediaCapability> {
            vec![MediaCapability::Image]
        }

        async fn describe_image(
            &self,
            _data: &[u8],
            _mime: &str,
            _model: Option<&str>,
            _prompt: Option<&str>,
        ) -> Result<ImageDescriptionResult, String> {
            Ok(ImageDescriptionResult {
                text: "ok".to_string(),
                model: Some("mock".to_string()),
            })
        }
    }

    struct MockAudioProvider;

    #[async_trait::async_trait]
    impl MediaProvider for MockAudioProvider {
        fn id(&self) -> &str {
            "mock-audio"
        }

        fn capabilities(&self) -> Vec<MediaCapability> {
            vec![MediaCapability::Audio]
        }

        async fn transcribe_audio(
            &self,
            _data: &[u8],
            _mime: &str,
            _model: Option<&str>,
            language: Option<&str>,
        ) -> Result<AudioTranscriptionResult, String> {
            Ok(AudioTranscriptionResult {
                text: language.unwrap_or("none").to_string(),
                model: Some("mock-audio-model".to_string()),
            })
        }
    }

    fn build_pdf_with_text(pages: &[&str]) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });

        let mut kids = Vec::new();
        for page_text in pages {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new(
                        "Tf",
                        vec![Object::Name(b"F1".to_vec()), Object::Integer(14)],
                    ),
                    Operation::new("Td", vec![Object::Integer(50), Object::Integer(760)]),
                    Operation::new("Tj", vec![Object::string_literal(*page_text)]),
                    Operation::new("ET", vec![]),
                ],
            };
            let stream = Stream::new(dictionary! {}, content.encode().expect("encode content"));
            let content_id = doc.add_object(stream);
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
                "Contents" => content_id,
                "Resources" => dictionary! {
                    "Font" => dictionary! {
                        "F1" => font_id
                    },
                },
            });
            kids.push(page_id.into());
        }

        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => pages.len() as i64,
            }),
        );

        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);

        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("write pdf");
        bytes
    }

    fn build_wav_sample() -> Vec<u8> {
        let samples: [i16; 16] = [
            0, 1000, -1000, 2000, -2000, 2500, -2500, 3000, -3000, 3500, -3500, 4000, -4000, 4500,
            -4500, 5000,
        ];
        let mut pcm = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            pcm.extend_from_slice(&sample.to_le_bytes());
        }

        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&(16000u32 * 2).to_le_bytes()); // byte rate
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        wav.extend_from_slice(&pcm);
        wav
    }

    fn build_png(width: u32, height: u32) -> Vec<u8> {
        let image = ImageBuffer::from_fn(width, height, |x, y| {
            let r = (x % 255) as u8;
            let g = (y % 255) as u8;
            let b = ((x + y) % 255) as u8;
            Rgb([r, g, b])
        });
        let dynamic = DynamicImage::ImageRgb8(image);
        let mut cursor = Cursor::new(Vec::new());
        dynamic
            .write_to(&mut cursor, ImageFormat::Png)
            .expect("encode png");
        cursor.into_inner()
    }

    #[tokio::test]
    async fn scope_disabled_marks_attachment_skipped() {
        let mut cfg = MediaUnderstandingConfig::default();
        cfg.enable_image = false;
        let service = MediaUnderstandingService::new(cfg);
        service.register_provider(Box::new(MockImageProvider)).await;

        let attachment =
            MediaAttachment::from_bytes(vec![1, 2, 3], Some("image/png".to_string()), 0);
        let result = service.process(vec![attachment]).await;

        assert_eq!(result.outputs.len(), 0);
        assert_eq!(result.decisions.len(), 1);
        assert!(matches!(
            result.decisions[0].status,
            ProviderDecisionStatus::Skipped
        ));
    }

    #[tokio::test]
    async fn success_records_provider_decision() {
        let mut cfg = MediaUnderstandingConfig::default();
        cfg.image_provider = Some("mock-image".to_string());
        let service = MediaUnderstandingService::new(cfg);
        service.register_provider(Box::new(MockImageProvider)).await;

        let attachment =
            MediaAttachment::from_bytes(vec![1, 2, 3], Some("image/png".to_string()), 2);
        let result = service.process(vec![attachment]).await;

        assert_eq!(result.outputs.len(), 1);
        assert_eq!(result.decisions.len(), 1);
        assert!(matches!(
            result.decisions[0].status,
            ProviderDecisionStatus::Success
        ));
        assert_eq!(result.decisions[0].provider.as_deref(), Some("mock-image"));
    }

    #[tokio::test]
    async fn image_preflight_resizes_and_reencodes() {
        let mut cfg = MediaUnderstandingConfig::default();
        cfg.image_max_dimension = 512;
        cfg.image_quality = 72;
        let service = MediaUnderstandingService::new(cfg.clone());

        let input = build_png(2200, 1300);
        let preflight = service.image_preflight(input, "image/png", &cfg).await;

        assert_eq!(preflight.mime, "image/jpeg");
        assert!(preflight.resized);
        assert!(preflight.converted);

        let parsed = image::load_from_memory(&preflight.data).expect("decode preflight output");
        assert!(parsed.width() <= 512);
        assert!(parsed.height() <= 512);
    }

    #[test]
    fn image_preflight_quality_is_configurable() {
        let input = build_png(1200, 900);
        let (high_bytes, ..) =
            MediaUnderstandingService::transcode_image(&input, "image/png", 1024, 95)
                .expect("high quality transcode");
        let (low_bytes, ..) =
            MediaUnderstandingService::transcode_image(&input, "image/png", 1024, 35)
                .expect("low quality transcode");
        assert!(
            low_bytes.len() < high_bytes.len(),
            "lower quality output should be smaller"
        );
    }

    #[tokio::test]
    async fn pdf_is_processed_locally_per_page() {
        let service = MediaUnderstandingService::new(MediaUnderstandingConfig::default());
        let pdf = build_pdf_with_text(&["Hello PDF"]);
        let attachment = MediaAttachment::from_bytes(pdf, Some("application/pdf".to_string()), 7);
        let result = service.process(vec![attachment]).await;

        assert_eq!(result.outputs.len(), 1);
        assert_eq!(result.decisions.len(), 1);
        assert!(matches!(
            result.decisions[0].status,
            ProviderDecisionStatus::Success
        ));
        match &result.outputs[0] {
            MediaUnderstandingOutput::DocumentText { pages, .. } => {
                assert_eq!(pages.len(), 1);
                assert!(pages[0].text.contains("Hello"));
            }
            other => panic!("unexpected output variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn pdf_page_limit_generates_warning() {
        let mut cfg = MediaUnderstandingConfig::default();
        cfg.pdf_max_pages = 2;
        let service = MediaUnderstandingService::new(cfg);
        let pdf = build_pdf_with_text(&["one", "two", "three"]);
        let attachment = MediaAttachment::from_bytes(pdf, Some("application/pdf".to_string()), 9);
        let result = service.process(vec![attachment]).await;

        match &result.outputs[0] {
            MediaUnderstandingOutput::DocumentText {
                pages,
                total_pages,
                truncated,
                ..
            } => {
                assert_eq!(pages.len(), 2);
                assert_eq!(*total_pages, Some(3));
                assert!(*truncated);
            }
            other => panic!("unexpected output variant: {other:?}"),
        }
        assert!(
            result
                .errors
                .iter()
                .any(|line| line.contains("page limit reached"))
        );
    }

    #[tokio::test]
    async fn encrypted_pdf_is_skipped_with_warning() {
        let service = MediaUnderstandingService::new(MediaUnderstandingConfig::default());
        let payload = b"%PDF-1.4\n1 0 obj\n<< /Encrypt 2 0 R >>\nendobj\n".to_vec();
        let attachment =
            MediaAttachment::from_bytes(payload, Some("application/pdf".to_string()), 1);
        let result = service.process(vec![attachment]).await;

        assert_eq!(result.outputs.len(), 0);
        assert_eq!(result.decisions.len(), 1);
        assert!(matches!(
            result.decisions[0].status,
            ProviderDecisionStatus::Skipped
        ));
        assert!(
            result.decisions[0]
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("Encrypted PDF")
        );
    }

    #[tokio::test]
    async fn audio_preflight_detects_language_and_waveform() {
        let mut cfg = MediaUnderstandingConfig::default();
        cfg.audio_provider = Some("mock-audio".to_string());
        let service = MediaUnderstandingService::new(cfg);
        service.register_provider(Box::new(MockAudioProvider)).await;

        let attachment = MediaAttachment {
            path: None,
            url: Some("https://example.com/clip.wav?lang=ja".to_string()),
            mime: Some("audio/wav".to_string()),
            index: 4,
            data: Some(build_wav_sample()),
        };
        let result = service.process(vec![attachment]).await;
        assert_eq!(result.outputs.len(), 1);

        match &result.outputs[0] {
            MediaUnderstandingOutput::AudioTranscription { text, waveform, .. } => {
                assert_eq!(text, "ja");
                assert!(waveform.as_ref().is_some_and(|bins| !bins.is_empty()));
            }
            other => panic!("unexpected output variant: {other:?}"),
        }
    }
}
