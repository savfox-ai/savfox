//! Review and diff-presentation domain.
//!
//! This groups review formatting, review prompts, transcript policy, and
//! related diff-tracking helpers behind one namespace while preserving
//! root-level compatibility re-exports.

#[path = "../review_format.rs"]
pub mod review_format;
#[path = "../review_prompts.rs"]
pub mod review_prompts;
pub use crate::transcript_policy;
#[path = "../turn_diff_tracker.rs"]
pub mod turn_diff_tracker;
