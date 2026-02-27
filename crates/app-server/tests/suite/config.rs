use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use app_test_support::{McpProcess, test_tmp_path, to_response};
use savfox_app_server_protocol::{
    GetUserSavedConfigResponse, JSONRPCResponse, Profile, RequestId, SandboxSettings, Tools,
    UserSavedConfig,
};
use savfox_core::protocol::AskForApproval;
use savfox_protocol::config_types::{ForcedLoginMethod, ReasoningSummary, SandboxMode, Verbosity};
use savfox_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn create_config_toml(savfox_home: &Path) -> std::io::Result<()> {
    let writable_root = test_tmp_path();
    let config_toml = savfox_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "gpt-5.1-savfox-max"
approval_policy = "on-request"
sandbox_mode = "workspace-write"
model_reasoning_summary = "detailed"
model_reasoning_effort = "high"
model_verbosity = "medium"
profile = "test"
forced_chatgpt_workspace_id = "12345678-0000-0000-0000-000000000000"
forced_login_method = "chatgpt"

[sandbox_workspace_write]
writable_roots = [{}]
network_access = true
exclude_tmpdir_env_var = true
exclude_slash_tmp = true

[tools]
web_search = false
view_image = true

[profiles.test]
model = "gpt-4o"
approval_policy = "on-request"
model_reasoning_effort = "high"
model_reasoning_summary = "detailed"
model_verbosity = "medium"
model_provider = "openai"
chatgpt_base_url = "https://api.taidge.com"
"#,
            serde_json::json!(writable_root)
        ),
    )
}

#[tokio::test(flavor = "multi_session", worker_sessions = 4)]
async fn get_config_toml_parses_all_fields() -> Result<()> {
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp.send_get_user_saved_config_request().await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let config: GetUserSavedConfigResponse = to_response(resp)?;
    let writable_root = test_tmp_path();
    let expected = GetUserSavedConfigResponse {
        config: UserSavedConfig {
            approval_policy: Some(AskForApproval::OnRequest),
            sandbox_mode: Some(SandboxMode::WorkspaceWrite),
            sandbox_settings: Some(SandboxSettings {
                writable_roots: vec![writable_root],
                network_access: Some(true),
                exclude_tmpdir_env_var: Some(true),
                exclude_slash_tmp: Some(true),
            }),
            forced_chatgpt_workspace_id: Some("12345678-0000-0000-0000-000000000000".into()),
            forced_login_method: Some(ForcedLoginMethod::Chatgpt),
            model: Some("gpt-5.1-savfox-max".into()),
            model_reasoning_effort: Some(ReasoningEffort::High),
            model_reasoning_summary: Some(ReasoningSummary::Detailed),
            model_verbosity: Some(Verbosity::Medium),
            tools: Some(Tools {
                web_search: Some(false),
                view_image: Some(true),
            }),
            profile: Some("test".to_string()),
            profiles: HashMap::from([(
                "test".into(),
                Profile {
                    model: Some("gpt-4o".into()),
                    approval_policy: Some(AskForApproval::OnRequest),
                    model_reasoning_effort: Some(ReasoningEffort::High),
                    model_reasoning_summary: Some(ReasoningSummary::Detailed),
                    model_verbosity: Some(Verbosity::Medium),
                    model_provider: Some("openai".into()),
                    chatgpt_base_url: Some("https://api.taidge.com".into()),
                },
            )]),
        },
    };

    assert_eq!(config, expected);
    Ok(())
}

#[tokio::test(flavor = "multi_session", worker_sessions = 2)]
async fn get_config_toml_empty() -> Result<()> {
    let savfox_home = TempDir::new()?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp.send_get_user_saved_config_request().await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let config: GetUserSavedConfigResponse = to_response(resp)?;
    let expected = GetUserSavedConfigResponse {
        config: UserSavedConfig {
            approval_policy: None,
            sandbox_mode: None,
            sandbox_settings: None,
            forced_chatgpt_workspace_id: None,
            forced_login_method: None,
            model: None,
            model_reasoning_effort: None,
            model_reasoning_summary: None,
            model_verbosity: None,
            tools: None,
            profile: None,
            profiles: HashMap::new(),
        },
    };

    assert_eq!(config, expected);
    Ok(())
}
