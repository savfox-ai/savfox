use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Mutex, broadcast};
use tracing::{debug, info, warn};

use super::audio_capture::{
    AudioCapture, AudioConfig, AudioFrame, MockAudioCapture, bytes_to_samples,
};
use super::wake_word::{WakeWordDetector, WakeWordEvent};

/// Combined detector that processes audio and detects wake words.
pub struct WakeWordDetectorService {
    detector: Arc<Mutex<WakeWordDetector>>,
    capture: Arc<Mutex<Box<dyn AudioCapture>>>,
    event_tx: broadcast::Sender<WakeWordEvent>,
    running: Arc<Mutex<bool>>,
}

impl WakeWordDetectorService {
    pub fn new(keyword: &str, sensitivity: f32) -> Self {
        let (event_tx, _) = broadcast::channel(64);
        let config = AudioConfig::default();

        Self {
            detector: Arc::new(Mutex::new(WakeWordDetector::new(keyword, sensitivity))),
            capture: Arc::new(Mutex::new(Box::new(MockAudioCapture::new(config)))),
            event_tx,
            running: Arc::new(Mutex::new(false)),
        }
    }

    /// Create with a custom audio capture backend.
    pub fn with_capture<C: AudioCapture + 'static>(
        keyword: &str,
        sensitivity: f32,
        capture: C,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(64);

        Self {
            detector: Arc::new(Mutex::new(WakeWordDetector::new(keyword, sensitivity))),
            capture: Arc::new(Mutex::new(Box::new(capture))),
            event_tx,
            running: Arc::new(Mutex::new(false)),
        }
    }

    /// Subscribe to wake word events.
    pub fn subscribe(&self) -> broadcast::Receiver<WakeWordEvent> {
        self.event_tx.subscribe()
    }

    /// Start listening for wake words.
    pub async fn start(&self) -> Result<()> {
        let mut running = self.running.lock().await;
        if *running {
            return Ok(());
        }
        *running = true;

        let _ = self.event_tx.send(WakeWordEvent::Started);
        info!("Wake word detector service started");

        // Start audio capture
        {
            let mut capture = self.capture.lock().await;
            capture.start().await?;
        }

        // Spawn the detection loop
        let running_clone = self.running.clone();
        let detector_clone = self.detector.clone();
        let event_tx_clone = self.event_tx.clone();
        let capture_clone = self.capture.clone();

        tokio::spawn(async move {
            let mut audio_rx = {
                let capture = capture_clone.lock().await;
                capture.subscribe()
            };

            while *running_clone.lock().await {
                tokio::select! {
                    result = audio_rx.recv() => {
                        match result {
                            Ok(frame) => {
                                // Process audio frame for wake word detection
                                if let Some(event) = Self::process_frame(
                                    &frame,
                                    &detector_clone,
                                ).await {
                                    let _ = event_tx_clone.send(event);
                                }
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                warn!("Audio channel closed");
                                break;
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                warn!("Audio processing lagged by {} frames", n);
                            }
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                        // Periodic check for shutdown
                    }
                }
            }

            // Stop audio capture
            let mut capture = capture_clone.lock().await;
            let _ = capture.stop().await;

            let _ = event_tx_clone.send(WakeWordEvent::Stopped);
            info!("Wake word detector service stopped");
        });

        Ok(())
    }

    /// Stop listening for wake words.
    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        *running = false;
    }

    /// Check if the service is running.
    pub async fn is_running(&self) -> bool {
        *self.running.lock().await
    }

    /// Update the wake word keyword.
    pub async fn set_keyword(&self, keyword: &str) {
        let mut detector = self.detector.lock().await;
        detector.set_keyword(keyword);
        info!("Wake word updated to: {}", keyword);
    }

    /// Update the sensitivity level.
    pub async fn set_sensitivity(&self, sensitivity: f32) {
        let mut detector = self.detector.lock().await;
        detector.set_sensitivity(sensitivity);
        debug!("Sensitivity updated to: {}", sensitivity);
    }

    /// Process an audio frame and check for wake word.
    async fn process_frame(
        frame: &AudioFrame,
        detector: &Arc<Mutex<WakeWordDetector>>,
    ) -> Option<WakeWordEvent> {
        // This is a placeholder for actual audio processing.
        // In a real implementation, we would:
        // 1. Convert audio to the format expected by the STT engine
        // 2. Send to STT for transcription
        // 3. Check the transcription for the wake word

        // For now, we just add the audio to the detector's buffer
        {
            let detector = detector.lock().await;
            detector.add_audio(&frame.data).await;
        }

        None
    }

    /// Process transcribed text and emit wake word event if detected.
    pub async fn process_transcription(&self, text: &str) -> Option<WakeWordEvent> {
        let detector = self.detector.lock().await;

        if let Some(keyword) = detector.check_text(text) {
            let event = WakeWordEvent::Detected {
                keyword,
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            let _ = self.event_tx.send(event.clone());
            Some(event)
        } else {
            None
        }
    }
}

/// Builder for configuring the wake word detector service.
pub struct WakeWordDetectorBuilder {
    keyword: String,
    sensitivity: f32,
}

impl WakeWordDetectorBuilder {
    pub fn new() -> Self {
        Self {
            keyword: "hey savfox".to_string(),
            sensitivity: 0.5,
        }
    }

    pub fn keyword(mut self, keyword: &str) -> Self {
        self.keyword = keyword.to_string();
        self
    }

    pub fn sensitivity(mut self, sensitivity: f32) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    pub fn build(self) -> WakeWordDetectorService {
        WakeWordDetectorService::new(&self.keyword, self.sensitivity)
    }
}

impl Default for WakeWordDetectorBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_detector_service_lifecycle() {
        let service = WakeWordDetectorService::new("hey savfox", 0.5);

        assert!(!service.is_running().await);

        service.start().await.expect("start");
        assert!(service.is_running().await);

        service.stop().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!service.is_running().await);
    }

    #[tokio::test]
    async fn test_process_transcription() {
        let service = WakeWordDetectorService::new("hey savfox", 0.9);

        let event = service.process_transcription("hey savfox").await;
        assert!(matches!(event, Some(WakeWordEvent::Detected { .. })));

        let no_event = service.process_transcription("hello world").await;
        assert!(no_event.is_none());
    }

    #[tokio::test]
    async fn test_builder() {
        let service = WakeWordDetectorBuilder::new()
            .keyword("hello world")
            .sensitivity(0.8)
            .build();

        // Service should be configured with the specified values
        let _detector = service.detector.lock().await;
    }
}
