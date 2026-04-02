use strum::IntoEnumIterator;
use strum_macros::{AsRefStr, EnumIter, EnumString, IntoStaticStr};

/// Commands that can be invoked by starting a message with a leading slash.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, EnumIter, AsRefStr, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum SlashCommand {
    // DO NOT ALPHA-SORT! Enum order is presentation order in the popup, so
    // more frequently used commands should be listed first.
    Model,
    Connect,
    Approvals,
    Permissions,
    #[strum(serialize = "setup-elevated-sandbox")]
    ElevateSandbox,
    Experimental,
    Skills,
    Review,
    Rename,
    New,
    Resume,
    Fork,
    Init,
    Compact,
    Plan,
    Collab,
    Agent,
    // Undo,
    Diff,
    Mention,
    Status,
    Mcp,
    Apps,
    Quit,
    Exit,
    Feedback,
    Rollout,
    Ps,
    Personality,
    TestApproval,
}

impl SlashCommand {
    /// User-visible description shown in the popup.
    pub fn description(self) -> &'static str {
        match self {
            Self::Feedback => "send logs to maintainers",
            Self::New => "start a new chat during a conversation",
            Self::Init => "create an AGENTS.md file with instructions for Savfox",
            Self::Compact => "summarize conversation to prevent hitting the context limit",
            Self::Review => "review my current changes and find issues",
            Self::Rename => "rename the current session",
            Self::Resume => "resume a saved chat",
            Self::Fork => "fork the current chat",
            // SlashCommand::Undo => "ask Savfox to undo a turn",
            Self::Quit | Self::Exit => "exit Savfox",
            Self::Diff => "show git diff (including untracked files)",
            Self::Mention => "mention a file",
            Self::Skills => "use skills to improve how Savfox performs specific tasks",
            Self::Status => "show current session configuration and token usage",
            Self::Ps => "list background terminals",
            Self::Model => "choose what model and reasoning effort to use",
            Self::Connect => "connect a model provider and import its models",
            Self::Personality => "choose a communication style for Savfox",
            Self::Plan => "switch to Plan mode",
            Self::Collab => "change collaboration mode (experimental)",
            Self::Agent => "switch the active agent session",
            Self::Approvals => "choose what Savfox can do without approval",
            Self::Permissions => "choose what Savfox is allowed to do",
            Self::ElevateSandbox => "set up elevated agent sandbox",
            Self::Experimental => "toggle experimental features",
            Self::Mcp => "list configured MCP tools",
            Self::Apps => "manage apps",
            Self::Rollout => "print the rollout file path",
            Self::TestApproval => "test approval request",
        }
    }

    /// Command string without the leading '/'. Provided for compatibility with
    /// existing code that expects a method named `command()`.
    pub fn command(self) -> &'static str {
        self.into()
    }

    /// Whether this command supports inline args (for example `/review ...`).
    pub fn supports_inline_args(self) -> bool {
        matches!(
            self,
            Self::Review | Self::Rename | Self::Plan
        )
    }

    /// Whether this command can be run while a task is in progress.
    pub fn available_during_task(self) -> bool {
        match self {
            Self::New
            | Self::Resume
            | Self::Fork
            | Self::Init
            | Self::Compact
            // | SlashCommand::Undo
            | Self::Model
            | Self::Connect
            | Self::Personality
            | Self::Approvals
            | Self::Permissions
            | Self::ElevateSandbox
            | Self::Experimental
            | Self::Review
            | Self::Plan => false,
            Self::Diff
            | Self::Rename
            | Self::Mention
            | Self::Skills
            | Self::Status
            | Self::Ps
            | Self::Mcp
            | Self::Apps
            | Self::Feedback
            | Self::Quit
            | Self::Exit => true,
            Self::Rollout => true,
            Self::TestApproval => true,
            Self::Collab => true,
            Self::Agent => true,
        }
    }

    fn is_visible(self) -> bool {
        match self {
            Self::Rollout | Self::TestApproval => cfg!(debug_assertions),
            _ => true,
        }
    }
}

/// Return all built-in commands in a Vec paired with their command string.
pub fn built_in_slash_commands() -> Vec<(&'static str, SlashCommand)> {
    SlashCommand::iter()
        .filter(|command| command.is_visible())
        .map(|c| (c.command(), c))
        .collect()
}
