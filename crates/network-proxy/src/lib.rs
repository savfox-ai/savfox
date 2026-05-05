//! HTTP / SOCKS5 proxy with allow / deny network-policy enforcement.
//!
//! This crate provides the proxy listener that Savfox interposes between
//! agent-spawned subprocesses and the public network. Every outbound
//! connection is classified through a [`NetworkPolicyDecider`] before it
//! is permitted, and denied connections are returned to the client with a
//! structured reason rather than a generic refusal.
//!
//! Two protocols are supported:
//!
//! * **HTTP CONNECT** — for `https://` / TLS tunnelling. Implemented in
//!   the `http_proxy` module; the upstream-tunnel headers are validated
//!   before being forwarded.
//! * **SOCKS5** — for opaque TCP. Implemented in the `proxy` module.
//!
//! The policy layer (`policy`, `network_policy`, `responses`) decides
//! per-host / per-port allow/deny outcomes. Configuration is loaded via
//! [`config`] and may be reloaded at runtime through the `admin` module.
//!
//! # Threat model
//!
//! The proxy is a **defence-in-depth** layer, not the sole boundary —
//! sandbox policies (Seatbelt, Landlock, Windows AppContainer) still
//! restrict the spawning subprocess. The proxy ensures that outbound
//! traffic which slips past those layers is at least classified and
//! audit-logged.

#![deny(clippy::print_stdout, clippy::print_stderr)]
#![allow(unreachable_pub)]
#![allow(missing_debug_implementations)]
#![allow(clippy::manual_let_else, clippy::return_self_not_must_use)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod admin;
mod config;
mod http_proxy;
mod network_policy;
mod policy;
mod proxy;
mod reasons;
mod responses;
mod runtime;
mod socks5;
mod state;
mod upstream;

use anyhow::Result;
pub use network_policy::{
    NetworkDecision, NetworkPolicyDecider, NetworkPolicyRequest, NetworkPolicyRequestArgs,
    NetworkProtocol,
};
pub use proxy::{Args, NetworkProxy, NetworkProxyBuilder, NetworkProxyHandle};

pub async fn run_main(args: Args) -> Result<()> {
    let _ = args;
    let proxy = NetworkProxy::builder().build().await?;
    proxy.run().await?.wait().await
}
