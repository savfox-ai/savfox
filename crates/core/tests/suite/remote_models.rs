#![cfg(not(target_os = "windows"))]
#![allow(clippy::expect_used)]
// unified exec is not supported on Windows OS
use std::sync::Arc;

use anyhow::Result;
use core_test_support::responses::{
    ev_assistant_message, ev_completed, ev_function_call, ev_response_created, mount_models_once,
    mount_models_once_with_delay, mount_sse_once, mount_sse_sequence, sse,
};
use core_test_support::test_savfox::{TestSavfox, test_savfox};
use core_test_support::{
    load_default_config_for_test, skip_if_no_network, skip_if_sandbox, wait_for_event,
    wait_for_event_match,
};
use pretty_assertions::assert_eq;
use savfox_core::config::Config;
use savfox_core::features::Feature;
use savfox_core::models_manager::manager::{ModelsManager, RefreshStrategy};
use savfox_core::protocol::{AskForApproval, EventMsg, ExecCommandSource, Op, SandboxPolicy};
use savfox_core::{ModelProviderInfo, SavfoxAuth, built_in_model_providers};
use savfox_protocol::config_types::ReasoningSummary;
use savfox_protocol::openai_models::{
    ConfigShellToolType, ModelInfo, ModelPreset, ModelVisibility, ModelsResponse, ReasoningEffort,
    ReasoningEffortPreset, TruncationPolicyConfig, default_input_modalities,
};
use savfox_protocol::user_input::UserInput;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::{Duration, Instant, sleep, timeout};
use wiremock::{BodyPrintLimit, MockServer};

