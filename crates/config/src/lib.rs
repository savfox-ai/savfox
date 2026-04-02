//! Shared configuration types and utilities extracted from `savfox-core`.
//!
//! This crate houses leaf types and pure utility functions that do not depend
//! on the heavier core runtime, making them available to other crates without
//! pulling in the full core dependency.
#![allow(missing_debug_implementations)]
#![allow(clippy::manual_let_else, clippy::ptr_arg, clippy::ref_option)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod channel_store;
mod constraint;
pub mod fingerprint;
mod merge;
mod overrides;
mod requirement_source;
pub mod types;

pub use constraint::{Constrained, ConstraintError, ConstraintResult};
pub use merge::merge_toml_values;
pub use overrides::{build_cli_overrides_layer, default_empty_table};
pub use requirement_source::RequirementSource;

// ── Config file name constants ──────────────────────────────────────────────

pub const CONFIG_TOML_FILE: &str = "config.toml";
pub const CONFIG_JSON_FILE: &str = "config.json";
pub const CONFIG_YAML_FILE: &str = "config.yaml";
pub const CONFIG_YML_FILE: &str = "config.yml";
