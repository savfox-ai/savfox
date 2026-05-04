//! Gateway runtime coordination domain.
//!
//! This groups session, routing, channel orchestration, and related runtime
//! helpers under one namespace while preserving root-level compatibility
//! re-exports.

pub(crate) mod agent_routing;
pub use crate::{channel, identity_links, message_queue, pairing_store};
pub mod routing;
pub mod session;
