#![allow(clippy::unwrap_used)]

use core_test_support::responses::start_mock_server;
use core_test_support::test_savfox::test_savfox;
use core_test_support::{load_sse_fixture_with_id, responses, skip_if_no_network};
use savfox_core::features::Feature;
use savfox_protocol::config_types::WebSearchMode;

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
async fn collect_tool_identifiers_for_model(model: &str) -> Vec<String> {
    let server = start_mock_server().await;
    let sse = sse_completed(model);
    let resp_mock = responses::mount_sse_once(&server, sse).await;

    let mut builder = test_savfox()
        .with_model(model)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_selects_expected_tools() {
    skip_if_no_network!();
    use pretty_assertions::assert_eq;

    let savfox_tools = collect_tool_identifiers_for_model("savfox-mini-latest").await;
    assert_eq!(
        savfox_tools,
        vec![
            "local_shell".to_owned(),
            "list_mcp_resources".to_owned(),
            "list_mcp_resource_templates".to_owned(),
            "read_mcp_resource".to_owned(),
            "update_plan".to_owned(),
            "request_user_input".to_owned(),
            "web_search".to_owned(),
            "view_image".to_owned()
        ],
        "savfox-mini-latest should expose the local shell tool",
    );

    let gpt5_savfox_tools = collect_tool_identifiers_for_model("gpt-5-savfox").await;
    assert_eq!(
        gpt5_savfox_tools,
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
        "gpt-5-savfox should expose the apply_patch tool",
    );

    let gpt51_savfox_tools = collect_tool_identifiers_for_model("gpt-5.1-savfox").await;
    assert_eq!(
        gpt51_savfox_tools,
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
        "gpt-5.1-savfox should expose the apply_patch tool",
    );

    let gpt5_tools = collect_tool_identifiers_for_model("gpt-5").await;
    assert_eq!(
        gpt5_tools,
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
        "gpt-5 should expose the apply_patch tool",
    );

    let gpt51_tools = collect_tool_identifiers_for_model("gpt-5.1").await;
    assert_eq!(
        gpt51_tools,
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
        "gpt-5.1 should expose the apply_patch tool",
    );
    let exp_tools = collect_tool_identifiers_for_model("exp-5.1").await;
    assert_eq!(
        exp_tools,
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
        "exp-5.1 should expose the apply_patch tool",
    );
}
