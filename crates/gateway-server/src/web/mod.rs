//! Web-facing gateway domain.
//!
//! This groups static asset serving, webchat, the main HTTP server wiring,
//! WebSocket transport, and WS-RPC dispatch under one domain namespace while
//! keeping root-level compatibility re-exports for existing call sites.

#[path = "../server.rs"]
pub(crate) mod server;
#[path = "../static_assets.rs"]
pub(crate) mod static_assets;
#[path = "../webchat.rs"]
pub(crate) mod webchat;
#[path = "../ws.rs"]
pub mod ws;
#[path = "../ws_rpc/mod.rs"]
pub mod ws_rpc;
