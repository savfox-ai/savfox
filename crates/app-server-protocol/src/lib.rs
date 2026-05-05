//! Wire protocol for the Savfox app-server (JSON-RPC v1).
//!
//! This crate is the canonical schema source for the JSON-RPC frames that
//! flow between the Savfox app-server (host process) and its embedders
//! — TUI, IDE plugins, the gateway-server, and external SDKs. All
//! request, notification, and response shapes are defined in
//! [`protocol::v1`] and intentionally use `#[serde(deny_unknown_fields)]`
//! so an unknown field at the wire is treated as a hard error rather than
//! silently dropped.
//!
//! The crate also ships TypeScript / JSON-Schema **codegen** entry points
//! ([`generate_ts`], [`generate_json`]) so the same definitions can be
//! emitted for the web UI without hand-maintaining a parallel schema. See
//! the `export` module's per-function documentation for usage examples.
//!
//! # Relationship to `savfox-protocol`
//!
//! `savfox-protocol` describes the *agent ↔ engine* protocol (token-stream
//! events, tool calls, sandbox policies). `savfox-app-server-protocol`
//! describes the *engine ↔ embedder* protocol that wraps it. Some types
//! (e.g. `AskForApproval`, `SandboxMode`) appear in both because the
//! embedder needs to mirror agent-level concepts on its own wire surface.

#![allow(unreachable_pub)]
#![allow(missing_debug_implementations)]
#![allow(clippy::unused_self)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod experimental_api;
mod export;
mod jsonrpc_lite;
mod protocol;
mod schema_fixtures;

pub use experimental_api::*;
pub use export::{
    GenerateTsOptions, generate_json, generate_json_with_experimental, generate_ts,
    generate_ts_with_options, generate_types,
};
pub use jsonrpc_lite::*;
pub use protocol::common::*;
pub use protocol::session_history::*;
pub use protocol::v1::*;
pub use schema_fixtures::{
    SchemaFixtureOptions, read_schema_fixture_tree, write_schema_fixtures,
    write_schema_fixtures_with_options,
};
