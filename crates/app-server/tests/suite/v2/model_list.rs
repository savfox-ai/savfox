use std::time::Duration;

use anyhow::{Result, anyhow};
use app_test_support::{McpProcess, test_catalog, to_response, write_models_cache};
use pretty_assertions::assert_eq;
use savfox_app_server_protocol::{
    JSONRPCError, JSONRPCResponse, Model, ModelListParams, ModelListResponse,
    ReasoningEffortOption, RequestId,
};
use savfox_protocol::openai_models::ModelPreset;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;

/// Project a `ModelPreset` (catalog entry) onto the `Model` shape that the
/// app-server emits over the v2 RPC. Mirrors the upstream codex pattern in
/// `codex-rs/app-server/tests/suite/v2/model_list.rs`.
fn model_from_preset(preset: &ModelPreset) -> Model {
    Model {
        id: preset.id.clone(),
        slug: preset.slug.clone(),
        name: preset.name.clone(),
        description: preset.description.clone(),
        supported_reasoning_efforts: preset
            .supported_reasoning_efforts
            .iter()
            .map(|preset| ReasoningEffortOption {
                reasoning_effort: preset.effort,
                description: preset.description.clone(),
            })
            .collect(),
        default_reasoning_effort: preset.default_reasoning_effort,
        input_modalities: preset.input_modalities.clone(),
        // `write_models_cache()` round-trips through a simplified ModelInfo
        // fixture that does not preserve personality placeholders in base
        // instructions, so app-server list results from cache report
        // `supports_personality = false`.
        supports_personality: false,
        is_default: preset.is_default,
    }
}

/// Build the list of models the v2 RPC is expected to return for a fresh cache
/// with non-ChatGPT auth, projected from the same fixture the cache was written
/// from. There is no bundled catalog to draw on: what the server can list is
/// exactly what the test published.
fn expected_visible_models() -> Vec<Model> {
    let presets: Vec<ModelPreset> = test_catalog().into_iter().map(Into::into).collect();
    let mut visible: Vec<ModelPreset> =
        ModelPreset::filter_by_auth(presets, /* chatgpt_mode */ false)
            .into_iter()
            .filter(|preset| preset.show_in_picker)
            .collect();
    // The catalog's first listed model is the default, matching how
    // `ModelsManager::build_available_models` marks one.
    if let Some(first) = visible.first_mut() {
        first.is_default = true;
    }
    visible.iter().map(model_from_preset).collect()
}

#[tokio::test]
async fn list_models_returns_all_models_with_large_limit() -> Result<()> {
    let savfox_home = TempDir::new()?;
    write_models_cache(savfox_home.path())?;
    let mut mcp = McpProcess::new(savfox_home.path()).await?;

    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_list_models_request(ModelListParams {
            limit: Some(100),
            cursor: None,
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let ModelListResponse {
        data: items,
        next_cursor,
    } = to_response::<ModelListResponse>(response)?;

    let expected_models = expected_visible_models();

    assert_eq!(items, expected_models);
    assert!(next_cursor.is_none());
    Ok(())
}

#[tokio::test]
async fn list_models_pagination_works() -> Result<()> {
    let savfox_home = TempDir::new()?;
    write_models_cache(savfox_home.path())?;
    let mut mcp = McpProcess::new(savfox_home.path()).await?;

    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let expected_models = expected_visible_models();
    let mut cursor: Option<String> = None;
    let mut items: Vec<Model> = Vec::new();

    for _ in 0..expected_models.len() {
        let request_id = mcp
            .send_list_models_request(ModelListParams {
                limit: Some(1),
                cursor: cursor.clone(),
            })
            .await?;

        let response: JSONRPCResponse = timeout(
            DEFAULT_TIMEOUT,
            mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
        )
        .await??;

        let ModelListResponse {
            data: page_items,
            next_cursor,
        } = to_response::<ModelListResponse>(response)?;

        assert_eq!(page_items.len(), 1);
        items.extend(page_items);

        if let Some(next_cursor) = next_cursor {
            cursor = Some(next_cursor);
        } else {
            assert_eq!(items, expected_models);
            return Ok(());
        }
    }

    Err(anyhow!(
        "model pagination did not terminate after {} pages",
        expected_models.len()
    ))
}

#[tokio::test]
async fn list_models_rejects_invalid_cursor() -> Result<()> {
    let savfox_home = TempDir::new()?;
    write_models_cache(savfox_home.path())?;
    let mut mcp = McpProcess::new(savfox_home.path()).await?;

    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_list_models_request(ModelListParams {
            limit: None,
            cursor: Some("invalid".to_owned()),
        })
        .await?;

    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.id, RequestId::Integer(request_id));
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(error.error.message, "invalid cursor: invalid");
    Ok(())
}