const REMOTE_MODEL_SLUG: &str = "savfox-test";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_models_remote_model_uses_unified_exec() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = MockServer::builder()
        .body_print_limit(BodyPrintLimit::Limited(80_000))
        .start()
        .await;

    let remote_model = ModelInfo {
        slug: REMOTE_MODEL_SLUG.to_string(),
        name: "Remote Test".to_string(),
        description: Some("A remote model that requires the test shell".to_string()),
        default_reasoning_level: Some(ReasoningEffort::Medium),
        supported_reasoning_levels: vec![ReasoningEffortPreset {
            effort: ReasoningEffort::Medium,
            description: ReasoningEffort::Medium.to_string(),
        }],
        shell_type: ConfigShellToolType::UnifiedExec,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        input_modalities: default_input_modalities(),
        priority: 1,
        upgrade: None,
        base_instructions: "base instructions".to_string(),
        model_messages: None,
        supports_reasoning_summaries: false,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        truncation_policy: TruncationPolicyConfig::bytes(10_000),
        supports_parallel_tool_calls: false,
        context_window: Some(272_000),
        max_output_tokens: None,
        auto_compact_token_limit: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
    };

    let models_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: vec![remote_model],
        },
    )
    .await;

    let mut builder = test_savfox()
        .with_auth(SavfoxAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config.features.enable(Feature::RemoteModels);
            config.model = Some("gpt-5.1".to_string());
        });
    let TestSavfox {
        savfox,
        cwd,
        config,
        session_manager,
        ..
    } = builder.build(&server).await?;

    let models_manager = session_manager.get_models_manager();
    let available_model =
        wait_for_model_available(&models_manager, REMOTE_MODEL_SLUG, &config).await;

    assert_eq!(available_model.slug, REMOTE_MODEL_SLUG);

    let requests = models_mock.requests();
    assert_eq!(
        requests.len(),
        1,
        "expected a single /models refresh request for the remote models feature"
    );
    assert_eq!(requests[0].url.path(), "/v1/models");

    let model_info = models_manager
        .get_model_info(REMOTE_MODEL_SLUG, &config)
        .await;
    assert_eq!(model_info.shell_type, ConfigShellToolType::UnifiedExec);

    savfox
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: Some(REMOTE_MODEL_SLUG.to_string()),
            effort: None,
            summary: None,
            collaboration_mode: None,
            personality: None,
            permission_policy: None,
        })
        .await?;

    let call_id = "call";
    let args = json!({
        "cmd": "/bin/echo call",
        "yield_time_ms": 250,
    });
    let responses = vec![
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "exec_command", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
        sse(vec![
            ev_response_created("resp-2"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    ];
    mount_sse_sequence(&server, responses).await;

    savfox
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "run call".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: REMOTE_MODEL_SLUG.to_string(),
            effort: None,
            summary: ReasoningSummary::Auto,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    let begin_event = wait_for_event_match(&savfox, |msg| match msg {
        EventMsg::ExecCommandBegin(event) if event.call_id == call_id => Some(event.clone()),
        _ => None,
    })
    .await;

    assert_eq!(begin_event.source, ExecCommandSource::UnifiedExecStartup);

    wait_for_event(&savfox, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_models_truncation_policy_without_override_preserves_remote() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = MockServer::builder()
        .body_print_limit(BodyPrintLimit::Limited(80_000))
        .start()
        .await;

    let slug = "savfox-test-truncation-policy";
    let remote_model = test_remote_model_with_policy(
        slug,
        ModelVisibility::List,
        1,
        TruncationPolicyConfig::bytes(12_000),
    );
    mount_models_once(
        &server,
        ModelsResponse {
            models: vec![remote_model],
        },
    )
    .await;

    let mut builder = test_savfox()
        .with_auth(SavfoxAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config.features.enable(Feature::RemoteModels);
            config.model = Some("gpt-5.1".to_string());
        });
    let test = builder.build(&server).await?;

    let models_manager = test.session_manager.get_models_manager();
    wait_for_model_available(&models_manager, slug, &test.config).await;

    let model_info = models_manager.get_model_info(slug, &test.config).await;
    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::bytes(12_000)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_models_truncation_policy_with_tool_output_override() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = MockServer::builder()
        .body_print_limit(BodyPrintLimit::Limited(80_000))
        .start()
        .await;

    let slug = "savfox-test-truncation-override";
    let remote_model = test_remote_model_with_policy(
        slug,
        ModelVisibility::List,
        1,
        TruncationPolicyConfig::bytes(10_000),
    );
    mount_models_once(
        &server,
        ModelsResponse {
            models: vec![remote_model],
        },
    )
    .await;

    let mut builder = test_savfox()
        .with_auth(SavfoxAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config.features.enable(Feature::RemoteModels);
            config.model = Some("gpt-5.1".to_string());
            config.tool_output_token_limit = Some(50);
        });
    let test = builder.build(&server).await?;

    let models_manager = test.session_manager.get_models_manager();
    wait_for_model_available(&models_manager, slug, &test.config).await;

    let model_info = models_manager.get_model_info(slug, &test.config).await;
    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::bytes(200)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_models_apply_remote_base_instructions() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = MockServer::builder()
        .body_print_limit(BodyPrintLimit::Limited(80_000))
        .start()
        .await;

    let model = "test-gpt-5-remote";

    let remote_base = "Use the remote base instructions only.";
    let remote_model = ModelInfo {
        slug: model.to_string(),
        name: "Parallel Remote".to_string(),
        description: Some("A remote model with custom instructions".to_string()),
        default_reasoning_level: Some(ReasoningEffort::Medium),
        supported_reasoning_levels: vec![ReasoningEffortPreset {
            effort: ReasoningEffort::Medium,
            description: ReasoningEffort::Medium.to_string(),
        }],
        shell_type: ConfigShellToolType::ShellCommand,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        input_modalities: default_input_modalities(),
        priority: 1,
        upgrade: None,
        base_instructions: remote_base.to_string(),
        model_messages: None,
        supports_reasoning_summaries: false,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        truncation_policy: TruncationPolicyConfig::bytes(10_000),
        supports_parallel_tool_calls: false,
        context_window: Some(272_000),
        max_output_tokens: None,
        auto_compact_token_limit: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
    };
    mount_models_once(
        &server,
        ModelsResponse {
            models: vec![remote_model],
        },
    )
    .await;

    let response_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-1"),
        ]),
    )
    .await;

    let mut builder = test_savfox()
        .with_auth(SavfoxAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config.features.enable(Feature::RemoteModels);
            config.model = Some("gpt-5.1".to_string());
        });
    let TestSavfox {
        savfox,
        cwd,
        config,
        session_manager,
        ..
    } = builder.build(&server).await?;

    let models_manager = session_manager.get_models_manager();
    wait_for_model_available(&models_manager, model, &config).await;

    savfox
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: Some(model.to_string()),
            effort: None,
            summary: None,
            collaboration_mode: None,
            personality: None,
            permission_policy: None,
        })
        .await?;

    savfox
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello remote".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: model.to_string(),
            effort: None,
            summary: ReasoningSummary::Auto,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&savfox, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let base_model_info = models_manager.get_model_info("gpt-5.1", &config).await;
    let body = response_mock.single_request().body_json();
    let instructions = body["instructions"].as_str().unwrap();
    assert_eq!(instructions, base_model_info.base_instructions);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_models_preserve_builtin_presets() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = MockServer::start().await;
    let remote_model = test_remote_model("remote-alpha", ModelVisibility::List, 0);
    let models_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: vec![remote_model.clone()],
        },
    )
    .await;

    let savfox_home = TempDir::new()?;
    let mut config = load_default_config_for_test(&savfox_home).await;
    config.features.enable(Feature::RemoteModels);

    let auth = SavfoxAuth::create_dummy_chatgpt_auth_for_testing();
    let provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };
    let manager = ModelsManager::with_provider(
        savfox_home.path().to_path_buf(),
        savfox_core::auth::AuthManager::from_auth_for_testing(auth),
        provider,
    );

    let available = manager
        .list_models(&config, RefreshStrategy::OnlineIfUncached)
        .await;
    let remote = available
        .iter()
        .find(|model| model.slug == "remote-alpha")
        .expect("remote model should be listed");
    let mut expected_remote: ModelPreset = remote_model.into();
    expected_remote.is_default = remote.is_default;
    assert_eq!(*remote, expected_remote);
    let default_model = available
        .iter()
        .find(|model| model.show_in_picker)
        .expect("default model should be set");
    assert!(default_model.is_default);
    assert_eq!(
        available.iter().filter(|model| model.is_default).count(),
        1,
        "expected a single default model"
    );
    assert!(
        available
            .iter()
            .any(|model| model.slug == "gpt-5.1-savfox-max"),
        "builtin presets should remain available after refresh"
    );
    assert_eq!(
        models_mock.requests().len(),
        1,
        "expected a single /models request"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_models_merge_adds_new_high_priority_first() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = MockServer::start().await;
    let remote_model = test_remote_model("remote-top", ModelVisibility::List, -10_000);
    let models_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: vec![remote_model],
        },
    )
    .await;

    let savfox_home = TempDir::new()?;
    let mut config = load_default_config_for_test(&savfox_home).await;
    config.features.enable(Feature::RemoteModels);

    let auth = SavfoxAuth::create_dummy_chatgpt_auth_for_testing();
    let provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };
    let manager = ModelsManager::with_provider(
        savfox_home.path().to_path_buf(),
        savfox_core::auth::AuthManager::from_auth_for_testing(auth),
        provider,
    );

    let available = manager
        .list_models(&config, RefreshStrategy::OnlineIfUncached)
        .await;
    assert_eq!(
        available.first().map(|model| model.slug.as_str()),
        Some("remote-top")
    );
    assert_eq!(
        models_mock.requests().len(),
        1,
        "expected a single /models request"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_models_merge_replaces_overlapping_model() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = MockServer::start().await;
    let slug = default_model_slug();
    let mut remote_model = test_remote_model(&slug, ModelVisibility::List, 0);
    remote_model.name = "Overridden".to_string();
    remote_model.description = Some("Overridden description".to_string());
    let models_mock = mount_models_once(
        &server,
        ModelsResponse {
            models: vec![remote_model.clone()],
        },
    )
    .await;

    let savfox_home = TempDir::new()?;
    let mut config = load_default_config_for_test(&savfox_home).await;
    config.features.enable(Feature::RemoteModels);

    let auth = SavfoxAuth::create_dummy_chatgpt_auth_for_testing();
    let provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };
    let manager = ModelsManager::with_provider(
        savfox_home.path().to_path_buf(),
        savfox_core::auth::AuthManager::from_auth_for_testing(auth),
        provider,
    );

    let available = manager
        .list_models(&config, RefreshStrategy::OnlineIfUncached)
        .await;
    let overridden = available
        .iter()
        .find(|model| model.slug == slug)
        .expect("overlapping model should be listed");
    assert_eq!(overridden.name, remote_model.name);
    assert_eq!(
        overridden.description,
        remote_model
            .description
            .expect("remote model should include description")
    );
    assert_eq!(
        models_mock.requests().len(),
        1,
        "expected a single /models request"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_presets_available_on_empty_remote_response() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = MockServer::start().await;
    let models_mock = mount_models_once(&server, ModelsResponse { models: Vec::new() }).await;

    let savfox_home = TempDir::new()?;
    let mut config = load_default_config_for_test(&savfox_home).await;
    config.features.enable(Feature::RemoteModels);

    let auth = SavfoxAuth::create_dummy_chatgpt_auth_for_testing();
    let provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };
    let manager = ModelsManager::with_provider(
        savfox_home.path().to_path_buf(),
        savfox_core::auth::AuthManager::from_auth_for_testing(auth),
        provider,
    );

    let available = manager
        .list_models(&config, RefreshStrategy::OnlineIfUncached)
        .await;
    assert!(
        !available.is_empty(),
        "local presets should remain available after empty remote response"
    );
    assert_eq!(
        models_mock.requests().len(),
        1,
        "expected a single /models request"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_models_request_times_out_after_5s() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = MockServer::start().await;
    let remote_model = test_remote_model("remote-timeout", ModelVisibility::List, 0);
    let models_mock = mount_models_once_with_delay(
        &server,
        ModelsResponse {
            models: vec![remote_model],
        },
        Duration::from_secs(6),
    )
    .await;

    let savfox_home = TempDir::new()?;
    let mut config = load_default_config_for_test(&savfox_home).await;
    config.features.enable(Feature::RemoteModels);

    let auth = SavfoxAuth::create_dummy_chatgpt_auth_for_testing();
    let provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };
    let manager = ModelsManager::with_provider(
        savfox_home.path().to_path_buf(),
        savfox_core::auth::AuthManager::from_auth_for_testing(auth),
        provider,
    );

    let start = Instant::now();
    let model = timeout(
        Duration::from_secs(7),
        manager.get_default_model(&None, &config, RefreshStrategy::OnlineIfUncached),
    )
    .await;
    let elapsed = start.elapsed();
    // get_model should return a default model even when refresh times out
    let default_model = model.expect("get_model should finish and return default model");
    assert!(
        default_model == "gpt-5.2-savfox",
        "get_model should return default model when refresh times out, got: {default_model}"
    );
    let _ = server
        .received_requests()
        .await
        .expect("mock server should capture requests")
        .iter()
        .map(|req| format!("{} {}", req.method, req.url.path()))
        .collect::<Vec<String>>();
    assert!(
        elapsed >= Duration::from_millis(4_500),
        "expected models call to block near the timeout; took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(5_800),
        "expected models call to time out before the delayed response; took {elapsed:?}"
    );
    assert_eq!(
        models_mock.requests().len(),
        1,
        "expected a single /models request"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_models_hide_picker_only_models() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = MockServer::start().await;
    let remote_model = test_remote_model("savfox-auto-balanced", ModelVisibility::Hide, 0);
    mount_models_once(
        &server,
        ModelsResponse {
            models: vec![remote_model],
        },
    )
    .await;

    let savfox_home = TempDir::new()?;
    let mut config = load_default_config_for_test(&savfox_home).await;
    config.features.enable(Feature::RemoteModels);

    let auth = SavfoxAuth::create_dummy_chatgpt_auth_for_testing();
    let provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };
    let manager = ModelsManager::with_provider(
        savfox_home.path().to_path_buf(),
        savfox_core::auth::AuthManager::from_auth_for_testing(auth),
        provider,
    );

    let selected = manager
        .get_default_model(&None, &config, RefreshStrategy::OnlineIfUncached)
        .await;
    assert_eq!(selected, "gpt-5.2-savfox");

    let available = manager
        .list_models(&config, RefreshStrategy::OnlineIfUncached)
        .await;
    let hidden = available
        .iter()
        .find(|model| model.slug == "savfox-auto-balanced")
        .expect("hidden remote model should be listed");
    assert!(!hidden.show_in_picker, "hidden models should remain hidden");

    Ok(())
}

