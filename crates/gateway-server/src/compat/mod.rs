//! OpenClaw protocol compatibility layer.
//!
//! Allows OpenClaw clients (iOS/Android/macOS native apps, web UI) to
//! connect to an Savfox gateway by translating between the OpenClaw
//! frame format and Savfox's native WS-RPC format.

pub(crate) mod frame;
pub(crate) mod translator;
