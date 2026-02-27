use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiSettings {
    #[serde(rename = "gatewayUrl")]
    pub gateway_url: Option<String>,
    pub token: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    #[serde(rename = "lastActiveSessionId", alias = "lastActiveSessionKey")]
    pub last_active_session_id: Option<String>,
    pub theme: ThemeMode,
    #[serde(rename = "chatFocusMode")]
    pub chat_focus_mode: bool,
    #[serde(rename = "chatShowThinking")]
    pub chat_show_thinking: bool,
    #[serde(rename = "splitRatio")]
    pub split_ratio: f32,
    #[serde(rename = "navCollapsed")]
    pub nav_collapsed: bool,
    #[serde(rename = "navGroupsCollapsed")]
    pub nav_groups_collapsed: HashMap<String, bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

impl UiSettings {
    pub fn new() -> Self {
        Self {
            gateway_url: None,
            token: None,
            session_id: None,
            last_active_session_id: None,
            theme: ThemeMode::default(),
            chat_focus_mode: false,
            chat_show_thinking: true,
            split_ratio: 0.5,
            nav_collapsed: false,
            nav_groups_collapsed: HashMap::new(),
        }
    }
}

impl ThemeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThemeMode::System => "system",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "light" => ThemeMode::Light,
            "dark" => ThemeMode::Dark,
            _ => ThemeMode::System,
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_storage {
    use super::*;
    use web_sys::Storage;

    fn get_local_storage() -> Option<Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    const SETTINGS_KEY: &str = "savfox_settings";

    pub fn load_settings() -> UiSettings {
        let storage = match get_local_storage() {
            Some(s) => s,
            None => return UiSettings::new(),
        };

        let json = match storage.get_item(SETTINGS_KEY) {
            Ok(Some(s)) => s,
            _ => return UiSettings::new(),
        };

        serde_json::from_str(&json).unwrap_or_else(|_| UiSettings::new())
    }

    pub fn save_settings(settings: &UiSettings) {
        let storage = match get_local_storage() {
            Some(s) => s,
            None => return,
        };

        if let Ok(json) = serde_json::to_string(settings) {
            let _ = storage.set_item(SETTINGS_KEY, &json);
        }
    }

    pub fn clear_settings() {
        let storage = match get_local_storage() {
            Some(s) => s,
            None => return,
        };

        let _ = storage.remove_item(SETTINGS_KEY);
    }

    pub fn get_token() -> Option<String> {
        load_settings().token
    }

    pub fn set_token(token: &str) {
        let mut settings = load_settings();
        settings.token = Some(token.to_string());
        save_settings(&settings);
    }

    pub fn clear_token() {
        let mut settings = load_settings();
        settings.token = None;
        save_settings(&settings);
    }

    pub fn get_theme() -> ThemeMode {
        load_settings().theme
    }

    pub fn set_theme(theme: ThemeMode) {
        let mut settings = load_settings();
        settings.theme = theme;
        save_settings(&settings);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native_storage {
    use super::*;

    pub fn load_settings() -> UiSettings {
        UiSettings::new()
    }

    pub fn save_settings(_settings: &UiSettings) {}

    pub fn clear_settings() {}

    pub fn get_token() -> Option<String> {
        None
    }

    pub fn set_token(_token: &str) {}

    pub fn clear_token() {}

    pub fn get_theme() -> ThemeMode {
        ThemeMode::default()
    }

    pub fn set_theme(_theme: ThemeMode) {}
}

#[cfg(target_arch = "wasm32")]
#[allow(unused_imports)]
pub use wasm_storage::*;

#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_imports)]
pub use native_storage::{
    clear_settings, clear_token, get_theme, get_token, load_settings, save_settings, set_theme,
    set_token,
};
