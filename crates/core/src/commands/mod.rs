//! Command-execution and shell-surface domain.
//!
//! This groups shell dialect helpers, parsing, shell snapshots, and command
//! safety checks behind one namespace while preserving root-level
//! compatibility re-exports.

#[path = "../bash.rs"]
pub mod bash;
#[path = "../parse_command.rs"]
pub mod parse_command;
#[path = "../powershell.rs"]
pub mod powershell;
#[path = "../command_safety/mod.rs"]
pub mod safety;
#[path = "../shell.rs"]
pub mod shell;
#[path = "../shell_snapshot.rs"]
pub mod shell_snapshot;
