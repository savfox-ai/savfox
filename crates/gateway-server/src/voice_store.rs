use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VoiceSettings {
    #[serde(default = "default_talk_mode")]
    talk_mode: String,
    #[serde(default)]
    voicewake_enabled: bool,
    #[serde(default = "default_keyword")]
    voicewake_keyword: String,
}

fn default_talk_mode() -> String {
    "text".to_string()
}

fn default_keyword() -> String {
    "hey savfox".to_string()
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            talk_mode: default_talk_mode(),
            voicewake_enabled: false,
            voicewake_keyword: default_keyword(),
        }
    }
}

fn store_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("voice-settings.json")
}

async fn load_settings(savfox_home: &Path) -> Result<VoiceSettings, String> {
    let path = store_path(savfox_home);
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(_) => return Ok(VoiceSettings::default()),
    };
    serde_json::from_str(&content).map_err(|err| format!("failed to parse voice settings: {err}"))
}

async fn save_settings(savfox_home: &Path, settings: &VoiceSettings) -> Result<(), String> {
    let path = store_path(savfox_home);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("failed to create voice settings dir: {err}"))?;
    }
    let content = serde_json::to_string_pretty(settings)
        .map_err(|err| format!("failed to serialize voice settings: {err}"))?;
    tokio::fs::write(path, content)
        .await
        .map_err(|err| format!("failed to write voice settings: {err}"))
}

pub(crate) async fn set_talk_mode(savfox_home: &Path, mode: &str) -> Result<Value, String> {
    let mut settings = load_settings(savfox_home).await?;
    settings.talk_mode = mode.to_owned();
    save_settings(savfox_home, &settings).await?;
    Ok(json!({ "mode": settings.talk_mode }))
}

pub(crate) async fn get_voicewake(savfox_home: &Path) -> Result<Value, String> {
    let settings = load_settings(savfox_home).await?;
    Ok(json!({
        "enabled": settings.voicewake_enabled,
        "keyword": settings.voicewake_keyword,
    }))
}

pub(crate) async fn set_voicewake(
    savfox_home: &Path,
    enabled: bool,
    keyword: &str,
) -> Result<Value, String> {
    let mut settings = load_settings(savfox_home).await?;
    settings.voicewake_enabled = enabled;
    settings.voicewake_keyword = keyword.to_owned();
    save_settings(savfox_home, &settings).await?;
    Ok(json!({
        "enabled": settings.voicewake_enabled,
        "keyword": settings.voicewake_keyword,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn voice_settings_roundtrip() {
        let home = std::env::temp_dir().join(format!("savfox-voice-test-{}", uuid::Uuid::now_v7()));
        let _ = set_talk_mode(&home, "voice").await.expect("set talk mode");
        let _ = set_voicewake(&home, true, "hey fox")
            .await
            .expect("set voicewake");
        let got = get_voicewake(&home).await.expect("get voicewake");
        assert_eq!(got.get("enabled").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(got.get("keyword").and_then(|v| v.as_str()), Some("hey fox"));
        let _ = tokio::fs::remove_dir_all(home).await;
    }
}
