use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Mutex, broadcast};
use tracing::{debug, info, warn};

/// Audio capture configuration.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub frame_size_ms: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            channels: 1,
            bits_per_sample: 16,
            frame_size_ms: 20,
        }
    }
}

/// Audio frame containing captured audio data.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub data: Vec<u8>,
    pub timestamp_ms: u64,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Audio capture trait for different backends.
#[async_trait::async_trait]
pub trait AudioCapture: Send + Sync {
    async fn start(&mut self) -> Result<()>;
    async fn stop(&mut self) -> Result<()>;
    async fn is_capturing(&self) -> bool;
    fn subscribe(&self) -> broadcast::Receiver<AudioFrame>;
}

/// Mock audio capture for testing (doesn't capture real audio).
pub struct MockAudioCapture {
    config: AudioConfig,
    running: bool,
    frame_tx: broadcast::Sender<AudioFrame>,
}

impl MockAudioCapture {
    pub fn new(config: AudioConfig) -> Self {
        let (frame_tx, _) = broadcast::channel(64);
        Self {
            config,
            running: false,
            frame_tx,
        }
    }

    /// Simulate receiving audio data (for testing).
    pub fn inject_audio(&self, data: Vec<u8>) {
        let frame = AudioFrame {
            data,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            sample_rate: self.config.sample_rate,
            channels: self.config.channels,
        };
        let _ = self.frame_tx.send(frame);
    }
}

#[async_trait::async_trait]
impl AudioCapture for MockAudioCapture {
    async fn start(&mut self) -> Result<()> {
        self.running = true;
        info!("Mock audio capture started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        self.running = false;
        info!("Mock audio capture stopped");
        Ok(())
    }

    async fn is_capturing(&self) -> bool {
        self.running
    }

    fn subscribe(&self) -> broadcast::Receiver<AudioFrame> {
        self.frame_tx.subscribe()
    }
}

/// Audio level detector for voice activity detection.
pub struct AudioLevelDetector {
    threshold: f32,
    sample_rate: u32,
}

impl AudioLevelDetector {
    pub fn new(threshold: f32, sample_rate: u32) -> Self {
        Self {
            threshold,
            sample_rate,
        }
    }

    /// Calculate RMS level of audio samples.
    pub fn calculate_level(&self, samples: &[i16]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }

        let sum_squares: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();

        let rms = (sum_squares / samples.len() as f64).sqrt();

        // Normalize to 0.0 - 1.0 range (assuming 16-bit audio)
        (rms / 32768.0) as f32
    }

    /// Check if audio level exceeds threshold (voice activity detected).
    pub fn is_voice_active(&self, samples: &[i16]) -> bool {
        self.calculate_level(samples) >= self.threshold
    }

    /// Set the voice activity threshold.
    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold.clamp(0.0, 1.0);
    }

    /// Get the current threshold.
    pub fn threshold(&self) -> f32 {
        self.threshold
    }
}

/// Convert bytes to i16 samples (little-endian).
pub fn bytes_to_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

/// Convert i16 samples to bytes (little-endian).
pub fn samples_to_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|&s| s.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bytes_to_samples_roundtrip() {
        let original: Vec<i16> = vec![0, 1000, -1000, 32767, -32768];
        let bytes = samples_to_bytes(&original);
        let decoded = bytes_to_samples(&bytes);
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_audio_level_silence() {
        let detector = AudioLevelDetector::new(0.01, 16000);
        let silence: Vec<i16> = vec![0, 0, 0, 0, 0];
        assert!(!detector.is_voice_active(&silence));
    }

    #[test]
    fn test_audio_level_loud() {
        let detector = AudioLevelDetector::new(0.01, 16000);
        let loud: Vec<i16> = vec![10000, 10000, 10000, 10000, 10000];
        assert!(detector.is_voice_active(&loud));
    }

    #[tokio::test]
    async fn test_mock_capture() {
        let config = AudioConfig::default();
        let mut capture = MockAudioCapture::new(config);

        capture.start().await.expect("start");
        assert!(capture.is_capturing().await);

        capture.stop().await.expect("stop");
        assert!(!capture.is_capturing().await);
    }
}
