use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{debug, info, warn};

use super::conversation::{ConversationManager, ConversationState, ConversationTurnBuilder};
use super::turn_detection::{TurnDetectionConfig, TurnDetector, TurnEvent};
use super::{TalkModeConfig, TalkModeState};

/// Talk mode service that manages continuous conversation.
pub struct TalkModeService {
    state: Arc<RwLock<TalkModeState>>,
    config: Arc<RwLock<TalkModeConfig>>,
    conversation_manager: Arc<ConversationManager>,
    turn_detector: Arc<Mutex<TurnDetector>>,
    event_tx: broadcast::Sender<TalkModeEvent>,
    current_session: Arc<RwLock<Option<String>>>,
}

/// Events emitted by the talk mode service.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum TalkModeEvent {
    StateChanged {
        from: String,
        to: String,
    },
    TurnStarted {
        session_id: String,
    },
    TurnEnded {
        session_id: String,
        duration_ms: u64,
    },
    UserTranscribed {
        session_id: String,
        text: String,
    },
    AssistantResponse {
        session_id: String,
        text: String,
    },
    Error {
        message: String,
    },
}

impl TalkModeService {
    pub fn new() -> Self {
        let config = TalkModeConfig::default();
        let turn_config = TurnDetectionConfig {
            silence_threshold_ms: config.silence_threshold_ms,
            max_turn_duration_ms: config.max_turn_duration_ms,
            ..Default::default()
        };

        let (event_tx, _) = broadcast::channel(64);

        Self {
            state: Arc::new(RwLock::new(TalkModeState::Inactive)),
            config: Arc::new(RwLock::new(config)),
            conversation_manager: Arc::new(ConversationManager::new(100)),
            turn_detector: Arc::new(Mutex::new(TurnDetector::new(turn_config))),
            event_tx,
            current_session: Arc::new(RwLock::new(None)),
        }
    }

    pub fn with_config(config: TalkModeConfig) -> Self {
        let turn_config = TurnDetectionConfig {
            silence_threshold_ms: config.silence_threshold_ms,
            max_turn_duration_ms: config.max_turn_duration_ms,
            ..Default::default()
        };

        let (event_tx, _) = broadcast::channel(64);

        Self {
            state: Arc::new(RwLock::new(TalkModeState::Inactive)),
            config: Arc::new(RwLock::new(config)),
            conversation_manager: Arc::new(ConversationManager::new(100)),
            turn_detector: Arc::new(Mutex::new(TurnDetector::new(turn_config))),
            event_tx,
            current_session: Arc::new(RwLock::new(None)),
        }
    }

    /// Subscribe to talk mode events.
    pub fn subscribe(&self) -> broadcast::Receiver<TalkModeEvent> {
        self.event_tx.subscribe()
    }

    /// Start a talk mode session.
    pub async fn start_session(&self, session_id: &str) -> Result<()> {
        let mut current = self.current_session.write().await;
        if current.is_some() {
            return Err(anyhow::anyhow!("Talk mode session already active"));
        }

        *current = Some(session_id.to_string());
        self.conversation_manager
            .create_conversation(session_id)
            .await;
        self.set_state(TalkModeState::Listening).await;

        info!(session_id = %session_id, "Talk mode session started");
        Ok(())
    }

    /// End the current talk mode session.
    pub async fn end_session(&self) {
        let session_id = {
            let mut current = self.current_session.write().await;
            current.take()
        };

        if let Some(session_id) = session_id {
            self.conversation_manager
                .remove_conversation(&session_id)
                .await;
            info!(session_id = %session_id, "Talk mode session ended");
        }

        self.set_state(TalkModeState::Inactive).await;
    }

    /// Get the current state.
    pub async fn state(&self) -> TalkModeState {
        self.state.read().await.clone()
    }

    /// Get the current session ID.
    pub async fn session_id(&self) -> Option<String> {
        self.current_session.read().await.clone()
    }

    /// Pause the talk mode.
    pub async fn pause(&self) {
        self.set_state(TalkModeState::Paused).await;
        self.turn_detector.lock().await.reset();
        info!("Talk mode paused");
    }

    /// Resume the talk mode.
    pub async fn resume(&self) {
        if let Some(session_id) = self.session_id().await {
            self.set_state(TalkModeState::Listening).await;
            info!(session_id = %session_id, "Talk mode resumed");
        }
    }

