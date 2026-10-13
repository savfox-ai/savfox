#![allow(clippy::unwrap_used)]

use core_test_support::test_savfox::test_savfox;
use core_test_support::{load_sse_fixture_with_id, responses, skip_if_no_network};
use savfox_core::features::Feature;
use savfox_protocol::config_types::WebSearchMode;
use savfox_protocol::openai_models::{
    ApplyPatchToolType, ConfigShellToolType, ModelInfo, ModelVisibility, ModelsResponse,
};
use wiremock::MockServer;

fn sse_completed(id: &str) -> String {
    load_sse_fixture_with_id("../fixtures/completed_template.json", id)
}

#[allow(clippy::expect_used)]
fn tool_identifiers(body: &serde_json::Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| {
            tool.get("name")
                .and_then(|v| v.as_str())
                .or_else(|| tool.get("type").and_then(|v| v.as_str()))
                .map(str::to_owned)
                .expect("tool should have either name or type")
        })
        .collect()
}

#[allow(clippy::expect_used)]
async fn collect_tool_identifiers_for_model(model_info: ModelInfo) -> Vec<String> {
    let server = MockServer::start().await;
    let model = model_info.slug.clone();
    responses::mount_models_once(
        &server,
        ModelsResponse {
            models: vec![model_info],
        },
    )
    .await;
    let sse = sse_completed(&model);
    let resp_mock = responses::mount_sse_once(&server, sse).await;

    let mut builder = test_savfox()
        .with_model(&model)
        // Keep tool expectations stable when the default web_search mode changes.
        .with_config(|config| {
            config.web_search_mode = Some(WebSearchMode::Cached);
            config.features.enable(Feature::CollaborationModes);
        });
    let test = builder
        .build(&server)
        .await
        .expect("create test Savfox conversation");

    test.submit_turn("hello tools").await.expect("submit turn");

    let body = resp_mock.single_request().body_json();
    tool_identifiers(&body)
}

fn catalog_model(slug: &str, shell_type: ConfigShellToolType) -> ModelInfo {
    ModelInfo {
        visibility: ModelVisibility::List,
        shell_type,
        ..ModelInfo::new(slug, slug)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_selects_expected_tools() {
    skip_if_no_network!();
    use pretty_assertions::assert_eq;

    let default_shell_tools = collect_tool_identifiers_for_model(catalog_model(
        "catalog-default-shell",
        ConfigShellToolType::Default,
    ))
    .await;
    assert_eq!(
        default_shell_tools,
        vec![
            "shell".to_owned(),
            "list_mcp_resources".to_owned(),
            "list_mcp_resource_templates".to_owned(),
            "read_mcp_resource".to_owned(),
            "update_plan".to_owned(),
            "request_user_input".to_owned(),
            "web_search".to_owned(),
            "view_image".to_owned()
        ],
        "catalog metadata should expose the default shell tool",
    );

    let mut shell_command_model =
        catalog_model("catalog-shell-command", ConfigShellToolType::ShellCommand);
    shell_command_model.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    let shell_command_tools = collect_tool_identifiers_for_model(shell_command_model).await;
    assert_eq!(
        shell_command_tools,
        vec![
            "shell_command".to_owned(),
            "list_mcp_resources".to_owned(),
            "list_mcp_resource_templates".to_owned(),
            "read_mcp_resource".to_owned(),
            "update_plan".to_owned(),
            "request_user_input".to_owned(),
            "apply_patch".to_owned(),
            "web_search".to_owned(),
            "view_image".to_owned()
        ],
        "catalog metadata should expose shell_command and apply_patch",
    );

    let mut unified_exec_model =
        catalog_model("catalog-unified-exec", ConfigShellToolType::UnifiedExec);
    unified_exec_model.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    let unified_exec_tools = collect_tool_identifiers_for_model(unified_exec_model).await;
    assert_eq!(
        unified_exec_tools,
        vec![
            "exec_command".to_owned(),
            "write_stdin".to_owned(),
            "list_mcp_resources".to_owned(),
            "list_mcp_resource_templates".to_owned(),
            "read_mcp_resource".to_owned(),
            "update_plan".to_owned(),
            "request_user_input".to_owned(),
            "apply_patch".to_owned(),
            "web_search".to_owned(),
            "view_image".to_owned()
        ],
        "catalog metadata should expose unified exec and apply_patch",
    );
}
