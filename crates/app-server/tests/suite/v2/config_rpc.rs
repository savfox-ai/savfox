use anyhow::Result;
use app_test_support::{McpProcess, test_tmp_path_buf, to_response};
use pretty_assertions::assert_eq;
use savfox_app_server_protocol::{
    AskForApproval, ConfigBatchWriteParams, ConfigEdit, ConfigLayerSource, ConfigReadParams,
    ConfigReadResponse, ConfigValueWriteParams, ConfigWriteResponse, JSONRPCError, JSONRPCResponse,
    MergeStrategy, RequestId, SandboxMode, ToolsV2, WriteStatus,
};
use savfox_core::config::set_project_trust_level;
use savfox_core::config_loader::SYSTEM_CONFIG_TOML_FILE_UNIX;
use savfox_protocol::config_types::TrustLevel;
use savfox_protocol::openai_models::ReasoningEffort;
use savfox_utils::absolute_path::AbsolutePathBuf;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn write_config(savfox_home: &TempDir, contents: &str) -> Result<()> {
    Ok(std::fs::write(
        savfox_home.path().join("config.toml"),
        contents,
    )?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_read_returns_effective_and_layers() -> Result<()> {
    let savfox_home = TempDir::new()?;
    write_config(
        &savfox_home,
        r#"
sandbox_mode = "workspace-write"
"#,
    )?;
    let savfox_home_path = savfox_home.path().canonicalize()?;
    let user_file = AbsolutePathBuf::try_from(savfox_home_path.join("config.toml"))?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_config_read_request(ConfigReadParams {
            include_layers: true,
            cwd: None,
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let ConfigReadResponse {
        config,
        origins,
        layers,
    } = to_response(resp)?;

    assert_eq!(config.sandbox_mode, Some(SandboxMode::WorkspaceWrite),);
    assert_eq!(
        origins.get("sandbox_mode").expect("origin").name,
        ConfigLayerSource::User {
            file: user_file.clone(),
        }
    );
    let layers = layers.expect("layers present");
    assert_layers_user_then_optional_system(&layers, user_file)?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_read_includes_tools() -> Result<()> {
    let savfox_home = TempDir::new()?;
    write_config(
        &savfox_home,
        r#"
[tools]
web_search = true
view_image = false
"#,
    )?;
    let savfox_home_path = savfox_home.path().canonicalize()?;
    let user_file = AbsolutePathBuf::try_from(savfox_home_path.join("config.toml"))?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_config_read_request(ConfigReadParams {
            include_layers: true,
            cwd: None,
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let ConfigReadResponse {
        config,
        origins,
        layers,
    } = to_response(resp)?;

    let tools = config.tools.expect("tools present");
    assert_eq!(
        tools,
        ToolsV2 {
            web_search: Some(true),
            view_image: Some(false),
        }
    );
    assert_eq!(
        origins.get("tools.web_search").expect("origin").name,
        ConfigLayerSource::User {
            file: user_file.clone(),
        }
    );
    assert_eq!(
        origins.get("tools.view_image").expect("origin").name,
        ConfigLayerSource::User {
            file: user_file.clone(),
        }
    );

    let layers = layers.expect("layers present");
    assert_layers_user_then_optional_system(&layers, user_file)?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_read_includes_project_layers_for_cwd() -> Result<()> {
    let savfox_home = TempDir::new()?;
    write_config(&savfox_home, "")?;

    let workspace = TempDir::new()?;
    let project_config_dir = workspace.path().join(".savfox");
    std::fs::create_dir_all(&project_config_dir)?;
    std::fs::write(
        project_config_dir.join("config.toml"),
        r#"
model_reasoning_effort = "high"
"#,
    )?;
    set_project_trust_level(savfox_home.path(), workspace.path(), TrustLevel::Trusted)?;
    let project_config = AbsolutePathBuf::try_from(project_config_dir)?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_config_read_request(ConfigReadParams {
            include_layers: true,
            cwd: Some(workspace.path().to_string_lossy().into_owned()),
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let ConfigReadResponse {
        config, origins, ..
    } = to_response(resp)?;

    assert_eq!(config.model_reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(
        origins.get("model_reasoning_effort").expect("origin").name,
        ConfigLayerSource::Project {
            dot_savfox_folder: project_config
        }
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_value_write_replaces_value() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let savfox_home = temp_dir.path().canonicalize()?;
    write_config(
        &temp_dir,
        r#"
approval_policy = "on-request"
"#,
    )?;

    let mut mcp = McpProcess::new(&savfox_home).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let read_id = mcp
        .send_config_read_request(ConfigReadParams {
            include_layers: false,
            cwd: None,
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let read: ConfigReadResponse = to_response(read_resp)?;
    let expected_version = read
        .origins
        .get("approval_policy")
        .map(|m| m.version.clone());

    let write_id = mcp
        .send_config_value_write_request(ConfigValueWriteParams {
            file_path: None,
            key_path: "approval_policy".to_owned(),
            value: json!("never"),
            merge_strategy: MergeStrategy::Replace,
            expected_version,
        })
        .await?;
    let write_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(write_id)),
    )
    .await??;
    let write: ConfigWriteResponse = to_response(write_resp)?;
    let expected_file_path =
        AbsolutePathBuf::resolve_path_against_base("config.toml", savfox_home)?;

    assert_eq!(write.status, WriteStatus::Ok);
    assert_eq!(write.file_path, expected_file_path);
    assert!(write.overridden_metadata.is_none());

    let verify_id = mcp
        .send_config_read_request(ConfigReadParams {
            include_layers: false,
            cwd: None,
        })
        .await?;
    let verify_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(verify_id)),
    )
    .await??;
    let verify: ConfigReadResponse = to_response(verify_resp)?;
    assert_eq!(verify.config.approval_policy, Some(AskForApproval::Never));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_value_write_rejects_version_conflict() -> Result<()> {
    let savfox_home = TempDir::new()?;
    write_config(
        &savfox_home,
        r#"
[model]
slug = "gpt-old"
provider = "openai"
"#,
    )?;

    let mut mcp = McpProcess::new(savfox_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let write_id = mcp
        .send_config_value_write_request(ConfigValueWriteParams {
            file_path: Some(savfox_home.path().join("config.toml").display().to_string()),
            key_path: "model".to_owned(),
            value: json!("gpt-new"),
            merge_strategy: MergeStrategy::Replace,
            expected_version: Some("sha256:stale".to_owned()),
        })
        .await?;

    let err: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(write_id)),
    )
    .await??;
    let code = err
        .error
        .data
        .as_ref()
        .and_then(|d| d.get("config_write_error_code"))
        .and_then(|v| v.as_str());
    assert_eq!(code, Some("configVersionConflict"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_batch_write_applies_multiple_edits() -> Result<()> {
    let tmp_dir = TempDir::new()?;
    let savfox_home = tmp_dir.path().canonicalize()?;
    write_config(&tmp_dir, "")?;

    let mut mcp = McpProcess::new(&savfox_home).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let writable_root = test_tmp_path_buf();
    let batch_id = mcp
        .send_config_batch_write_request(ConfigBatchWriteParams {
            file_path: Some(savfox_home.join("config.toml").display().to_string()),
            edits: vec![
                ConfigEdit {
                    key_path: "sandbox_mode".to_owned(),
                    value: json!("workspace-write"),
                    merge_strategy: MergeStrategy::Replace,
                },
                ConfigEdit {
                    key_path: "sandbox_workspace_write".to_owned(),
                    value: json!({
                        "writable_roots": [writable_root.clone()],
                        "network_access": false
                    }),
                    merge_strategy: MergeStrategy::Replace,
                },
            ],
            expected_version: None,
        })
        .await?;
    let batch_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(batch_id)),
    )
    .await??;
    let batch_write: ConfigWriteResponse = to_response(batch_resp)?;
    assert_eq!(batch_write.status, WriteStatus::Ok);
    let expected_file_path =
        AbsolutePathBuf::resolve_path_against_base("config.toml", savfox_home)?;
    assert_eq!(batch_write.file_path, expected_file_path);

    let read_id = mcp
        .send_config_read_request(ConfigReadParams {
            include_layers: false,
            cwd: None,
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let read: ConfigReadResponse = to_response(read_resp)?;
    assert_eq!(read.config.sandbox_mode, Some(SandboxMode::WorkspaceWrite));
    let sandbox = read
        .config
        .sandbox_workspace_write
        .as_ref()
        .expect("sandbox workspace write");
    assert_eq!(sandbox.writable_roots, vec![writable_root]);
    assert!(!sandbox.network_access);

    Ok(())
}

fn assert_layers_user_then_optional_system(
    layers: &[savfox_app_server_protocol::ConfigLayer],
    user_file: AbsolutePathBuf,
) -> Result<()> {
    // `config/read` returns layers in HighestPrecedenceFirst order, so on
    // Unix the system layer (if present) comes before the user layer.
    if cfg!(unix) {
        let system_file = AbsolutePathBuf::from_absolute_path(SYSTEM_CONFIG_TOML_FILE_UNIX)?;
        assert_eq!(layers.len(), 2);
        assert_eq!(
            layers[0].name,
            ConfigLayerSource::System { file: system_file }
        );
        assert_eq!(layers[1].name, ConfigLayerSource::User { file: user_file });
    } else {
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].name, ConfigLayerSource::User { file: user_file });
    }
    Ok(())
}
