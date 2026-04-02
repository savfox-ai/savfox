#![allow(unreachable_pub)]
#![allow(clippy::future_not_send, clippy::manual_let_else)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg(feature = "cli")]
mod approval_mode_cli_arg;

#[cfg(feature = "elapsed")]
pub mod elapsed;

#[cfg(feature = "cli")]
pub use approval_mode_cli_arg::ApprovalModeCliArg;

#[cfg(feature = "cli")]
mod sandbox_mode_cli_arg;

#[cfg(feature = "cli")]
pub use sandbox_mode_cli_arg::SandboxModeCliArg;

#[cfg(feature = "cli")]
pub mod format_env_display;

mod sandbox_summary;

#[cfg(feature = "sandbox_summary")]
pub use sandbox_summary::summarize_sandbox_policy;

mod config_summary;

pub use config_summary::create_config_summary_entries;
// Shared fuzzy matcher (used by TUI selection popups and other UI filtering)
pub mod fuzzy_match;
// Unified permission presets (sandbox + approval + tool access) for TUI and Gateway.
pub mod permission_presets;
// Shared OSS provider utilities used by TUI and exec
pub mod oss;
