use std::path::Path;

use anyhow::Result;
use app_test_support::{McpProcess, to_response};
use savfox_app_server_protocol::{
    JSONRPCResponse, RequestId, SetDefaultModelParams, SetDefaultModelResponse,
};
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_persists_overrides() -> Result<()> {
    let savfox_home = TempDir::new()?;
    create_config_toml(savfox_home.path())?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let params = SetDefaultModelParams {
        model: Some("gpt-4.1".to_string()),
        reasoning_effort: None,
    };

    let request_id = mcp.send_set_default_model_request(params).await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let _: SetDefaultModelResponse = to_response(resp)?;

    let config_path = savfox_home.path().join("config.toml");
    let config_contents = tokio::fs::read_to_string(&config_path).await?;
    assert!(
        config_contents.contains("slug = \"gpt-4.1\""),
        "updated model should be written to config.toml, got: {config_contents:?}"
    );
    assert!(
        config_contents.contains("provider = \"openai\""),
        "model provider should remain openai for unprefixed model ids, got: {config_contents:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_default_model_prefixed_updates_model_provider() -> Result<()> {
    let savfox_home = TempDir::new()?;
    std::fs::write(
        savfox_home.path().join("config.toml"),
        r#"
model = { slug = "gpt-5.1-savfox-max", provider = "openai" }
model_provider = "openai"
"#,
    )?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let params = SetDefaultModelParams {
        model: Some("zhipuai-coding-plan/glm-5".to_string()),
        reasoning_effort: None,
    };
    let request_id = mcp.send_set_default_model_request(params).await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: SetDefaultModelResponse = to_response(resp)?;

    let config_path = savfox_home.path().join("config.toml");
    let config_contents = tokio::fs::read_to_string(&config_path).await?;
    assert!(
        config_contents.contains("slug = \"glm-5\""),
        "updated model slug should be written to config.toml, got: {config_contents:?}"
    );
    assert!(
        config_contents.contains("provider = \"zhipuai-coding-plan\""),
        "model.provider should be updated to new provider, got: {config_contents:?}"
    );
    assert!(
        config_contents.contains("model_provider = \"zhipuai-coding-plan\""),
        "top-level model_provider should be updated to new provider, got: {config_contents:?}"
    );
    Ok(())
}

// Helper to create a config.toml; mirrors create_conversation.rs
fn create_config_toml(savfox_home: &Path) -> std::io::Result<()> {
    let config_toml = savfox_home.join("config.toml");
    std::fs::write(
        config_toml,
        r#"
model = { slug = "gpt-5.1-savfox-max", provider = "openai", reasoning_effort = "medium" }
"#,
    )
}
