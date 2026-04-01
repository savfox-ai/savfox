use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct VoiceSettings {
    pub enabled: Option<bool>,
    pub wake_word: Option<String>,
    pub sensitivity: Option<f64>,
}
