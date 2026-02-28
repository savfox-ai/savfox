#![allow(unreachable_pub)]

mod auth_status;
mod logging_client_handler;
mod oauth;
mod perform_oauth_login;
mod program_resolver;
mod rmcp_client;
mod utils;

pub use auth_status::{determine_streamable_http_auth_status, supports_oauth_login};
pub(crate) use oauth::load_oauth_tokens;
pub use oauth::{
    OAuthCredentialsStoreMode, StoredOAuthTokens, WrappedOAuthTokenResponse, delete_oauth_tokens,
    save_oauth_tokens,
};
pub use perform_oauth_login::{
    OauthLoginHandle, perform_oauth_login, perform_oauth_login_return_url,
};
pub use rmcp::model::ElicitationAction;
pub use rmcp_client::{
    Elicitation, ElicitationResponse, ListToolsWithConnectorIdResult, RmcpClient, SendElicitation,
    ToolWithConnectorId,
};
pub use savfox_protocol::protocol::McpAuthStatus;
