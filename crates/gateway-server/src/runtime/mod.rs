//! Gateway runtime coordination domain.
//!
//! This groups session, routing, channel orchestration, and related runtime
//! helpers under one namespace while preserving root-level compatibility
//! re-exports.

#[path = "../agent_routing.rs"]
pub(crate) mod agent_routing;
pub use crate::channel;
pub use crate::identity_links;
pub use crate::message_queue;
pub use crate::pairing_store;
#[path = "../routing/mod.rs"]
pub mod routing;
#[path = "../session/mod.rs"]
pub mod session;
