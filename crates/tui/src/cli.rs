use std::path::PathBuf;

use clap::{Parser, ValueHint};
use savfox_common::ApprovalModeCliArg;
use savfox_protocol::config_types::AltScreenMode;

fn parse_alt_screen_mode(value: &str) -> Result<AltScreenMode, String> {
    match value.to_ascii_lowercase().as_str() {
        "always" => Ok(AltScreenMode::Always),
        "auto" => Ok(AltScreenMode::Auto),
        "never" => Ok(AltScreenMode::Never),
        _ => Err(format!(
            "invalid alt-screen mode '{value}', expected one of: always, auto, never"
        )),
    }
}

#[derive(Parser, Debug)]
#[command(version)]
#[derive(Default)]
pub struct Cli {
    /// Optional user prompt to start the session.
    #[arg(value_name = "PROMPT", value_hint = clap::ValueHint::Other)]
    pub prompt: Option<String>,

    /// Optional image(s) to attach to the initial prompt.
    #[arg(long = "image", short = 'i', value_name = "FILE", value_delimiter = ',', num_args = 1..)]
    pub images: Vec<PathBuf>,

    // Internal controls set by the top-level `savfox resume` subcommand.
    // These are not exposed as user flags on the base `savfox` command.
    #[clap(skip)]
    pub resume_picker: bool,

    #[clap(skip)]
    pub resume_last: bool,

    /// Internal: resume a specific recorded session by id (UUID). Set by the
    /// top-level `savfox resume <SESSION_ID>` wrapper; not exposed as a public flag.
    #[clap(skip)]
    pub resume_session_id: Option<String>,

    /// Internal: show all sessions (disables cwd filtering and shows CWD column).
    #[clap(skip)]
    pub resume_show_all: bool,

    // Internal controls set by the top-level `savfox fork` subcommand.
    // These are not exposed as user flags on the base `savfox` command.
    #[clap(skip)]
    pub fork_picker: bool,

    #[clap(skip)]
    pub fork_last: bool,

    /// Internal: fork a specific recorded session by id (UUID). Set by the
    /// top-level `savfox fork <SESSION_ID>` wrapper; not exposed as a public flag.
    #[clap(skip)]
    pub fork_session_id: Option<String>,

    /// Internal: show all sessions (disables cwd filtering and shows CWD column).
    #[clap(skip)]
    pub fork_show_all: bool,

    /// Model the agent should use.
    #[arg(long, short = 'm')]
    pub model: Option<String>,

    /// Convenience flag to select the local open source model provider. Equivalent to -c
    /// model_provider=oss; verifies a local LM Studio or Ollama server is running.
    #[arg(long = "oss", default_value_t = false)]
    pub oss: bool,

    /// Specify which local provider to use (lmstudio, ollama, or ollama-chat).
    /// If not specified with --oss, will use config default or show selection.
    #[arg(long = "local-provider")]
    pub oss_provider: Option<String>,

    /// Select the sandbox policy to use when executing model-generated shell
    /// commands.
    #[arg(long = "sandbox", short = 's')]
    pub sandbox_mode: Option<savfox_common::SandboxModeCliArg>,

    /// Configure when the model requires human approval before executing a command.
    #[arg(long = "ask-for-approval", short = 'a')]
    pub approval_policy: Option<ApprovalModeCliArg>,

    /// Convenience alias for low-friction sandboxed automatic execution (-a on-request, --sandbox
    /// workspace-write).
    #[arg(long = "full-auto", default_value_t = false)]
    pub full_auto: bool,

    /// Skip all confirmation prompts and execute commands without sandboxing.
    /// EXTREMELY DANGEROUS. Intended solely for running in environments that are externally
    /// sandboxed.
    #[arg(
        long = "dangerously-bypass-approvals-and-sandbox",
        alias = "yolo",
        default_value_t = false,
        conflicts_with_all = ["approval_policy", "full_auto"]
    )]
    pub dangerously_bypass_approvals_and_sandbox: bool,

    /// Tell the agent to use the specified directory as its working root.
    #[clap(long = "cd", short = 'C', value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// Enable live web search. When enabled, the native Responses `web_search` tool is available
    /// to the model (no per‑call approval).
    #[arg(long = "search", default_value_t = false)]
    pub web_search: bool,

    /// Additional directories that should be writable alongside the primary workspace.
    #[arg(long = "add-dir", value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub add_dir: Vec<PathBuf>,

    /// Override alternate screen mode.
    ///
    /// - `auto`: detect terminal multiplexer and disable alternate screen only for Zellij.
    /// - `always`: always use alternate screen.
    /// - `never`: never use alternate screen (inline mode).
    ///
    /// When set, this overrides `tui.alternate_screen` from config.
    #[arg(long = "alt-screen", value_name = "MODE", value_parser = parse_alt_screen_mode)]
    pub alt_screen: Option<AltScreenMode>,
}


#[cfg(test)]
mod tests {
    use clap::Parser;
    use savfox_protocol::config_types::AltScreenMode;

    use super::Cli;

    #[test]
    fn parses_alt_screen_values() {
        let always = Cli::parse_from(["savfox", "--alt-screen", "always"]);
        assert_eq!(always.alt_screen, Some(AltScreenMode::Always));

        let auto = Cli::parse_from(["savfox", "--alt-screen", "auto"]);
        assert_eq!(auto.alt_screen, Some(AltScreenMode::Auto));

        let never = Cli::parse_from(["savfox", "--alt-screen", "never"]);
        assert_eq!(never.alt_screen, Some(AltScreenMode::Never));
    }

    #[test]
    fn rejects_invalid_alt_screen_value() {
        let err = Cli::try_parse_from(["savfox", "--alt-screen", "invalid"])
            .expect_err("expected invalid value");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }
}
