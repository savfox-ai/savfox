use serde_json::json;

use super::protocol::State;

#[test]
fn final_usage_calibrates_and_subagent_text_is_ignored() {
    let mut state = State::default();
    state.event(&json!({"type":"assistant","session_id":"s","parent_tool_use_id":"tool","message":{"id":"sub","content":[{"type":"text","text":"private"}]}}), None, "s").unwrap();
    state.event(&json!({"type":"result","session_id":"s","subtype":"success","result":"done","usage":{"input_tokens":10,"output_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":2}}), None, "s").unwrap();
    assert_eq!(state.reply, "done");
    assert_eq!(state.usage.unwrap().total_tokens, 20);
}

#[test]
fn partial_full_and_result_do_not_duplicate_text() {
    use std::sync::{Arc, Mutex};
    let chunks = Arc::new(Mutex::new(String::new()));
    let capture = chunks.clone();
    let sink: crate::agent_terminal_delegate::TerminalEventSink = Arc::new(move |event| {
        if let crate::terminal_agent::TerminalAgentEvent::OutputDelta { text, .. } = event {
            capture.lock().unwrap().push_str(&text);
        }
    });
    let sink = Some(sink);
    let mut state = State::default();
    state
        .event(
            &json!({"type":"stream_event","session_id":"s","event":{"type":"message_start"}}),
            sink.as_ref(),
            "s",
        )
        .unwrap();
    state.event(&json!({"type":"stream_event","session_id":"s","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}}), sink.as_ref(), "s").unwrap();
    let full = json!({"type":"assistant","session_id":"s","message":{"id":"m","content":[{"type":"text","text":"hello"}]}});
    state.event(&full, sink.as_ref(), "s").unwrap();
    state.event(&full, sink.as_ref(), "s").unwrap();
    state
        .event(
            &json!({"type":"result","session_id":"s","result":"hello","subtype":"success"}),
            sink.as_ref(),
            "s",
        )
        .unwrap();
    assert_eq!(chunks.lock().unwrap().as_str(), "hello");
    assert_eq!(state.reply, "hello");
    assert!(state.usage.is_none());
}

#[test]
fn invalid_identity_and_unknown_display_events() {
    let mut state = State::default();
    assert!(
        state
            .event(&json!({"type":"system","session_id":"other"}), None, "s")
            .is_err()
    );
    assert!(
        state
            .event(
                &json!({"type":"future_display","new_field":true}),
                None,
                "s"
            )
            .is_ok()
    );
    assert!(
        state
            .event(&json!({"missing_type":true}), None, "s")
            .is_err()
    );
    assert_eq!(
        super::protocol::unsupported("x")["response"]["subtype"],
        "error"
    );
}

#[test]
fn configuration_requires_version_and_structured_arguments() {
    let mut config = savfox_gateway_shared::ClaudeCodeConfig::default();
    assert!(super::validate(&config, &[]).is_err());
    config.expected_version = Some("fixture".to_owned());
    assert!(super::validate(&config, &[]).is_ok());
    assert!(super::validate(&config, &["--dangerously-skip-permissions".to_owned()]).is_err());
    assert!(super::validate_workspace(Some("read_only")).is_err());
    assert!(super::validate_workspace(Some("unknown")).is_err());
    assert!(super::validate_workspace(Some("worktree")).is_ok());
    config.model = Some("anthropic/claude".to_owned());
    assert!(super::validate(&config, &[]).is_err());
}

#[test]
fn answers_preserve_original_questions_and_other_input() {
    let input = json!({"questions":[{"question":"Which?","options":[{"label":"A"},{"label":"B"}]}],"extra":"original"});
    let updated = super::protocol::approved_input(&input, &json!({"0":"B"})).unwrap();
    assert_eq!(updated["questions"], input["questions"]);
    assert_eq!(updated["extra"], "original");
    assert_eq!(updated["answers"], json!({"Which?":"B"}));
    assert!(super::protocol::approved_input(&input, &json!({"unrelated":"A"})).is_err());
    let duplicated = json!({"questions":[input["questions"][0],input["questions"][0]]});
    assert!(super::protocol::approved_input(&duplicated, &json!({"0":"A","1":"B"})).is_err());
}

#[cfg(unix)]
mod process {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use savfox_gateway_shared::ClaudeCodeConfig;
    use savfox_protocol::SessionId;
    use serde_json::Value;
    use tokio::sync::mpsc;

    use super::*;
    use crate::auth::{TokenInfo, TokenScope};
    use crate::claude_code::{Execution, RunOptions, Runtime};
    use crate::session::{ClientSession, GatewaySessionManager};
    use crate::terminal_agent::{TerminalCommandSpec, TerminalTemplateValues};

    struct Fixture {
        root: tempfile::TempDir,
        runtime: Runtime,
        execution: Execution,
        events: mpsc::Receiver<crate::protocol::GatewayMessage>,
    }

    impl Fixture {
        async fn new(approval: bool) -> Self {
            let root = tempfile::tempdir().unwrap();
            let sessions = GatewaySessionManager::new();
            let (sender, events) = mpsc::channel(32);
            let mut client = ClientSession::new(
                SessionId::new(),
                TokenInfo {
                    label: "same label".to_owned(),
                    scopes: vec![TokenScope::Operator],
                },
                sender,
            );
            client.principal = "owner".to_owned();
            sessions.add_session(client).await;
            Self {
                runtime: Runtime::new(root.path().to_owned(), sessions),
                execution: Execution {
                    owner: "owner".to_owned(),
                    request_id: "request-1".to_owned(),
                    session_id: SessionId::new().to_string(),
                    agent_id: "claude".to_owned(),
                    approval_client: approval,
                    trusted_context: None,
                },
                root,
                events,
            }
        }

        async fn options(&self, mode: &str) -> RunOptions {
            let (run_id, cancel, existing) = self
                .runtime
                .begin(
                    self.execution.clone(),
                    format!("input-{}", self.execution.request_id),
                )
                .await
                .unwrap();
            assert!(existing.is_none());
            RunOptions {
                spec: TerminalCommandSpec {
                    program: concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/claude/cli.sh")
                        .to_owned(),
                    args: Vec::new(),
                    cwd: self.root.path().to_owned(),
                    env: BTreeMap::from([
                        (
                            "FAKE_BINDING".to_owned(),
                            self.root
                                .path()
                                .join("binding.json")
                                .to_string_lossy()
                                .into_owned(),
                        ),
                        ("FAKE_MODE".to_owned(), mode.to_owned()),
                        (
                            "FAKE_INPUT".to_owned(),
                            self.root
                                .path()
                                .join("input.log")
                                .to_string_lossy()
                                .into_owned(),
                        ),
                        (
                            "FAKE_CONTROL".to_owned(),
                            self.root
                                .path()
                                .join("control.log")
                                .to_string_lossy()
                                .into_owned(),
                        ),
                        (
                            "FAKE_ARGS".to_owned(),
                            self.root
                                .path()
                                .join("args.log")
                                .to_string_lossy()
                                .into_owned(),
                        ),
                    ]),
                    stdin: None,
                    timeout: Duration::from_secs(5),
                    max_output_bytes: 1024 * 1024,
                },
                config: ClaudeCodeConfig {
                    expected_version: Some("synthetic-claude-2".to_owned()),
                    approval_timeout_secs: 1,
                    shutdown_timeout_secs: 1,
                    ..Default::default()
                },
                values: TerminalTemplateValues {
                    prompt: "current request".to_owned(),
                    conversation_context: "older history".to_owned(),
                    system_prompt: "system instructions".to_owned(),
                    ..Default::default()
                },
                binding_path: self.root.path().join("binding.json"),
                resume: true,
                bridge: if self.execution.approval_client {
                    "required"
                } else {
                    "prompt"
                }
                .to_owned(),
                sink: None,
                runtime: self.runtime.clone(),
                execution: self.execution.clone(),
                run_id,
                cancel,
            }
        }

        async fn approval_event(&mut self) -> Value {
            loop {
                match tokio::time::timeout(Duration::from_secs(3), self.events.recv())
                    .await
                    .unwrap()
                    .unwrap()
                {
                    crate::protocol::GatewayMessage::Event { event, payload, .. }
                        if event == "exec.approval.requested" || event == "claude.question" =>
                    {
                        return payload;
                    }
                    _ => {}
                }
            }
        }
    }

    #[tokio::test]
    async fn cancelled_registered_run_does_not_start_a_process() {
        let fixture = Fixture::new(false).await;
        let options = fixture.options("normal").await;
        assert!(fixture.runtime.cancel("owner", &options.run_id).await);
        let output = crate::claude_code::run(options).await;
        assert!(output.supervised.pid.is_none());
        assert!(!fixture.root.path().join("input.log").exists());
        assert_eq!(
            fixture.runtime.query("owner", "request-1").await.unwrap()["status"],
            "cancelled"
        );
        fixture
            .runtime
            .finish_setup_error(&fixture.execution, &output.run_id, "late error".to_owned())
            .await;
        assert_eq!(
            fixture.runtime.query("owner", "request-1").await.unwrap()["status"],
            "cancelled"
        );
    }

    #[tokio::test]
    async fn expired_or_audit_consumed_approval_never_grants() {
        for expired in [true, false] {
            let fixture = Fixture::new(true).await;
            let options = fixture.options("approval").await;
            let (id, receiver) = fixture
                .runtime
                .approval(
                    &fixture.execution,
                    &options.run_id,
                    "control",
                    "Bash",
                    &json!({"command":"original"}),
                    fixture.root.path(),
                    if expired {
                        Duration::ZERO
                    } else {
                        Duration::from_secs(60)
                    },
                )
                .await
                .unwrap();
            let pending = fixture.runtime.query("owner", "request-1").await.unwrap();
            let nonce = pending["pending_approvals"][0]["nonce"]
                .as_str()
                .unwrap()
                .to_owned();
            if !expired {
                crate::exec_approval::persist_resolved_approval(
                    fixture.root.path(),
                    &crate::exec_approval::ExecApprovalResolution {
                        id: id.clone(),
                        nonce: nonce.clone(),
                        approved: false,
                        resolved_by: None,
                        reason: None,
                    },
                )
                .await
                .unwrap();
            }
            assert!(
                fixture
                    .runtime
                    .resolve(
                        "owner",
                        &json!({"request_id":id,"nonce":nonce,"decision":"approve-once"})
                    )
                    .await
                    .is_err()
            );
            fixture.runtime.remove_approval(&id, "test cleanup").await;
            assert!(receiver.await.is_err());
            fixture
                .runtime
                .finish_setup_error(
                    &fixture.execution,
                    &options.run_id,
                    "test cleanup".to_owned(),
                )
                .await;
        }
    }

    #[tokio::test]
    async fn resume_reuses_identity_without_reinjecting_history() {
        let mut fixture = Fixture::new(false).await;
        let first = crate::claude_code::run(fixture.options("normal").await).await;
        assert!(
            first.supervised.exit_reason.is_success(),
            "{:?}",
            first.supervised
        );
        let binding: Value = serde_json::from_slice(
            &std::fs::read(fixture.root.path().join("binding.json")).unwrap(),
        )
        .unwrap();
        fixture.execution.request_id = "request-2".to_owned();
        let second = crate::claude_code::run(fixture.options("normal").await).await;
        assert!(
            second.supervised.exit_reason.is_success(),
            "{:?}",
            second.supervised
        );
        let next: Value = serde_json::from_slice(
            &std::fs::read(fixture.root.path().join("binding.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(binding["session_id"], next["session_id"]);
        let inputs = std::fs::read_to_string(fixture.root.path().join("input.log")).unwrap();
        let lines: Vec<_> = inputs.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("older history"));
        assert!(!lines[1].contains("older history"));
        assert!(!inputs.contains("system instructions"));
        let args = std::fs::read_to_string(fixture.root.path().join("args.log")).unwrap();
        assert!(args.contains("--append-system-prompt\nsystem instructions"));
        assert!(args.contains("--resume"));
        assert_eq!(second.usage.unwrap().total_tokens, 20);
    }

    #[tokio::test]
    async fn approve_once_returns_unchanged_original_input_and_rejects_replay() {
        let mut fixture = Fixture::new(true).await;
        let options = fixture.options("approval").await;
        let run = tokio::spawn(crate::claude_code::run(options));
        let request = fixture.approval_event().await;
        let params = json!({"request_id":request["request_id"],"nonce":request["nonce"],"decision":"approve-once","updatedInput":{"command":"replacement"}});
        assert!(fixture.runtime.resolve("other", &params).await.is_err());
        let mut wrong = params.clone();
        wrong["nonce"] = json!("wrong");
        assert!(fixture.runtime.resolve("owner", &wrong).await.is_err());
        assert!(
            fixture
                .runtime
                .resolve("owner", &params)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            fixture
                .runtime
                .resolve("owner", &params)
                .await
                .unwrap()
                .is_none()
        );
        let output = run.await.unwrap();
        assert!(
            output.supervised.exit_reason.is_success(),
            "{:?}",
            output.supervised
        );
        assert_eq!(output.supervised.stdout, "allowed");
        let control: Value = serde_json::from_slice(
            std::fs::read(fixture.root.path().join("control.log"))
                .unwrap()
                .split(|byte| *byte == b'\n')
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            control["response"]["response"]["updatedInput"],
            json!({"command":"printf original","token":"secret-original"})
        );
        assert!(!request.to_string().contains("secret-original"));
    }

    #[tokio::test]
    async fn question_answers_use_original_question_keys() {
        let mut fixture = Fixture::new(true).await;
        let run = tokio::spawn(crate::claude_code::run(fixture.options("question").await));
        let request = fixture.approval_event().await;
        let mut params = json!({"request_id":request["request_id"],"nonce":request["nonce"],"decision":"approve-once","answers":{"99":"wrong"}});
        assert!(fixture.runtime.resolve("owner", &params).await.is_err());
        params["answers"] = json!({"0":"B"});
        assert!(
            fixture
                .runtime
                .resolve("owner", &params)
                .await
                .unwrap()
                .is_some()
        );
        let output = run.await.unwrap();
        assert!(output.supervised.exit_reason.is_success());
        assert!(
            std::fs::read_to_string(fixture.root.path().join("control.log"))
                .unwrap()
                .contains("\"Which?\":\"B\"")
        );
    }

    #[tokio::test]
    async fn approval_timeout_and_no_client_deny() {
        for approval in [true, false] {
            let fixture = Fixture::new(approval).await;
            let output = crate::claude_code::run(fixture.options("approval").await).await;
            assert!(
                output.supervised.exit_reason.is_success(),
                "{:?}",
                output.supervised
            );
            assert_eq!(output.supervised.stdout, "denied");
            assert!(fixture.runtime.pending_ids("owner").await.is_empty());
        }
    }

    #[tokio::test]
    async fn cancellation_during_approval_invalidates_nonce_and_reaps_process() {
        let mut fixture = Fixture::new(true).await;
        let options = fixture.options("approval").await;
        let run_id = options.run_id.clone();
        let run = tokio::spawn(crate::claude_code::run(options));
        let request = fixture.approval_event().await;
        assert!(!fixture.runtime.cancel("other", &run_id).await);
        assert!(fixture.runtime.cancel("owner", &run_id).await);
        let output = tokio::time::timeout(Duration::from_secs(4), run)
            .await
            .unwrap()
            .unwrap();
        assert!(!output.supervised.exit_reason.is_success());
        assert!(fixture.runtime.pending_ids("owner").await.is_empty());
        assert!(fixture.runtime.resolve("owner", &json!({"request_id":request["request_id"],"nonce":request["nonce"],"decision":"approve-once"})).await.unwrap().is_none());
        assert_eq!(
            fixture.runtime.query("owner", "request-1").await.unwrap()["status"],
            "cancelled"
        );
    }

    #[tokio::test]
    async fn shutdown_cancels_owned_process_and_rejects_new_runs() {
        let fixture = Fixture::new(false).await;
        let run = tokio::spawn(crate::claude_code::run(fixture.options("hanging").await));
        tokio::time::timeout(Duration::from_secs(3), async {
            while !fixture.root.path().join("input.log").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        fixture.runtime.shutdown().await;
        let output = run.await.unwrap();
        assert!(!output.supervised.exit_reason.is_success());
        assert!(output.supervised.pid.is_some());
        assert_eq!(
            fixture.runtime.query("owner", "request-1").await.unwrap()["status"],
            "cancelled"
        );
        assert!(
            fixture
                .runtime
                .begin(fixture.execution.clone(), "new".to_owned())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn final_binding_write_failure_retains_reply_but_fails_execution() {
        let fixture = Fixture::new(false).await;
        let output = crate::claude_code::run(fixture.options("binding_error").await).await;
        assert_eq!(output.supervised.stdout, "done");
        assert_eq!(output.supervised.exit_code, Some(0));
        assert!(!output.supervised.exit_reason.is_success());
        assert!(output.supervised.error.is_some());
        assert_eq!(
            fixture.runtime.query("owner", "request-1").await.unwrap()["status"],
            "error"
        );
    }

    #[tokio::test]
    async fn bounds_bad_json_and_drains_stderr() {
        for mode in ["bad_json", "oversized", "truncated"] {
            let fixture = Fixture::new(false).await;
            let output = crate::claude_code::run(fixture.options(mode).await).await;
            assert!(!output.supervised.exit_reason.is_success(), "mode {mode}");
            assert!(output.supervised.error.is_some());
        }
        let fixture = Fixture::new(false).await;
        let output = crate::claude_code::run(fixture.options("stderr").await).await;
        assert!(
            output.supervised.exit_reason.is_success(),
            "{:?}",
            output.supervised
        );
        assert!(output.supervised.stderr_truncated);
        assert!(output.supervised.stderr.len() <= 64 * 1024);
    }

    #[tokio::test]
    async fn finite_background_continuation_is_not_killed_at_first_result() {
        let fixture = Fixture::new(false).await;
        let output = crate::claude_code::run(fixture.options("background").await).await;
        assert!(
            output.supervised.exit_reason.is_success(),
            "{:?}",
            output.supervised
        );
        assert_eq!(output.supervised.stdout, "continued");
    }

    #[tokio::test]
    async fn duplicate_requests_and_restarted_unknown_runs_never_replay() {
        let fixture = Fixture::new(false).await;
        let options = fixture.options("normal").await;
        let (_, _, duplicate) = fixture
            .runtime
            .begin(fixture.execution.clone(), "input-request-1".to_owned())
            .await
            .unwrap();
        assert!(duplicate.is_some());
        assert!(
            fixture
                .runtime
                .begin(fixture.execution.clone(), "different".to_owned())
                .await
                .is_err()
        );
        let restarted = Runtime::new(fixture.root.path().to_owned(), GatewaySessionManager::new());
        assert_eq!(
            restarted.query("owner", "request-1").await.unwrap()["status"],
            "unknown_after_restart"
        );
        assert!(
            restarted
                .begin(fixture.execution.clone(), "input-request-1".to_owned())
                .await
                .is_err()
        );
        let _ = crate::claude_code::run(options).await;
        let (_, _, duplicate) = fixture
            .runtime
            .begin(fixture.execution.clone(), "input-request-1".to_owned())
            .await
            .unwrap();
        assert!(duplicate.unwrap().borrow().is_some());
        assert!(fixture.runtime.query("other", "request-1").await.is_err());
        assert!(
            fixture
                .runtime
                .authorize_session(Some("other"), &fixture.execution.session_id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn changed_policy_requires_new_session_before_prompt() {
        let mut fixture = Fixture::new(false).await;
        let _ = crate::claude_code::run(fixture.options("normal").await).await;
        fixture.execution.request_id = "second".to_owned();
        let mut options = fixture.options("normal").await;
        options.values.system_prompt = "changed".to_owned();
        let output = crate::claude_code::run(options).await;
        assert!(output.supervised.error.unwrap().contains("binding changed"));
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("input.log"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}
