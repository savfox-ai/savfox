#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use core_test_support::responses::{
    ResponsesRequest, ev_assistant_message, ev_completed, ev_function_call, ev_response_created,
    sse, start_mock_server,
};
use core_test_support::test_savfox::{TestSavfox, test_savfox};
use core_test_support::{responses, skip_if_no_network, wait_for_event, wait_for_event_match};
use pretty_assertions::assert_eq;
use savfox_core::features::Feature;
use savfox_core::protocol::{AskForApproval, EventMsg, Op, SandboxPolicy};
use savfox_protocol::config_types::{CollaborationMode, ModeKind, ReasoningSummary, Settings};
use savfox_protocol::request_user_input::{RequestUserInputAnswer, RequestUserInputResponse};
use savfox_protocol::user_input::UserInput;
use serde_json::{Value, json};

fn call_output(req: &ResponsesRequest, call_id: &str) -> String {
    let raw = req.function_call_output(call_id);
    assert_eq!(
        raw.get("call_id").and_then(Value::as_str),
        Some(call_id),
        "mismatched call_id in function_call_output"
    );
    let (content_opt, _success) = match req.function_call_output_content_and_success(call_id) {
        Some(values) => values,
        None => panic!("function_call_output present"),
    };
    match content_opt {
        Some(content) => content,
        None => panic!("function_call_output content present"),
    }
}

fn call_output_content_and_success(
    req: &ResponsesRequest,
    call_id: &str,
) -> (String, Option<bool>) {
    let raw = req.function_call_output(call_id);
    assert_eq!(
        raw.get("call_id").and_then(Value::as_str),
        Some(call_id),
        "mismatched call_id in function_call_output"
    );
    let (content_opt, success) = match req.function_call_output_content_and_success(call_id) {
        Some(values) => values,
        None => panic!("function_call_output present"),
    };
    let content = match content_opt {
        Some(content) => content,
        None => panic!("function_call_output content present"),
    };
    (content, success)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_user_input_round_trip_resolves_pending() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let builder = test_savfox();
    let TestSavfox {
        savfox,
        cwd,
        session_configured,
        ..
    } = builder
        .with_config(|config| {
            config.features.enable(Feature::CollaborationModes);
        })
        .build(&server)
        .await?;

    let call_id = "user-input-call";
    let request_args = json!({
        "questions": [{
            "id": "confirm_path",
            "header": "Confirm",
            "question": "Proceed with the plan?",
            "options": [{
                "label": "Yes (Recommended)",
                "description": "Continue the current plan."
            }, {
                "label": "No",
                "description": "Stop and revisit the approach."
            }]
        }]
    })
    .to_string();

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(call_id, "request_user_input", &request_args),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "thanks"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    savfox
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please confirm".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
            collaboration_mode: Some(CollaborationMode {
                mode: ModeKind::Plan,
                settings: Settings {
                    model: session_configured.model.clone(),
                    reasoning_effort: None,
                    developer_instructions: None,
                },
            }),
            personality: None,
        })
        .await?;

    let request = wait_for_event_match(&savfox, |event| match event {
        EventMsg::RequestUserInput(request) => Some(request.clone()),
        _ => None,
    })
    .await;
    assert_eq!(request.call_id, call_id);
    assert_eq!(request.questions.len(), 1);
    assert_eq!(request.questions[0].is_other, true);

    let mut answers = HashMap::new();
    answers.insert(
        "confirm_path".to_string(),
        RequestUserInputAnswer {
            answers: vec!["yes".to_string()],
        },
    );
    let response = RequestUserInputResponse { answers };
    savfox
        .submit(Op::UserInputAnswer {
            id: request.turn_id.clone(),
            response,
        })
        .await?;

    wait_for_event(&savfox, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let req = second_mock.single_request();
    let output_text = call_output(&req, call_id);
    let output_json: Value = serde_json::from_str(&output_text)?;
    assert_eq!(
        output_json,
        json!({
            "answers": {
                "confirm_path": { "answers": ["yes"] }
            }
        })
    );

    Ok(())
}

async fn assert_request_user_input_rejected<F>(mode_name: &str, build_mode: F) -> anyhow::Result<()>
where
    F: FnOnce(String) -> CollaborationMode,
{
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let builder = test_savfox();
    let TestSavfox {
        savfox,
        cwd,
        session_configured,
        ..
    } = builder
        .with_config(|config| {
            config.features.enable(Feature::CollaborationModes);
        })
        .build(&server)
        .await?;

    let mode_slug = mode_name.to_lowercase();
    let call_id = format!("user-input-{mode_slug}-call");
    let request_args = json!({
        "questions": [{
            "id": "confirm_path",
            "header": "Confirm",
            "question": "Proceed with the plan?",
            "options": [{
                "label": "Yes (Recommended)",
                "description": "Continue the current plan."
            }, {
                "label": "No",
                "description": "Stop and revisit the approach."
            }]
        }]
    })
    .to_string();

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(&call_id, "request_user_input", &request_args),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "thanks"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();
    let collaboration_mode = build_mode(session_model.clone());

    savfox
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please confirm".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
            collaboration_mode: Some(collaboration_mode),
            personality: None,
        })
        .await?;

    wait_for_event(&savfox, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let req = second_mock.single_request();
    let (output, success) = call_output_content_and_success(&req, &call_id);
    assert_eq!(success, None);
    assert_eq!(
        output,
        format!("request_user_input is unavailable in {mode_name} mode")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_user_input_rejected_in_execute_mode() -> anyhow::Result<()> {
    assert_request_user_input_rejected("Execute", |model| CollaborationMode {
        mode: ModeKind::Execute,
        settings: Settings {
            model,
            reasoning_effort: None,
            developer_instructions: None,
        },
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_user_input_rejected_in_code_mode() -> anyhow::Result<()> {
    assert_request_user_input_rejected("Code", |model| CollaborationMode {
        mode: ModeKind::Code,
        settings: Settings {
            model,
            reasoning_effort: None,
            developer_instructions: None,
        },
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_user_input_rejected_in_custom_mode() -> anyhow::Result<()> {
    assert_request_user_input_rejected("Custom", |model| CollaborationMode {
        mode: ModeKind::Custom,
        settings: Settings {
            model,
            reasoning_effort: None,
            developer_instructions: None,
        },
    })
    .await
}
