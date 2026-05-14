//! Prompt and project-context domain.
//!
//! This groups prompt customization, instruction assembly, and project docs
//! behind one namespace while preserving root-level compatibility re-exports.

pub mod custom_prompts;
pub mod instructions;
pub mod project_doc;
