//! Prompt and project-context domain.
//!
//! This groups prompt customization, instruction assembly, project docs, and
//! personality migration behind one namespace while preserving root-level
//! compatibility re-exports.

#[path = "../custom_prompts.rs"]
pub mod custom_prompts;
#[path = "../instructions/mod.rs"]
pub mod instructions;
#[path = "../personality_migration.rs"]
pub mod personality_migration;
#[path = "../project_doc.rs"]
pub mod project_doc;
