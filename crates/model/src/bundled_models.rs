use savfox_protocol::openai_models::{ModelInfo, ModelsResponse};

pub fn bundled_models_json() -> &'static str {
    include_str!("../openai/models.json")
}

pub fn bundled_models_response() -> Result<ModelsResponse, serde_json::Error> {
    serde_json::from_str(bundled_models_json())
}

pub fn bundled_models() -> Result<Vec<ModelInfo>, serde_json::Error> {
    Ok(bundled_models_response()?.models)
}
