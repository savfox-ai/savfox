#![allow(clippy::unwrap_used, clippy::expect_used)]

use core::time::Duration;

use core_test_support::{load_default_config_for_test, wait_for_event};
use savfox_core::config::CONFIG_TOML_FILE;
use savfox_core::features::Feature;
use savfox_core::protocol::{EventMsg, InitialHistory, WarningEvent};
use savfox_core::{AuthManager, NewSession, SavfoxAuth, SessionManager};
use savfox_utils::absolute_path::AbsolutePathBuf;
use tempfile::TempDir;
use tokio::time::timeout;
use toml::toml;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emits_warning_when_unstable_features_enabled_via_config() {
    let home = TempDir::new().expect("tempdir");
    let mut config = load_default_config_for_test(&home).await;
    config.features.enable(Feature::ChildAgentsMd);
    let user_config_path =
        AbsolutePathBuf::from_absolute_path(config.savfox_home.join(CONFIG_TOML_FILE))
            .expect("absolute user config path");
    config.config_layer_stack = config.config_layer_stack.with_user_config(
        &user_config_path,
        toml! { features = { child_agents_md = true } }.into(),
    );

    let session_manager = SessionManager::with_models_provider(
        SavfoxAuth::from_api_key("test"),
        config.model_provider.clone(),
    );
    let auth_manager = AuthManager::from_auth_for_testing(SavfoxAuth::from_api_key("test"));

    let NewSession {
        session: conversation,
        ..
    } = session_manager
        .resume_session_with_history(config, InitialHistory::New, auth_manager)
        .await
        .expect("spawn conversation");

    let warning = wait_for_event(&conversation, |ev| matches!(ev, EventMsg::Warning(_))).await;
    let EventMsg::Warning(WarningEvent { message }) = warning else {
        panic!("expected warning event");
    };
    assert!(message.contains("child_agents_md"));
    assert!(message.contains("Under-development features enabled"));
    assert!(message.contains("suppress_unstable_features_warning = true"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suppresses_warning_when_configured() {
    let home = TempDir::new().expect("tempdir");
    let mut config = load_default_config_for_test(&home).await;
    config.features.enable(Feature::ChildAgentsMd);
    config.suppress_unstable_features_warning = true;
    let user_config_path =
        AbsolutePathBuf::from_absolute_path(config.savfox_home.join(CONFIG_TOML_FILE))
            .expect("absolute user config path");
    config.config_layer_stack = config.config_layer_stack.with_user_config(
        &user_config_path,
        toml! { features = { child_agents_md = true } }.into(),
    );

    let session_manager = SessionManager::with_models_provider(
        SavfoxAuth::from_api_key("test"),
        config.model_provider.clone(),
    );
    let auth_manager = AuthManager::from_auth_for_testing(SavfoxAuth::from_api_key("test"));

    let NewSession {
        session: conversation,
        ..
    } = session_manager
        .resume_session_with_history(config, InitialHistory::New, auth_manager)
        .await
        .expect("spawn conversation");

    let warning = timeout(
        Duration::from_millis(150),
        wait_for_event(&conversation, |ev| matches!(ev, EventMsg::Warning(_))),
    )
    .await;
    assert!(warning.is_err());
}
