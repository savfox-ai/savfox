use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;
use tracing::debug;

/// Wake word event emitted when detection occurs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum WakeWordEvent {
    Detected { keyword: String, timestamp: String },
    Started,
    Stopped,
    Error { message: String },
}

/// Simple wake word detector using text matching on transcribed speech.
/// In production, this would use a proper wake word engine like Porcupine or Vosk.
pub struct WakeWordDetector {
    keyword: String,
    keyword_lower: String,
    sensitivity: f32,
    audio_buffer: Arc<Mutex<Vec<u8>>>,
}

impl WakeWordDetector {
    #[must_use]
    pub fn new(keyword: &str, sensitivity: f32) -> Self {
        Self {
            keyword: keyword.to_owned(),
            keyword_lower: keyword.to_lowercase(),
            sensitivity,
            audio_buffer: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Check if the wake word is present in transcribed text.
    /// Returns Some(keyword) if detected, None otherwise.
    pub async fn detect(&self) -> Result<Option<String>> {
        // This is a placeholder implementation.
        // In a real implementation, this would:
        // 1. Capture audio from microphone
        // 2. Send audio to a speech-to-text service
        // 3. Check if the transcribed text contains the wake word

        // For now, we'll use a simple text matching approach
        // that would be triggered by external audio transcription

        Ok(None)
    }

    /// Process transcribed text and check for wake word.
    pub fn check_text(&self, text: &str) -> Option<String> {
        let text_lower = text.to_lowercase();

        // Check for exact match or near-match based on sensitivity
        if text_lower.contains(&self.keyword_lower) {
            return Some(self.keyword.clone());
        }

        // Check for fuzzy match if sensitivity allows
        if self.sensitivity < 0.8 {
            let similarity = self.calculate_similarity(&text_lower, &self.keyword_lower);
            if similarity >= self.sensitivity {
                debug!(
                    keyword = %self.keyword,
                    text = %text,
                    similarity = %similarity,
                    "Fuzzy wake word match detected"
                );
                return Some(self.keyword.clone());
            }
        }

        None
    }

    /// Calculate similarity between two strings (simple Levenshtein-based).
    fn calculate_similarity(&self, s1: &str, s2: &str) -> f32 {
        if s1.is_empty() && s2.is_empty() {
            return 1.0;
        }
        if s1.is_empty() || s2.is_empty() {
            return 0.0;
        }

        let _len1 = s1.chars().count();
        let _len2 = s2.chars().count();

        // Use a simple Jaccard similarity on word sets
        let words1: std::collections::HashSet<&str> = s1.split_whitespace().collect();
        let words2: std::collections::HashSet<&str> = s2.split_whitespace().collect();

        if words1.is_empty() && words2.is_empty() {
            return 1.0;
        }

        let intersection = words1.intersection(&words2).count();
        let union = words1.union(&words2).count();

        if union == 0 {
            return 0.0;
        }

        intersection as f32 / union as f32
    }

    /// Add audio data to the buffer for processing.
    pub async fn add_audio(&self, audio: &[u8]) {
        let mut buffer = self.audio_buffer.lock().await;
        buffer.extend_from_slice(audio);

        // Keep buffer size reasonable (max ~10 seconds at 16kHz 16-bit mono)
        const MAX_BUFFER_SIZE: usize = 16000 * 2 * 10;
        if buffer.len() > MAX_BUFFER_SIZE {
            let excess = buffer.len() - MAX_BUFFER_SIZE;
            buffer.drain(0..excess);
        }
    }

    /// Clear the audio buffer.
    pub async fn clear_buffer(&self) {
        self.audio_buffer.lock().await.clear();
    }

    /// Get the current keyword.
    #[must_use]
    pub fn keyword(&self) -> &str {
        &self.keyword
    }

    /// Get the sensitivity level.
    #[must_use]
    pub fn sensitivity(&self) -> f32 {
        self.sensitivity
    }

    /// Update the wake word keyword.
    pub fn set_keyword(&mut self, keyword: &str) {
        self.keyword = keyword.to_owned();
        self.keyword_lower = keyword.to_lowercase();
    }

    /// Update the sensitivity level.
    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.sensitivity = sensitivity.clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let detector = WakeWordDetector::new("hey savfox", 0.5);
        assert!(detector.check_text("hey savfox").is_some());
        assert!(detector.check_text("hey savfox how are you").is_some());
        assert!(detector.check_text("HEY SAVFOX").is_some());
    }

    #[test]
    fn test_no_match() {
        let detector = WakeWordDetector::new("hey savfox", 0.9);
        assert!(detector.check_text("hello world").is_none());
        assert!(detector.check_text("hey there").is_none());
    }

    #[test]
    fn test_similarity() {
        let detector = WakeWordDetector::new("hey savfox", 0.3);
        // With low sensitivity, partial matches may trigger
        let result = detector.calculate_similarity("hey fox", "hey savfox");
        assert!(result > 0.0 && result < 1.0);
    }
}
