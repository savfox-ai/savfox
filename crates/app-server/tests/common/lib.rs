mod auth_fixtures;
mod config;
mod mcp_process;
mod mock_model_server;
mod models_cache;
mod responses;
mod rollout;

pub use auth_fixtures::{
    ChatGptAuthFixture, ChatGptIdTokenClaims, encode_id_token, write_chatgpt_auth,
};
pub use config::write_mock_responses_config_toml;
pub use core_test_support::{
    format_with_current_shell, format_with_current_shell_display,
    format_with_current_shell_display_non_login, format_with_current_shell_non_login,
    test_path_buf_with_windows, test_tmp_path, test_tmp_path_buf,
};
pub use mcp_process::{DEFAULT_CLIENT_NAME, McpProcess};
pub use mock_model_server::{
    create_mock_responses_server_repeating_assistant, create_mock_responses_server_sequence,
    create_mock_responses_server_sequence_unchecked,
};
pub use models_cache::{test_catalog, write_models_cache, write_models_cache_with_models};
pub use responses::{
    create_apply_patch_sse_response, create_exec_command_sse_response,
    create_final_assistant_message_sse_response, create_request_user_input_sse_response,
    create_shell_command_sse_response,
};
pub use rollout::{
    create_fake_rollout, create_fake_rollout_with_source, create_fake_rollout_with_text_elements,
    rollout_path,
};
use savfox_app_server_protocol::JSONRPCResponse;
use serde::de::DeserializeOwned;

pub fn to_response<T: DeserializeOwned>(response: JSONRPCResponse) -> anyhow::Result<T> {
    let value = serde_json::to_value(response.result)?;
    let savfox_response = serde_json::from_value(value)?;
    Ok(savfox_response)
}
