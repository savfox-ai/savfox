use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2UIComponent {
    #[serde(rename = "type")]
    pub component_type: String,
    pub id: String,
    #[serde(default)]
    pub props: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<A2UIComponent>,
}

impl A2UIComponent {
    pub fn text(id: impl Into<String>, content: impl Into<String>) -> Self {
        let mut props = HashMap::new();
        props.insert("content".to_string(), serde_json::json!(content.into()));
        Self {
            component_type: "text".to_string(),
            id: id.into(),
            props,
            children: Vec::new(),
        }
    }

    pub fn heading(id: impl Into<String>, level: u8, content: impl Into<String>) -> Self {
        let mut props = HashMap::new();
        props.insert("level".to_string(), serde_json::json!(level));
        props.insert("content".to_string(), serde_json::json!(content.into()));
        Self {
            component_type: "heading".to_string(),
            id: id.into(),
            props,
            children: Vec::new(),
        }
    }

    pub fn button(
        id: impl Into<String>,
        label: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        let mut props = HashMap::new();
        props.insert("label".to_string(), serde_json::json!(label.into()));
        props.insert("action".to_string(), serde_json::json!(action.into()));
        Self {
            component_type: "button".to_string(),
            id: id.into(),
            props,
            children: Vec::new(),
        }
    }

    pub fn image(id: impl Into<String>, src: impl Into<String>, alt: Option<String>) -> Self {
        let mut props = HashMap::new();
        props.insert("src".to_string(), serde_json::json!(src.into()));
        if let Some(alt) = alt {
            props.insert("alt".to_string(), serde_json::json!(alt));
        }
        Self {
            component_type: "image".to_string(),
            id: id.into(),
            props,
            children: Vec::new(),
        }
    }

    pub fn card(id: impl Into<String>) -> Self {
        Self {
            component_type: "card".to_string(),
            id: id.into(),
            props: HashMap::new(),
            children: Vec::new(),
        }
    }

    pub fn list(id: impl Into<String>) -> Self {
        Self {
            component_type: "list".to_string(),
            id: id.into(),
            props: HashMap::new(),
            children: Vec::new(),
        }
    }

    pub fn input(id: impl Into<String>, placeholder: impl Into<String>) -> Self {
        let mut props = HashMap::new();
        props.insert(
            "placeholder".to_string(),
            serde_json::json!(placeholder.into()),
        );
        Self {
            component_type: "input".to_string(),
            id: id.into(),
            props,
            children: Vec::new(),
        }
    }

    pub fn child(mut self, child: A2UIComponent) -> Self {
        self.children.push(child);
        self
    }

    pub fn prop(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.props.insert(key.into(), value);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2UIAction {
    pub name: String,
    pub surface_id: String,
    pub source_component_id: String,
    #[serde(default)]
    pub context: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct A2UIState {
    pub surface_id: String,
    pub root: Option<A2UIComponent>,
    pub updated_at: u64,
}

impl A2UIState {
    pub fn new(surface_id: impl Into<String>) -> Self {
        Self {
            surface_id: surface_id.into(),
            root: None,
            updated_at: 0,
        }
    }

    pub fn set_root(&mut self, component: A2UIComponent) {
        self.root = Some(component);
        self.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
    }

    pub fn clear(&mut self) {
        self.root = None;
        self.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
    }
}
