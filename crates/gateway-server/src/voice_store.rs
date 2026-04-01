use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::json_store;

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
    savfox_home.join("voice-settings.json")
}

async fn load_settings(savfox_home: &Path) -> Result<VoiceSettings, String> {
    json_store::load_json(&store_path(savfox_home), "voice settings").await
}

async fn save_settings(savfox_home: &Path, settings: &VoiceSettings) -> Result<(), String> {
    json_store::save_json(&store_path(savfox_home), settings, "voice settings").await
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
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn voice_settings_roundtrip() {
        let tmp = tempdir().expect("tmpdir");
        let home = tmp.path().to_path_buf();
        let _ = set_talk_mode(&home, "voice").await.expect("set talk mode");
        let _ = set_voicewake(&home, true, "hey fox")
            .await
            .expect("set voicewake");
        let got = get_voicewake(&home).await.expect("get voicewake");
        assert_eq!(got.get("enabled").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(got.get("keyword").and_then(|v| v.as_str()), Some("hey fox"));
        assert!(home.join("voice-settings.json").is_file());
    }
}