async fn wait_for_model_available(
    manager: &Arc<ModelsManager>,
    slug: &str,
    config: &Config,
) -> ModelPreset {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(model) = {
            let guard = manager
                .list_models(config, RefreshStrategy::OnlineIfUncached)
                .await;
            guard.iter().find(|model| model.slug == slug).cloned()
        } {
            return model;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for the remote model {slug} to appear");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn default_model_slug() -> String {
    "gpt-5.3-codex".to_string()
}

fn test_remote_model(slug: &str, visibility: ModelVisibility, priority: i32) -> ModelInfo {
    test_remote_model_with_policy(
        slug,
        visibility,
        priority,
        TruncationPolicyConfig::bytes(10_000),
    )
}

fn test_remote_model_with_policy(
    slug: &str,
    visibility: ModelVisibility,
    priority: i32,
    truncation_policy: TruncationPolicyConfig,
) -> ModelInfo {
    ModelInfo {
        slug: slug.to_string(),
        name: format!("{slug} display"),
        description: Some(format!("{slug} description")),
        default_reasoning_level: Some(ReasoningEffort::Medium),
        supported_reasoning_levels: vec![ReasoningEffortPreset {
            effort: ReasoningEffort::Medium,
            description: ReasoningEffort::Medium.to_string(),
        }],
        shell_type: ConfigShellToolType::ShellCommand,
        visibility,
        supported_in_api: true,
        input_modalities: default_input_modalities(),
        priority,
        upgrade: None,
        base_instructions: "base instructions".to_string(),
        model_messages: None,
        supports_reasoning_summaries: false,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        truncation_policy,
        supports_parallel_tool_calls: false,
        context_window: Some(272_000),
        max_output_tokens: None,
        auto_compact_token_limit: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
    }
}
