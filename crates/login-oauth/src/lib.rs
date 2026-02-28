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
