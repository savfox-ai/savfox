pub mod conversation;
pub mod talk_mode;
pub mod turn_detection;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use conversation::{ConversationState, ConversationTurn};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub use talk_mode::TalkModeService;
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TalkModeState {
    Inactive,
    Listening,
    Processing,
    Speaking,
    Paused,
}

impl Default for TalkModeState {
    fn default() -> Self {
        Self::Inactive
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TalkModeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_voice")]
    pub default_voice: String,
    #[serde(default = "default_silence_threshold_ms")]
    pub silence_threshold_ms: u64,
    #[serde(default = "default_max_turn_duration_ms")]
    pub max_turn_duration_ms: u64,
    #[serde(default)]
    pub interruptible: bool,
    #[serde(default = "default_auto_tts")]
    pub auto_tts: bool,
}

fn default_voice() -> String {
    "alloy".to_string()
}

fn default_silence_threshold_ms() -> u64 {
    1000
}

fn default_max_turn_duration_ms() -> u64 {
    30000
}

fn default_auto_tts() -> bool {
    true
}

impl Default for TalkModeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_voice: default_voice(),
            silence_threshold_ms: default_silence_threshold_ms(),
            max_turn_duration_ms: default_max_turn_duration_ms(),
            interruptible: true,
            auto_tts: true,
        }
    }
}

fn config_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("talk-mode-config.json")
}

pub async fn load_config(savfox_home: &Path) -> Result<TalkModeConfig, String> {
    let path = config_path(savfox_home);
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(_) => return Ok(TalkModeConfig::default()),
    };
    serde_json::from_str(&content).map_err(|err| format!("failed to parse talk mode config: {err}"))
}

pub async fn save_config(savfox_home: &Path, config: &TalkModeConfig) -> Result<(), String> {
    let path = config_path(savfox_home);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("failed to create config dir: {err}"))?;
    }
    let content = serde_json::to_string_pretty(config)
        .map_err(|err| format!("failed to serialize config: {err}"))?;
    tokio::fs::write(path, content)
        .await
        .map_err(|err| format!("failed to write config: {err}"))
}

pub async fn status(savfox_home: &Path) -> Result<Value, String> {
    let config = load_config(savfox_home).await?;
    Ok(json!({
        "enabled": config.enabled,
        "default_voice": config.default_voice,
        "silence_threshold_ms": config.silence_threshold_ms,
        "max_turn_duration_ms": config.max_turn_duration_ms,
        "interruptible": config.interruptible,
        "auto_tts": config.auto_tts,
    }))
}

pub async fn enable(
    savfox_home: &Path,
    voice: Option<&str>,
    silence_threshold_ms: Option<u64>,
    auto_tts: Option<bool>,
) -> Result<Value, String> {
    let mut config = load_config(savfox_home).await?;
    config.enabled = true;
    if let Some(v) = voice {
        config.default_voice = v.to_string();
    }
    if let Some(t) = silence_threshold_ms {
        config.silence_threshold_ms = t;
    }
    if let Some(a) = auto_tts {
        config.auto_tts = a;
    }
    save_config(savfox_home, &config).await?;
    info!("Talk mode enabled with voice: {}", config.default_voice);
    Ok(json!({
        "status": "enabled",
        "default_voice": config.default_voice,
    }))
}

pub async fn disable(savfox_home: &Path) -> Result<Value, String> {
    let mut config = load_config(savfox_home).await?;
    config.enabled = false;
    save_config(savfox_home, &config).await?;
    info!("Talk mode disabled");
    Ok(json!({ "status": "disabled" }))
}
