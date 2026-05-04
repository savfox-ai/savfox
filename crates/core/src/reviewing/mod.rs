//! Review and diff-presentation domain.
//!
//! This groups review formatting, review prompts, transcript policy, and
//! related diff-tracking helpers behind one namespace while preserving
//! root-level compatibility re-exports.

pub mod review_format;
pub mod review_prompts;
pub use crate::transcript_policy;
pub mod turn_diff_tracker;
