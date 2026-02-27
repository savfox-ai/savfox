pub(crate) mod discord;
pub(crate) mod feishu;
pub(crate) mod googlechat;
pub(crate) mod imessage;
pub(crate) mod irc;
pub(crate) mod line;
pub(crate) mod matrix;
pub(crate) mod mattermost;
pub(crate) mod msteams;
pub(crate) mod nextcloud;
pub(crate) mod nostr;
pub(crate) mod runtime;
pub(crate) mod signal;
pub(crate) mod slack;
pub(crate) mod telegram;
pub(crate) mod tlon;
pub(crate) mod twitch;
pub(crate) mod webhook;
pub(crate) mod whatsapp;
pub(crate) mod zalo;

use async_trait::async_trait;
use serde_json::Value;

use crate::protocol::BridgeAction;

/// Trait for chat platform bridge adapters.
///
/// Each bridge connects to an external chat platform (Discord, Telegram, Slack, etc.)
/// and translates between the platform's message format and Savfox gateway operations.
#[async_trait]
pub(crate) trait ChatBridge: Send + Sync {
    /// Initialize the bridge (connect to platform API, register commands, etc.).
    async fn start(&mut self) -> anyhow::Result<()>;

    /// Send a plain text message to a chat channel.
    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()>;

    /// Send a structured/rich message to a chat channel (code blocks, embeds, etc.).
    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()>;

    /// Handle an incoming webhook payload from the platform.
    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction>;
}

/// Structured message for rich chat platform formatting.
#[derive(Debug, Clone)]
pub(crate) struct RichMessage {
    /// Main text content.
    pub(crate) text: String,
    /// Optional code blocks with language tags.
    pub(crate) code_blocks: Vec<CodeBlock>,
    /// Optional title/header.
    pub(crate) title: Option<String>,
    /// Color accent (platform-specific rendering).
    pub(crate) color: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CodeBlock {
    pub(crate) language: String,
    pub(crate) content: String,
}
