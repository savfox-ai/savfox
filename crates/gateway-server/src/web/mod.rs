//! Web-facing gateway domain.
//!
//! This groups static asset serving, webchat, the main HTTP server wiring,
//! WebSocket transport, and WS-RPC dispatch under one domain namespace while
//! keeping root-level compatibility re-exports for existing call sites.

#[allow(unused_imports)]
pub(crate) use crate::{server, static_assets, webchat};
pub use crate::{ws, ws_rpc};
