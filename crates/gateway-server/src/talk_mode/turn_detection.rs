use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::info;

/// Turn detection event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TurnEvent {
    Start {
        timestamp_ms: u64,
    },
    End {
        timestamp_ms: u64,
        duration_ms: u64,
        audio_bytes: usize,
    },
    Silence {
        duration_ms: u64,
    },
    Interrupt,
}

/// Configuration for turn detection.
#[derive(Debug, Clone)]
pub struct TurnDetectionConfig {
    /// Minimum silence duration to consider turn ended (ms).
    pub silence_threshold_ms: u64,
    /// Maximum turn duration before forced end (ms).
    pub max_turn_duration_ms: u64,
    /// Minimum audio level to consider as speech.
    pub speech_threshold: f32,
    /// Number of frames of speech before turn starts.
    pub speech_start_frames: usize,
    /// Number of silent frames before turn ends.
    pub silence_end_frames: usize,
}

impl Default for TurnDetectionConfig {
    fn default() -> Self {
        Self {
            silence_threshold_ms: 1000,
            max_turn_duration_ms: 30000,
            speech_threshold: 0.02,
            speech_start_frames: 3,
            silence_end_frames: 15, // ~300ms at 50ms frames
        }
    }
}

/// State machine for turn detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectionState {
    Idle,
    SpeechDetected,
    InTurn,
    Ending,
}

/// Turn detector using Voice Activity Detection (VAD).
pub struct TurnDetector {
    config: TurnDetectionConfig,
    state: DetectionState,
    consecutive_speech_frames: usize,
    consecutive_silence_frames: usize,
    turn_start_ms: Option<u64>,
    event_tx: broadcast::Sender<TurnEvent>,
    frame_count: usize,
}

impl TurnDetector {
    pub fn new(config: TurnDetectionConfig) -> Self {
        let (event_tx, _) = broadcast::channel(32);
        Self {
            config,
            state: DetectionState::Idle,
            consecutive_speech_frames: 0,
            consecutive_silence_frames: 0,
            turn_start_ms: None,
            event_tx,
            frame_count: 0,
        }
    }

    /// Subscribe to turn events.
    pub fn subscribe(&self) -> broadcast::Receiver<TurnEvent> {
        self.event_tx.subscribe()
    }

    /// Process an audio level sample.
    /// Returns true if a turn ended.
    pub fn process_level(&mut self, level: f32, timestamp_ms: u64) -> bool {
        self.frame_count += 1;
        let is_speech = level >= self.config.speech_threshold;

        let turn_ended = match self.state {
            DetectionState::Idle => {
                if is_speech {
                    self.consecutive_speech_frames += 1;
                    if self.consecutive_speech_frames >= self.config.speech_start_frames {
                        self.start_turn(timestamp_ms);
                    }
                } else {
                    self.consecutive_speech_frames = 0;
                }
                false
            }
            DetectionState::SpeechDetected | DetectionState::InTurn => {
                if is_speech {
                    self.consecutive_silence_frames = 0;
                    self.state = DetectionState::InTurn;
                    false
                } else {
                    self.consecutive_silence_frames += 1;

                    // Check for max duration
                    if let Some(start_ms) = self.turn_start_ms {
                        if timestamp_ms - start_ms >= self.config.max_turn_duration_ms {
                            return self.end_turn(timestamp_ms, 0);
                        }
                    }

                    // Check for silence threshold
                    if self.consecutive_silence_frames >= self.config.silence_end_frames {
                        self.end_turn(timestamp_ms, 0)
                    } else {
                        // Send silence event for progress
                        let silence_duration = self.consecutive_silence_frames as u64 * 20; // Assume 20ms frames
                        if silence_duration % 200 == 0 {
                            let _ = self.event_tx.send(TurnEvent::Silence {
                                duration_ms: silence_duration,
                            });
                        }
                        false
                    }
                }
            }
            DetectionState::Ending => false,
        };

        turn_ended
    }

    fn start_turn(&mut self, timestamp_ms: u64) {
        self.state = DetectionState::InTurn;
        self.turn_start_ms = Some(timestamp_ms);
        self.consecutive_silence_frames = 0;

        info!("Turn started at {}ms", timestamp_ms);
        let _ = self.event_tx.send(TurnEvent::Start { timestamp_ms });
    }

    fn end_turn(&mut self, timestamp_ms: u64, audio_bytes: usize) -> bool {
        let duration_ms = self
            .turn_start_ms
            .map(|start| timestamp_ms.saturating_sub(start))
            .unwrap_or(0);

        info!("Turn ended: duration={}ms", duration_ms);
        let _ = self.event_tx.send(TurnEvent::End {
            timestamp_ms,
            duration_ms,
            audio_bytes,
        });

        self.state = DetectionState::Idle;
        self.turn_start_ms = None;
        self.consecutive_speech_frames = 0;
        self.consecutive_silence_frames = 0;

        true
    }

    /// Interrupt the current turn.
    pub fn interrupt(&mut self) {
        if self.state == DetectionState::InTurn {
            info!("Turn interrupted");
            let _ = self.event_tx.send(TurnEvent::Interrupt);
            self.state = DetectionState::Idle;
            self.turn_start_ms = None;
            self.consecutive_speech_frames = 0;
            self.consecutive_silence_frames = 0;
        }
    }

    /// Reset the detector state.
    pub fn reset(&mut self) {
        self.state = DetectionState::Idle;
        self.turn_start_ms = None;
        self.consecutive_speech_frames = 0;
        self.consecutive_silence_frames = 0;
        self.frame_count = 0;
    }

    /// Check if currently in a turn.
    pub fn is_in_turn(&self) -> bool {
        matches!(
            self.state,
            DetectionState::InTurn | DetectionState::SpeechDetected
        )
    }

    /// Get current state.
    pub fn state(&self) -> &DetectionState {
        &self.state
    }

    /// Update configuration.
    pub fn set_config(&mut self, config: TurnDetectionConfig) {
        self.config = config;
    }
}

/// Calculate audio level (RMS) from samples.
pub fn calculate_audio_level(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_squares: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();

    let rms = (sum_squares / samples.len() as f64).sqrt();

    // Normalize to 0.0 - 1.0
    (rms / 32768.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_audio_level() {
        // Silence
        let silence = vec![0i16; 1000];
        assert!(calculate_audio_level(&silence) < 0.01);

        // Loud audio
        let loud: Vec<i16> = (0..1000).map(|i| ((i % 100) * 300) as i16).collect();
        assert!(calculate_audio_level(&loud) > 0.1);
    }

    #[test]
    fn test_turn_detector_start() {
        let config = TurnDetectionConfig {
            speech_start_frames: 2,
            silence_end_frames: 5,
            ..Default::default()
        };
        let mut detector = TurnDetector::new(config);

        // Speech frames
        assert!(!detector.process_level(0.5, 0)); // frame 1
        assert!(!detector.process_level(0.5, 50)); // frame 2 - should start turn
        assert!(detector.is_in_turn());
    }

    #[test]
    fn test_turn_detector_end() {
        let config = TurnDetectionConfig {
            speech_start_frames: 1,
            silence_end_frames: 2,
            ..Default::default()
        };
        let mut detector = TurnDetector::new(config);

        // Start turn
        detector.process_level(0.5, 0);
        assert!(detector.is_in_turn());

        // Silence frames
        assert!(!detector.process_level(0.0, 50));
        let ended = detector.process_level(0.0, 100); // Should end turn
        assert!(ended);
        assert!(!detector.is_in_turn());
    }
}
