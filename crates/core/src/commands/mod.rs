//! Command-execution and shell-surface domain.
//!
//! This groups shell dialect helpers, parsing, shell snapshots, and command
//! safety checks behind one namespace while preserving root-level
//! compatibility re-exports.

pub mod bash;
pub mod parse_command;
pub mod powershell;
pub mod safety;
pub mod shell;
pub mod shell_snapshot;
