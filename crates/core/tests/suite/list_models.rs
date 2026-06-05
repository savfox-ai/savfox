use anyhow::Result;
use core_test_support::load_default_config_for_test;
use pretty_assertions::assert_eq;
use savfox_core::models_manager::manager::RefreshStrategy;
use savfox_core::models_manager::model_presets::all_model_presets;
use savfox_core::{SavfoxAuth, SessionManager, built_in_model_providers};
use savfox_protocol::openai_models::ModelPreset;
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_models_returns_api_key_models() -> Result<()> {
    let savfox_home = tempdir()?;
    let config = load_default_config_for_test(&savfox_home).await;
    let manager = SessionManager::with_models_provider(
        SavfoxAuth::from_api_key("sk-test"),
        built_in_model_providers()["openai"].clone(),
    );
    let models = manager.list_models(&config, RefreshStrategy::Offline).await;

    let expected_models = expected_models_for_api_key();
    assert_eq!(expected_models, models);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_models_returns_chatgpt_models() -> Result<()> {
    let savfox_home = tempdir()?;
    let config = load_default_config_for_test(&savfox_home).await;
    let manager = SessionManager::with_models_provider(
        SavfoxAuth::create_dummy_chatgpt_auth_for_testing(),
        built_in_model_providers()["openai"].clone(),
    );
    let models = manager.list_models(&config, RefreshStrategy::Offline).await;

    let expected_models = expected_models_for_chatgpt();
    assert_eq!(expected_models, models);

    Ok(())
}

fn expected_models_for_api_key() -> Vec<ModelPreset> {
    all_model_presets()
        .iter()
        .filter(|model| model.supported_in_api)
        .cloned()
        .collect()
}

fn expected_models_for_chatgpt() -> Vec<ModelPreset> {
    all_model_presets().clone()
}
