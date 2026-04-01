use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct TtsStatus {
    pub enabled: Option<bool>,
    pub provider: Option<String>,
    pub voice: Option<String>,
    pub default_model: Option<String>,
    pub speed: Option<f64>,
    pub pitch: Option<f64>,
    pub active_provider_has_key: Option<bool>,
}
