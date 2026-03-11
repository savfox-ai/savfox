use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CanvasMessage {
    Push {
        surface_id: String,
        component: serde_json::Value,
    },
    Reset {
        surface_id: String,
    },
    Action {
        name: String,
        surface_id: String,
        source_component_id: String,
        context: serde_json::Value,
    },
    StateUpdate {
        surface_id: String,
        component: Option<serde_json::Value>,
    },
    Error {
        message: String,
    },
}

impl CanvasMessage {
    pub fn push(surface_id: impl Into<String>, component: serde_json::Value) -> Self {
        Self::Push {
            surface_id: surface_id.into(),
            component,
        }
    }

    pub fn reset(surface_id: impl Into<String>) -> Self {
        Self::Reset {
            surface_id: surface_id.into(),
        }
    }

    pub fn action(
        name: impl Into<String>,
        surface_id: impl Into<String>,
        source_component_id: impl Into<String>,
        context: serde_json::Value,
    ) -> Self {
        Self::Action {
            name: name.into(),
            surface_id: surface_id.into(),
            source_component_id: source_component_id.into(),
            context,
        }
    }

    pub fn state_update(
        surface_id: impl Into<String>,
        component: Option<serde_json::Value>,
    ) -> Self {
        Self::StateUpdate {
            surface_id: surface_id.into(),
            component,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}
