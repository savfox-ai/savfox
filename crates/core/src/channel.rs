use async_trait::async_trait;
use serde_json::Value;

/// Actions emitted by a channel implementation from inbound webhook or event payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelAction {
    /// Start a new agent thread with the given prompt.
    StartThread { channel: String, prompt: String },
    /// Send a message to an existing thread.
    SendToThread { thread_id: String, message: String },
    /// Respond to an approval request.
    Approve { thread_id: String, decision: bool },
    /// No action needed for the inbound payload.
    Ignore,
}

/// Structured message for rich chat platform formatting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichMessage {
    /// Main text content.
    pub text: String,
    /// Optional code blocks with language tags.
    pub code_blocks: Vec<CodeBlock>,
    /// Optional title/header.
    pub title: Option<String>,
    /// Color accent (platform-specific rendering).
    pub color: Option<String>,
}

/// Code block content attached to a rich message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeBlock {
    pub language: String,
    pub content: String,
}

impl RichMessage {
    /// Format as plain text with fenced code blocks appended.
    #[must_use]
    pub fn to_plain_text(&self) -> String {
        let mut text = self.text.clone();
        for block in &self.code_blocks {
            text.push_str(&format!("\n```{}\n{}\n```", block.language, block.content));
        }
        text
    }
}

/// Trait for chat platform channel adapters.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Initialize the channel (connect to platform API, register commands, etc.).
    async fn start(&mut self) -> anyhow::Result<()>;

    /// Send a plain text message to a chat channel.
    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()>;

    /// Send a structured/rich message to a chat channel (code blocks, embeds, etc.).
    ///
    /// Default: formats as plain text with code blocks and delegates to `send_message`.
    /// Override for platform-specific rich formatting (embeds, cards, HTML, etc.).
    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        self.send_message(channel, &msg.to_plain_text()).await
    }

    /// Handle an incoming webhook payload from the platform.
    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<ChannelAction>;
}

pub const CHANNEL_NAMES: &[&str] = &[
    "discord",
    "dingtalk",
    "feishu",
    "googlechat",
    "imessage",
    "irc",
    "line",
    "arkret",
    "matrix",
    "mattermost",
    "msteams",
    "nextcloud",
    "nostr",
    "signal",
    "slack",
    "telegram",
    "tlon",
    "twitch",
    "webhook",
    "whatsapp",
    "zalo",
];
