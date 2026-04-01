use std::path::{Path, PathBuf};

use savfox_core::config::{CONFIG_JSON_FILE, CONFIG_TOML_FILE, CONFIG_YAML_FILE, CONFIG_YML_FILE};

const GATEWAY_DIR: &str = "gateway";

pub(crate) fn home_file(savfox_home: &Path, file_name: &str) -> PathBuf {
    savfox_home.join(file_name)
}

pub(crate) fn gateway_dir(savfox_home: &Path) -> PathBuf {
    savfox_home.join(GATEWAY_DIR)
}

pub(crate) fn gateway_file(savfox_home: &Path, file_name: &str) -> PathBuf {
    gateway_dir(savfox_home).join(file_name)
}

pub(crate) fn config_toml_path(savfox_home: &Path) -> PathBuf {
    home_file(savfox_home, CONFIG_TOML_FILE)
}

pub(crate) fn config_backup_path(savfox_home: &Path) -> PathBuf {
    home_file(savfox_home, "config.toml.bak")
}

pub(crate) fn config_candidates(savfox_home: &Path) -> [(&'static str, PathBuf); 4] {
    [
        ("toml", config_toml_path(savfox_home)),
        ("json", home_file(savfox_home, CONFIG_JSON_FILE)),
        ("yaml", home_file(savfox_home, CONFIG_YAML_FILE)),
        ("yaml", home_file(savfox_home, CONFIG_YML_FILE)),
    ]
}

pub(crate) fn exec_approval_policy_path(savfox_home: &Path) -> PathBuf {
    gateway_file(savfox_home, "exec-approval-policy.json")
}

pub(crate) fn heartbeat_config_path(savfox_home: &Path) -> PathBuf {
    home_file(savfox_home, "heartbeat-config.json")
}

pub(crate) fn hooks_config_path(savfox_home: &Path) -> PathBuf {
    home_file(savfox_home, "hooks-config.json")
}

pub(crate) fn log_rotation_config_path(savfox_home: &Path) -> PathBuf {
    home_file(savfox_home, "log-rotation-config.json")
}

pub(crate) fn stt_config_path(savfox_home: &Path) -> PathBuf {
    gateway_file(savfox_home, "stt-config.json")
}

pub(crate) fn streaming_config_path(savfox_home: &Path) -> PathBuf {
    home_file(savfox_home, "streaming-config.json")
}

pub(crate) fn talk_mode_config_path(savfox_home: &Path) -> PathBuf {
    gateway_file(savfox_home, "talk-mode-config.json")
}

pub(crate) fn tts_config_path(savfox_home: &Path) -> PathBuf {
    gateway_file(savfox_home, "tts-config.json")
}

pub(crate) fn tts_audio_dir(savfox_home: &Path) -> PathBuf {
    gateway_file(savfox_home, "tts-audio")
}

pub(crate) fn voice_wake_config_path(savfox_home: &Path) -> PathBuf {
    gateway_file(savfox_home, "voice-wake-config.json")
}
