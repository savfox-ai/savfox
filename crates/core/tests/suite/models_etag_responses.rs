#![cfg(not(target_os = "windows"))]

use std::sync::Arc;

use anyhow::Result;
use core_test_support::responses::{
    ev_assistant_message, ev_completed, ev_local_shell_call, ev_response_created, sse, sse_response,
};
use core_test_support::test_savfox::test_savfox;
use core_test_support::{responses, skip_if_no_network, wait_for_event};
use pretty_assertions::assert_eq;
use savfox_core::SavfoxAuth;
use savfox_core::features::Feature;
use savfox_core::protocol::{AskForApproval, EventMsg, Op, SandboxPolicy};
use savfox_protocol::config_types::ReasoningSummary;
use savfox_protocol::openai_models::ModelsResponse;
use savfox_protocol::user_input::UserInput;
use wiremock::MockServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_models_on_models_etag_mismatch_and_avoid_duplicate_models_fetch() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const ETAG_1: &str = "\"models-etag-1\"";
    const ETAG_2: &str = "\"models-etag-2\"";
    const CALL_ID: &str = "local-shell-call-1";

    let server = MockServer::start().await;

    // 1) On spawn, Savfox fetches /models and stores the ETag.
    let spawn_models_mock = responses::mount_models_once_with_etag(
        &server,
        ModelsResponse { models: Vec::new() },
        ETAG_1,
    )
    .await;

    let auth = SavfoxAuth::create_dummy_chatgpt_auth_for_testing();
    let mut builder = test_savfox()
        .with_auth(auth)
        .with_model("gpt-5")
        .with_config(|config| {
            config.features.enable(Feature::RemoteModels);
            // Keep this test deterministic: no request retries, and a small stream retry budget.
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(1);
        });

    let test = builder.build(&server).await?;
    let savfox = Arc::clone(&test.savfox);
    let cwd = Arc::clone(&test.cwd);
    let session_model = test.session_configured.model.clone();

    assert_eq!(spawn_models_mock.requests().len(), 1);
    assert_eq!(spawn_models_mock.single_request_path(), "/v1/models");

    // 2) If the server sends a different X-Models-Etag on /responses, Savfox refreshes /models.
    let refresh_models_mock = responses::mount_models_once_with_etag(
        &server,
        ModelsResponse { models: Vec::new() },
        ETAG_2,
    )
    .await;

    // First /responses request (user message) succeeds and returns a tool call.
    // It also includes a mismatched X-Models-Etag, which should trigger a /models refresh.
    let first_response_body = sse(vec![
        ev_response_created("resp-1"),
        ev_local_shell_call(CALL_ID, "completed", vec!["/bin/echo", "etag ok"]),
        ev_completed("resp-1"),
    ]);
    responses::mount_response_once(
        &server,
        sse_response(first_response_body).insert_header("X-Models-Etag", ETAG_2),
    )
    .await;

    // Second /responses request (tool output) includes the same X-Models-Etag; Savfox should not
    // refetch /models again after it has already refreshed the catalog.
    let completion_response_body = sse(vec![
        ev_response_created("resp-2"),
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-2"),
    ]);
    let tool_output_mock = responses::mount_response_once(
        &server,
        sse_response(completion_response_body).insert_header("X-Models-Etag", ETAG_2),
    )
    .await;

    savfox
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please run a tool".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    let _ = wait_for_event(&savfox, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Assert /models was refreshed exactly once after the X-Models-Etag mismatch.
    assert_eq!(refresh_models_mock.requests().len(), 1);
    assert_eq!(refresh_models_mock.single_request_path(), "/v1/models");
    let refresh_req = refresh_models_mock
        .requests()
        .into_iter()
        .next()
        .expect("one request");
    // Ensure Savfox includes client_version on refresh. (This is a stable signal that we're using
    // the /models client.)
    assert!(
        refresh_req
            .url
            .query_pairs()
            .any(|(k, _)| k == "client_version"),
        "expected /models refresh to include client_version query param"
    );

    // Assert the tool output /responses request succeeded and did not trigger another /models
    // fetch.
    let tool_req = tool_output_mock.single_request();
    let _ = tool_req.function_call_output(CALL_ID);
    assert_eq!(refresh_models_mock.requests().len(), 1);

    Ok(())
}
