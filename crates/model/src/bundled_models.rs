use savfox_protocol::openai_models::{ModelInfo, ModelsResponse};

pub fn bundled_models_json() -> &'static str {
    include_str!("../openai/models.json")
}

pub fn bundled_models_response() -> Result<ModelsResponse, serde_json::Error> {
    let contents = bundled_models_json().trim_start_matches('\u{feff}');
    serde_json::from_str(contents)
}

pub fn bundled_models() -> Result<Vec<ModelInfo>, serde_json::Error> {
    Ok(bundled_models_response()?.models)
}
