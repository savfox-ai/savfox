use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::info;

/// A single turn in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub id: String,
    pub role: TurnRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub audio_url: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TurnRole {
    User,
    Assistant,
    System,
}

/// State of the current conversation.
#[derive(Debug, Clone)]
pub struct ConversationState {
    pub session_id: String,
    pub turns: Vec<ConversationTurn>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub turn_count: usize,
}

impl ConversationState {
    #[must_use]
    pub fn new(session_id: &str) -> Self {
        let now = Utc::now();
        Self {
            session_id: session_id.to_owned(),
            turns: Vec::new(),
            created_at: now,
            last_activity: now,
            turn_count: 0,
        }
    }

    pub fn add_turn(&mut self, turn: ConversationTurn) {
        self.turns.push(turn);
        self.turn_count += 1;
        self.last_activity = Utc::now();
    }

    #[must_use]
    pub fn last_turn(&self) -> Option<&ConversationTurn> {
        self.turns.last()
    }

    #[must_use]
    pub fn last_user_turn(&self) -> Option<&ConversationTurn> {
        self.turns.iter().rev().find(|t| t.role == TurnRole::User)
    }

    #[must_use]
    pub fn last_assistant_turn(&self) -> Option<&ConversationTurn> {
        self.turns
            .iter()
            .rev()
            .find(|t| t.role == TurnRole::Assistant)
    }

    pub fn clear(&mut self) {
        self.turns.clear();
        self.turn_count = 0;
        self.last_activity = Utc::now();
    }

    /// Get conversation as a list of messages for API requests.
    #[must_use]
    pub fn to_messages(&self) -> Vec<serde_json::Value> {
        self.turns
            .iter()
            .map(|turn| {
                serde_json::json!({
                    "role": match turn.role {
                        TurnRole::User => "user",
                        TurnRole::Assistant => "assistant",
                        TurnRole::System => "system",
                    },
                    "content": turn.content,
                })
            })
            .collect()
    }
}

/// Manager for active conversations.
pub struct ConversationManager {
    conversations: RwLock<Vec<Arc<RwLock<ConversationState>>>>,
    max_conversations: usize,
}

impl ConversationManager {
    #[must_use]
    pub fn new(max_conversations: usize) -> Self {
        Self {
            conversations: RwLock::new(Vec::new()),
            max_conversations,
        }
    }

    pub async fn create_conversation(&self, session_id: &str) -> Arc<RwLock<ConversationState>> {
        let conversation = Arc::new(RwLock::new(ConversationState::new(session_id)));

        let mut conversations = self.conversations.write().await;

        // Remove oldest if at capacity
        if conversations.len() >= self.max_conversations {
            conversations.remove(0);
        }

        conversations.push(conversation.clone());
        info!(session_id = %session_id, "Created new conversation");
        conversation
    }

    pub async fn get_conversation(
        &self,
        session_id: &str,
    ) -> Option<Arc<RwLock<ConversationState>>> {
        let conversations = self.conversations.read().await;
        conversations
            .iter()
            .find(|c| {
                if let Ok(state) = c.try_read() {
                    state.session_id == session_id
                } else {
                    false
                }
            })
            .cloned()
    }

    pub async fn remove_conversation(&self, session_id: &str) -> bool {
        let mut conversations = self.conversations.write().await;
        let initial_len = conversations.len();
        conversations.retain(|c| {
            if let Ok(state) = c.try_read() {
                state.session_id != session_id
            } else {
                true
            }
        });
        let removed = conversations.len() < initial_len;
        if removed {
            info!(session_id = %session_id, "Removed conversation");
        }
        removed
    }

    pub async fn active_count(&self) -> usize {
        self.conversations.read().await.len()
    }

    pub async fn clear_all(&self) {
        self.conversations.write().await.clear();
        info!("Cleared all conversations");
    }
}

impl Default for ConversationManager {
    fn default() -> Self {
        Self::new(100)
    }
}

/// Builder for creating conversation turns.
pub struct ConversationTurnBuilder {
    role: TurnRole,
    content: String,
    audio_url: Option<String>,
    duration_ms: Option<u64>,
}

impl ConversationTurnBuilder {
    #[must_use]
    pub fn new(role: TurnRole, content: &str) -> Self {
        Self {
            role,
            content: content.to_owned(),
            audio_url: None,
            duration_ms: None,
        }
    }

    #[must_use]
    pub fn user(content: &str) -> Self {
        Self::new(TurnRole::User, content)
    }

    #[must_use]
    pub fn assistant(content: &str) -> Self {
        Self::new(TurnRole::Assistant, content)
    }

    #[must_use]
    pub fn system(content: &str) -> Self {
        Self::new(TurnRole::System, content)
    }

    #[must_use]
    pub fn audio_url(mut self, url: &str) -> Self {
        self.audio_url = Some(url.to_owned());
        self
    }

    #[must_use]
    pub fn duration_ms(mut self, ms: u64) -> Self {
        self.duration_ms = Some(ms);
        self
    }

    #[must_use]
    pub fn build(self) -> ConversationTurn {
        ConversationTurn {
            id: uuid::Uuid::now_v7().to_string(),
            role: self.role,
            content: self.content,
            timestamp: chrono::Utc::now(),
            audio_url: self.audio_url,
            duration_ms: self.duration_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversation_state() {
        let mut state = ConversationState::new("test-session");
        assert_eq!(state.turn_count, 0);

        let turn = ConversationTurnBuilder::user("Hello")
            .duration_ms(1500)
            .build();
        state.add_turn(turn);

        assert_eq!(state.turn_count, 1);
        assert!(state.last_user_turn().is_some());
    }

    #[tokio::test]
    async fn test_conversation_manager() {
        let manager = ConversationManager::new(10);

        let conv = manager
            .create_conversation("0194f7b3-1d7b-7c40-ae3d-95b6ef93e172")
            .await;
        assert_eq!(manager.active_count().await, 1);

        {
            let mut state = conv.write().await;
            state.add_turn(ConversationTurnBuilder::user("Hi").build());
        }

        let retrieved = manager
            .get_conversation("0194f7b3-1d7b-7c40-ae3d-95b6ef93e172")
            .await;
        assert!(retrieved.is_some());

        manager
            .remove_conversation("0194f7b3-1d7b-7c40-ae3d-95b6ef93e172")
            .await;
        assert_eq!(manager.active_count().await, 0);
    }
}
