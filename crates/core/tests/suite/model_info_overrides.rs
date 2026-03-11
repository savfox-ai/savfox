use core_test_support::load_default_config_for_test;
use pretty_assertions::assert_eq;
use savfox_core::models_manager::manager::ModelsManager;
use savfox_protocol::openai_models::TruncationPolicyConfig;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_model_info_without_tool_output_override() {
    let savfox_home = TempDir::new().expect("create temp dir");
    let config = load_default_config_for_test(&savfox_home).await;

    let model_info = ModelsManager::construct_model_info_offline("gpt-5.1", &config);

    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::bytes(10_000)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_model_info_with_tool_output_override() {
    let savfox_home = TempDir::new().expect("create temp dir");
    let mut config = load_default_config_for_test(&savfox_home).await;
    config.tool_output_token_limit = Some(123);

    let model_info = ModelsManager::construct_model_info_offline("gpt-5.1-savfox", &config);

    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::tokens(123)
    );
}
