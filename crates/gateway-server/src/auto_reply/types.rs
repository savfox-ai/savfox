use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Group activation mode - controls when the bot responds in group chats.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GroupActivation {
    /// Only respond when explicitly mentioned (@bot).
    #[default]
    Mention,
    /// Respond when message contains configured keyword(s) or bot is mentioned.
    Keyword,
    /// Respond to all messages in the group.
    Always,
    /// Only respond to recognized commands in groups.
    Command,
    /// Do not respond in groups.
    Off,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoReplyConfig {
    pub enabled: bool,
    pub greeting_message: Option<String>,
    pub status_format: Option<String>,
    pub allow_commands: bool,
    pub command_prefix: String,
    #[serde(default)]
    pub command_allowlist: Vec<String>,
    #[serde(default)]
    pub command_denylist: Vec<String>,
    /// Keyword list used when `group_activation = "keyword"`.
    #[serde(default)]
    pub group_keywords: Vec<String>,
    /// Per-channel override for group activation mode.
    /// Supports exact channel id (e.g. `discord:123`) or platform key
    /// (e.g. `discord`, `telegram`, `slack`).
    #[serde(default)]
    pub group_activation_overrides: HashMap<String, GroupActivation>,
    /// Group activation mode (default: mention-only).
    #[serde(default)]
    pub group_activation: GroupActivation,
}

impl Default for AutoReplyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            greeting_message: Some(
                "Hello! I'm ready to help. Type /help for commands.".to_owned(),
            ),
            status_format: None,
            allow_commands: true,
            command_prefix: "/".to_owned(),
            command_allowlist: Vec::new(),
            command_denylist: Vec::new(),
            group_keywords: Vec::new(),
            group_activation_overrides: HashMap::new(),
            group_activation: GroupActivation::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommandContext {
    pub sender_id: String,
    pub channel_id: String,
    pub session_id: Option<String>,
    pub is_authorized: bool,
    pub is_mentioned: bool,
    pub is_group: bool,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub reply: Option<String>,
    pub action: Option<CommandAction>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandAction {
    Reset,
    /// Clear conversation history on the current agent thread (keeps settings).
    Clear,
    Compact {
        turns: Option<u32>,
    },
    Stop,
    NewSession,
    SetModel {
        model: String,
    },
    SetVerbose {
        enabled: bool,
    },
    SetThinking {
        level: String,
    },
    ShowHelp,
    ShowStatus,
}

#[derive(Debug, Clone)]
pub struct AutoReplyResult {
    pub reply: Option<String>,
    pub is_command: bool,
    pub action: Option<CommandAction>,
}
