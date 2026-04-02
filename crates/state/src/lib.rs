//! SQLite-backed state for rollout metadata.
//!
//! This crate is intentionally small and focused: it extracts rollout metadata
//! from JSONL rollouts and mirrors it into a local SQLite database. Backfill
//! orchestration and rollout scanning live in `savfox-core`.
#![allow(missing_debug_implementations)]
#![allow(clippy::option_option)]

mod extract;
pub mod log_db;
mod migrations;
mod model;
mod paths;
mod runtime;

/// Low-level storage engine: useful for focused tests.
///
/// Most consumers should prefer [`StateRuntime`].
pub use extract::apply_rollout_item;
pub use model::{
    Anchor, BackfillStats, ExtractionOutcome, LogEntry, LogQuery, LogRow, SessionMetadata,
    SessionMetadataBuilder, SessionsPage, SortKey,
};
pub use runtime::STATE_DB_FILENAME;
/// Preferred entrypoint: owns configuration and metrics.
pub use runtime::StateRuntime;

/// Errors encountered during DB operations. Tags: [stage]
pub const DB_ERROR_METRIC: &str = "savfox.db.error";
/// Metrics on backfill process during first init of the db. Tags: [status]
pub const DB_METRIC_BACKFILL: &str = "savfox.db.backfill";
/// Metrics on backfill duration during first init of the db. Tags: [status]
pub const DB_METRIC_BACKFILL_DURATION_MS: &str = "savfox.db.backfill.duration_ms";
/// Metrics on errors during comparison between DB and rollout file. Tags: [stage]
pub const DB_METRIC_COMPARE_ERROR: &str = "savfox.db.compare_error";
