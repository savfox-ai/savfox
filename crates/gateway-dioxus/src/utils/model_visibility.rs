use std::collections::HashMap;

use serde::{Deserialize, Serialize};

const STORAGE_KEY: &str = "savfox_model_preferences_v2";

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelKey {
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
}

impl ModelKey {
    pub fn new(provider_id: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisibilityState {
    Show,
    Hide,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserPreference {
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
    pub visibility: VisibilityState,
    pub favorite: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModelPreferences {
    pub user: Vec<UserPreference>,
    pub recent: Vec<ModelKey>,
    #[serde(default)]
    pub variant: HashMap<String, Option<String>>,
}

pub fn load_model_preferences() -> ModelPreferences {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(storage) = web_sys::window()
            .and_then(|w| w.local_storage().ok())
            .flatten()
        else {
            return ModelPreferences::default();
        };

        if let Ok(Some(raw)) = storage.get_item(STORAGE_KEY) {
            if let Ok(parsed) = serde_json::from_str::<ModelPreferences>(&raw) {
                return parsed;
            }
        }

        ModelPreferences::default()
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        ModelPreferences::default()
    }
}

pub fn save_model_preferences(prefs: &ModelPreferences) {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(storage) = web_sys::window()
            .and_then(|w| w.local_storage().ok())
            .flatten()
        else {
            return;
        };
        if let Ok(raw) = serde_json::to_string(prefs) {
            let _ = storage.set_item(STORAGE_KEY, &raw);
        }
    }
}

pub fn is_model_visible(prefs: &ModelPreferences, key: &ModelKey) -> bool {
    prefs
        .user
        .iter()
        .find(|entry| entry.provider_id == key.provider_id && entry.model_id == key.model_id)
        .map(|entry| entry.visibility == VisibilityState::Show)
        .unwrap_or(true)
}

pub fn set_model_visibility(prefs: &mut ModelPreferences, key: &ModelKey, visible: bool) {
    if let Some(entry) = prefs
        .user
        .iter_mut()
        .find(|entry| entry.provider_id == key.provider_id && entry.model_id == key.model_id)
    {
        entry.visibility = if visible {
            VisibilityState::Show
        } else {
            VisibilityState::Hide
        };
        return;
    }

    prefs.user.push(UserPreference {
        provider_id: key.provider_id.clone(),
        model_id: key.model_id.clone(),
        visibility: if visible {
            VisibilityState::Show
        } else {
            VisibilityState::Hide
        },
        favorite: None,
    });
}

pub fn push_recent_model(prefs: &mut ModelPreferences, key: &ModelKey, limit: usize) {
    prefs
        .recent
        .retain(|entry| !(entry.provider_id == key.provider_id && entry.model_id == key.model_id));
    prefs.recent.insert(0, key.clone());
    if prefs.recent.len() > limit {
        prefs.recent.truncate(limit);
    }
}
