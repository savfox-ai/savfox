use anyhow::Result;
use core_test_support::load_default_config_for_test;
use savfox_core::config::provider_store::{ProviderStoreFile, save_provider_store_file};
use savfox_core::models_manager::manager::RefreshStrategy;
use savfox_core::{SavfoxAuth, SessionManager, built_in_model_providers};
use savfox_protocol::openai_models::ModelInfo;
use tempfile::{TempDir, tempdir};

/// There is no bundled catalog any more, so a test that wants models on offer
/// publishes them the way a real account does: through the provider store.
fn publish_models(savfox_home: &TempDir, models: &[ModelInfo]) {
    let mut file = ProviderStoreFile::empty("openai");
    file.models = models
        .iter()
        .map(|model| serde_json::to_value(model).expect("model serializes"))
        .collect();
    file.models_fetched_at = Some(chrono::Utc::now());
    save_provider_store_file(savfox_home.path(), "openai", &file).expect("store file persists");
}

fn model(slug: &str, supported_in_api: bool) -> ModelInfo {
    let mut model = savfox_model::find_model_info_for_slug(slug);
    model.supported_in_api = supported_in_api;
    model
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_models_hides_api_unsupported_models_from_api_key_auth() -> Result<()> {
    let savfox_home = tempdir()?;
    publish_models(
        &savfox_home,
        &[model("api-ok", true), model("chatgpt-only", false)],
    );
    let config = load_default_config_for_test(&savfox_home).await;
    let manager = SessionManager::with_models_provider_and_home(
        SavfoxAuth::from_api_key("sk-test"),
        built_in_model_providers()["openai"].clone(),
        savfox_home.path().to_path_buf(),
    );

    let slugs: Vec<String> = manager
        .list_models(&config, RefreshStrategy::Offline)
        .await
        .into_iter()
        .map(|preset| preset.slug)
        .collect();

    assert_eq!(slugs, vec!["api-ok".to_owned()]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_models_returns_every_model_for_chatgpt_auth() -> Result<()> {
    let savfox_home = tempdir()?;
    publish_models(
        &savfox_home,
        &[model("api-ok", true), model("chatgpt-only", false)],
    );
    let config = load_default_config_for_test(&savfox_home).await;
    let manager = SessionManager::with_models_provider_and_home(
        SavfoxAuth::create_dummy_chatgpt_auth_for_testing(),
        built_in_model_providers()["openai"].clone(),
        savfox_home.path().to_path_buf(),
    );

    let mut slugs: Vec<String> = manager
        .list_models(&config, RefreshStrategy::Offline)
        .await
        .into_iter()
        .map(|preset| preset.slug)
        .collect();
    slugs.sort();

    assert_eq!(slugs, vec!["api-ok".to_owned(), "chatgpt-only".to_owned()]);

    Ok(())
}

/// With nothing published and nothing cached there is simply nothing to offer —
/// the client no longer invents a vendor's model list to fill the gap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_models_is_empty_without_a_catalog() -> Result<()> {
    let savfox_home = tempdir()?;
    let config = load_default_config_for_test(&savfox_home).await;
    let manager = SessionManager::with_models_provider_and_home(
        SavfoxAuth::from_api_key("sk-test"),
        built_in_model_providers()["openai"].clone(),
        savfox_home.path().to_path_buf(),
    );

    let models = manager.list_models(&config, RefreshStrategy::Offline).await;

    assert!(
        models.is_empty(),
        "expected no models without a catalog, got {:?}",
        models.iter().map(|m| &m.slug).collect::<Vec<_>>()
    );

    Ok(())
}