    /// Process user transcription (from STT).
    pub async fn process_user_text(&self, text: &str) -> Result<()> {
        let session_id = self
            .session_id()
            .await
            .ok_or_else(|| anyhow::anyhow!("No active talk mode session"))?;

        // Add to conversation
        if let Some(conv) = self
            .conversation_manager
            .get_conversation(&session_id)
            .await
        {
            let turn = ConversationTurnBuilder::user(text).build();
            conv.write().await.add_turn(turn);
        }

        // Emit event
        let _ = self.event_tx.send(TalkModeEvent::UserTranscribed {
            session_id: session_id.clone(),
            text: text.to_string(),
        });

        // Transition to processing
        self.set_state(TalkModeState::Processing).await;

        info!(session_id = %session_id, text = %text, "User text received");
        Ok(())
    }

    /// Process assistant response.
    pub async fn process_assistant_text(&self, text: &str) -> Result<()> {
        let session_id = self
            .session_id()
            .await
            .ok_or_else(|| anyhow::anyhow!("No active talk mode session"))?;

        // Add to conversation
        if let Some(conv) = self
            .conversation_manager
            .get_conversation(&session_id)
            .await
        {
            let turn = ConversationTurnBuilder::assistant(text).build();
            conv.write().await.add_turn(turn);
        }

        // Emit event
        let _ = self.event_tx.send(TalkModeEvent::AssistantResponse {
            session_id: session_id.clone(),
            text: text.to_string(),
        });

        // Transition to speaking or listening based on config
        let config = self.config.read().await;
        if config.auto_tts {
            self.set_state(TalkModeState::Speaking).await;
        } else {
            self.set_state(TalkModeState::Listening).await;
        }

        Ok(())
    }

    /// Mark assistant speech as complete.
    pub async fn speech_complete(&self) {
        self.set_state(TalkModeState::Listening).await;
    }

    /// Process audio level for VAD.
    pub async fn process_audio_level(&self, level: f32, timestamp_ms: u64) {
        let state = self.state.read().await.clone();
        if state != TalkModeState::Listening {
            return;
        }

        let mut detector = self.turn_detector.lock().await;
        let turn_ended = detector.process_level(level, timestamp_ms);

        if turn_ended {
            if let Some(session_id) = self.session_id().await {
                let _ = self.event_tx.send(TalkModeEvent::TurnEnded {
                    session_id,
                    duration_ms: 0,
                });
            }
        }
    }

    /// Interrupt current turn.
    pub async fn interrupt(&self) {
        self.turn_detector.lock().await.interrupt();
        self.set_state(TalkModeState::Listening).await;
    }

    /// Update configuration.
    pub async fn update_config(&self, config: TalkModeConfig) {
        *self.config.write().await = config;
    }

    /// Get conversation for current session.
    pub async fn get_conversation(&self) -> Option<Arc<RwLock<ConversationState>>> {
        if let Some(session_id) = self.session_id().await {
            self.conversation_manager
                .get_conversation(&session_id)
                .await
        } else {
            None
        }
    }

    async fn set_state(&self, new_state: TalkModeState) {
        let mut state = self.state.write().await;
        if *state != new_state {
            let from = format!("{:?}", *state);
            *state = new_state.clone();
            let to = format!("{:?}", new_state);
            debug!("Talk mode state changed: {} -> {}", from, to);
            let _ = self.event_tx.send(TalkModeEvent::StateChanged { from, to });
        }
    }
}

impl Default for TalkModeService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_talk_mode_lifecycle() {
        let service = TalkModeService::new();

        assert_eq!(service.state().await, TalkModeState::Inactive);

        service.start_session("test-session").await.expect("start");
        assert_eq!(service.state().await, TalkModeState::Listening);

        service.pause().await;
        assert_eq!(service.state().await, TalkModeState::Paused);

        service.resume().await;
        assert_eq!(service.state().await, TalkModeState::Listening);

        service.end_session().await;
        assert_eq!(service.state().await, TalkModeState::Inactive);
    }

    #[tokio::test]
    async fn test_process_user_text() {
        let service = TalkModeService::new();
        service.start_session("test").await.expect("start");

        service.process_user_text("Hello").await.expect("process");
        assert_eq!(service.state().await, TalkModeState::Processing);

        service.end_session().await;
    }

    #[tokio::test]
    async fn test_conversation_turns() {
        let service = TalkModeService::new();
        service.start_session("test").await.expect("start");

        service.process_user_text("Hi there").await.expect("user");
        service
            .process_assistant_text("Hello! How can I help?")
            .await
            .expect("assistant");

        let conv = service.get_conversation().await.expect("conversation");
        let state = conv.read().await;
        assert_eq!(state.turn_count, 2);

        service.end_session().await;
    }
}
