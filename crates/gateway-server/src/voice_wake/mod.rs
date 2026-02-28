pub mod audio_capture;
pub mod detector;
pub mod wake_word;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use detector::WakeWordDetectorService;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast};
use tracing::{info, warn};
pub use wake_word::{WakeWordDetector, WakeWordEvent};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceWakeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_keyword")]
    pub keyword: String,
    #[serde(default = "default_sensitivity")]
    pub sensitivity: f32,
    #[serde(default)]
    pub auto_start_talk_mode: bool,
}

fn default_keyword() -> String {
    "hey savfox".to_string()
}

fn default_sensitivity() -> f32 {
    0.5
}

impl Default for VoiceWakeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            keyword: default_keyword(),
            sensitivity: default_sensitivity(),
            auto_start_talk_mode: true,
        }
    }
}

fn config_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("voice-wake-config.json")
}

pub async fn load_config(savfox_home: &Path) -> Result<VoiceWakeConfig, String> {
    let path = config_path(savfox_home);
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(_) => return Ok(VoiceWakeConfig::default()),
    };
    serde_json::from_str(&content)
        .map_err(|err| format!("failed to parse voice wake config: {err}"))
}

pub async fn save_config(savfox_home: &Path, config: &VoiceWakeConfig) -> Result<(), String> {
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
        "keyword": config.keyword,
        "sensitivity": config.sensitivity,
        "auto_start_talk_mode": config.auto_start_talk_mode,
    }))
}

pub async fn enable(
    savfox_home: &Path,
    keyword: Option<&str>,
    sensitivity: Option<f32>,
    auto_start_talk_mode: Option<bool>,
) -> Result<Value, String> {
    let mut config = load_config(savfox_home).await?;
    config.enabled = true;
    if let Some(kw) = keyword {
        config.keyword = kw.to_lowercase();
    }
    if let Some(s) = sensitivity {
        config.sensitivity = s.clamp(0.0, 1.0);
    }
    if let Some(auto) = auto_start_talk_mode {
        config.auto_start_talk_mode = auto;
    }
    save_config(savfox_home, &config).await?;
    info!("Voice wake enabled with keyword: {}", config.keyword);
    Ok(json!({
        "status": "enabled",
        "keyword": config.keyword,
        "sensitivity": config.sensitivity,
    }))
}

pub async fn disable(savfox_home: &Path) -> Result<Value, String> {
    let mut config = load_config(savfox_home).await?;
    config.enabled = false;
    save_config(savfox_home, &config).await?;
    info!("Voice wake disabled");
    Ok(json!({ "status": "disabled" }))
}

pub async fn set_keyword(savfox_home: &Path, keyword: &str) -> Result<Value, String> {
    let mut config = load_config(savfox_home).await?;
    config.keyword = keyword.to_lowercase();
    save_config(savfox_home, &config).await?;
    Ok(json!({
        "status": "updated",
        "keyword": config.keyword,
    }))
}

/// Voice Wake Service that manages the detection loop.
pub struct VoiceWakeService {
    config: Arc<Mutex<VoiceWakeConfig>>,
    savfox_home: PathBuf,
    event_tx: broadcast::Sender<WakeWordEvent>,
    running: Arc<Mutex<bool>>,
}

impl VoiceWakeService {
    pub fn new(savfox_home: PathBuf) -> Self {
        let (event_tx, _) = broadcast::channel(16);
        Self {
            config: Arc::new(Mutex::new(VoiceWakeConfig::default())),
            savfox_home,
            event_tx,
            running: Arc::new(Mutex::new(false)),
        }
    }

    pub async fn init(&self) {
        if let Ok(config) = load_config(&self.savfox_home).await {
            *self.config.lock().await = config;
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WakeWordEvent> {
        self.event_tx.subscribe()
    }

    pub async fn start(&self) -> Result<(), String> {
        let mut running = self.running.lock().await;
        if *running {
            return Ok(());
        }

        let config = self.config.lock().await.clone();
        if !config.enabled {
            return Err("Voice wake is not enabled".to_string());
        }

        *running = true;
        info!(
            "Voice wake service started, listening for: {}",
            config.keyword
        );

        let running_clone = self.running.clone();
        let event_tx = self.event_tx.clone();
        let keyword = config.keyword.clone();
        let sensitivity = config.sensitivity;

        tokio::spawn(async move {
            let mut detector = WakeWordDetector::new(&keyword, sensitivity);

            while *running_clone.lock().await {
                match detector.detect().await {
                    Ok(Some(detected_keyword)) => {
                        info!("Wake word detected: {}", detected_keyword);
                        let _ = event_tx.send(WakeWordEvent::Detected {
                            keyword: detected_keyword,
                            timestamp: chrono::Utc::now().to_rfc3339(),
                        });
                    }
                    Ok(None) => {
                        // No wake word detected, continue listening
                    }
                    Err(err) => {
                        warn!("Wake word detection error: {}", err);
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        });

        Ok(())
    }

    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        *running = false;
        info!("Voice wake service stopped");
    }

    pub async fn is_running(&self) -> bool {
        *self.running.lock().await
    }
}
