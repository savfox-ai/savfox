//! Shared data types for the Savfox gateway server and its web frontend.
//!
//! This crate contains serde-based types that are used for JSON-RPC
//! communication between the backend (native) and frontend (wasm32).
//! It has no platform-specific dependencies.

mod agents;
mod approvals;
mod channels;
mod chat;
mod config;
mod cron;
mod logs;
mod memory;
mod models;
mod nodes;
mod protocol;
mod sessions;
mod skills;
mod tts;
mod usage;
mod voice;
mod wizard;

pub use agents::*;
pub use approvals::*;
pub use channels::*;
pub use chat::*;
pub use config::*;
pub use cron::*;
pub use logs::*;
pub use memory::*;
pub use models::*;
pub use nodes::*;
pub use protocol::*;
pub use sessions::*;
pub use skills::*;
pub use tts::*;
pub use usage::*;
pub use voice::*;
pub use wizard::*;
