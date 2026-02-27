//! Subagent orchestration: registry, lifecycle, and context guards.
//!
//! This module provides infrastructure for spawning, tracking, and managing
//! child agent instances. A subagent is an autonomous agent invocation spawned
//! by a parent agent to handle a delegated task.

mod context_guard;
mod registry;

pub use context_guard::{ContextGuard, ContextGuardConfig, ContextPressure};
pub use registry::{SubagentEntry, SubagentRegistry, SubagentStatus};
