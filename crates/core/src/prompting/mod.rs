//! Prompt and project-context domain.
//!
//! This groups prompt customization, instruction assembly, project docs, and
//! personality migration behind one namespace while preserving root-level
//! compatibility re-exports.

pub mod custom_prompts;
pub mod instructions;
pub mod personality_migration;
pub mod project_doc;
