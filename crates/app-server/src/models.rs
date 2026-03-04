use std::sync::Arc;

use savfox_app_server_protocol::{Model, ReasoningEffortOption};
use savfox_core::SessionManager;
use savfox_core::config::Config;
use savfox_core::models_manager::manager::RefreshStrategy;
use savfox_protocol::openai_models::{ModelPreset, ReasoningEffortPreset};

pub async fn supported_models(session_manager: Arc<SessionManager>, config: &Config) -> Vec<Model> {
    session_manager
        .list_models(config, RefreshStrategy::OnlineIfUncached)
        .await
        .into_iter()
        .filter(|preset| preset.show_in_picker)
        .map(model_from_preset)
        .collect()
}

fn model_from_preset(preset: ModelPreset) -> Model {
    Model {
        id: preset.id.to_string(),
        slug: preset.slug.to_string(),
        name: preset.name.to_string(),
        description: preset.description.to_string(),
        supported_reasoning_efforts: reasoning_efforts_from_preset(
            preset.supported_reasoning_efforts,
        ),
        default_reasoning_effort: preset.default_reasoning_effort,
        input_modalities: preset.input_modalities,
        supports_personality: preset.supports_personality,
        is_default: preset.is_default,
    }
}

fn reasoning_efforts_from_preset(
    efforts: Vec<ReasoningEffortPreset>,
) -> Vec<ReasoningEffortOption> {
    efforts
        .iter()
        .map(|preset| ReasoningEffortOption {
            reasoning_effort: preset.effort,
            description: preset.description.to_string(),
        })
        .collect()
}
