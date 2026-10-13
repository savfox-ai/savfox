use core_test_support::load_default_config_for_test;
use pretty_assertions::assert_eq;
use savfox_core::models_manager::manager::ModelsManager;
use savfox_protocol::openai_models::TruncationPolicyConfig;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_model_info_without_tool_output_override() {
    let savfox_home = TempDir::new().expect("create temp dir");
    let config = load_default_config_for_test(&savfox_home).await;

    let model_info = ModelsManager::construct_model_info_offline("some-model", &config);

    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::bytes(10_000)
    );
}

/// The override preserves the policy's *mode*: a byte-limited model stays byte
/// limited, and a token-limited one stays token limited. The mode is catalog
/// metadata, so each case states it directly instead of picking a model slug
/// that used to imply it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_output_override_preserves_token_mode() {
    let savfox_home = TempDir::new().expect("create temp dir");
    let mut config = load_default_config_for_test(&savfox_home).await;
    config.tool_output_token_limit = Some(123);

    let mut model = savfox_model::find_model_info_for_slug("some-model");
    model.truncation_policy = TruncationPolicyConfig::tokens(10_000);
    let model_info = ModelsManager::apply_config_overrides(model, &config);

    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::tokens(123)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_output_override_preserves_byte_mode() {
    let savfox_home = TempDir::new().expect("create temp dir");
    let mut config = load_default_config_for_test(&savfox_home).await;
    config.tool_output_token_limit = Some(123);

    let mut model = savfox_model::find_model_info_for_slug("some-model");
    model.truncation_policy = TruncationPolicyConfig::bytes(10_000);
    let model_info = ModelsManager::apply_config_overrides(model, &config);

    assert_eq!(
        model_info.truncation_policy.mode,
        TruncationPolicyConfig::bytes(0).mode
    );
    assert!(model_info.truncation_policy.limit > 123);
}
