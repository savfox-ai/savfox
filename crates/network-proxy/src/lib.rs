#![deny(clippy::print_stdout, clippy::print_stderr)]
#![allow(unreachable_pub)]

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
