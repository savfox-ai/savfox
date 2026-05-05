//! OAuth 2.0 / PKCE login flows for Savfox.
//!
//! Two browser-based authorization code flows are exposed:
//!
//! * **Local-loopback PKCE** — [`server`] starts an ephemeral HTTP listener
//!   on a free port, opens the user's browser to the provider's
//!   authorization endpoint, and completes the PKCE exchange when the
//!   browser is redirected back to `http://127.0.0.1:<port>/callback`.
//!   The verifier is generated per-flow and never written to disk.
//! * **Device code grant** — [`request_device_code`] +
//!   [`complete_device_code_login`] for headless / TUI environments where
//!   the user enters a code on a separate browser device.
//!
//! Token persistence (refresh token, ID token claims) is delegated to
//! `savfox-core::auth`, whose helpers are re-exported below for backward
//! compatibility.

#![allow(unreachable_pub)]

mod device_code_auth;
mod pkce;
mod server;

pub use device_code_auth::{
    DeviceCode, complete_device_code_login, request_device_code, run_device_code_login,
};
// Re-export commonly used auth types and helpers from savfox-core for compatibility
pub use savfox_app_server_protocol::AuthMode;
pub use savfox_core::auth::{
    AuthDotJson, CLIENT_ID, OPENAI_API_KEY_ENV_VAR, SAVFOX_API_KEY_ENV_VAR, login_with_api_key,
    logout, save_auth,
};
pub use savfox_core::token_data::TokenData;
pub use savfox_core::{AuthManager, SavfoxAuth};
pub use server::{LoginServer, ServerOptions, ShutdownHandle, run_login_server};
