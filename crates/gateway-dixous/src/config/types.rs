use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JsonSchema {
    #[serde(rename = "type")]
    pub schema_type: Option<String>,
    pub properties: Option<HashMap<String, SchemaProperty>>,
    pub required: Option<Vec<String>>,
    #[serde(default)]
    pub definitions: HashMap<String, SchemaDefinition>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaProperty {
    #[serde(rename = "type")]
    pub schema_type: Option<SchemaType>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
    #[serde(rename = "enum")]
    pub enum_values: Option<Vec<String>>,
    #[serde(default)]
    pub properties: Option<HashMap<String, SchemaProperty>>,
    #[serde(default)]
    pub items: Option<Box<SchemaProperty>>,
    #[serde(rename = "additionalProperties")]
    pub additional_properties: Option<Box<SchemaProperty>>,
    #[serde(default)]
    pub required: Option<Vec<String>>,
    #[serde(rename = "ui:label")]
    pub ui_label: Option<String>,
    #[serde(rename = "ui:help")]
    pub ui_help: Option<String>,
    #[serde(rename = "ui:group")]
    pub ui_group: Option<String>,
    #[serde(rename = "ui:order")]
    pub ui_order: Option<i32>,
    #[serde(rename = "ui:sensitive")]
    pub ui_sensitive: Option<bool>,
    #[serde(rename = "ui:multiline")]
    pub ui_multiline: Option<bool>,
    #[serde(rename = "ui:placeholder")]
    pub ui_placeholder: Option<String>,
    #[serde(rename = "ui:min")]
    pub ui_min: Option<f64>,
    #[serde(rename = "ui:max")]
    pub ui_max: Option<f64>,
    #[serde(rename = "ui:step")]
    pub ui_step: Option<f64>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SchemaType {
    Single(String),
    Multiple(Vec<String>),
}

impl SchemaType {
    pub fn as_str(&self) -> &str {
        match self {
            SchemaType::Single(s) => s.as_str(),
            SchemaType::Multiple(v) => v.first().map(|s| s.as_str()).unwrap_or("string"),
        }
    }

    pub fn is_array(&self) -> bool {
        self.as_str() == "array"
    }

    pub fn is_object(&self) -> bool {
        self.as_str() == "object"
    }

    pub fn is_string(&self) -> bool {
        self.as_str() == "string"
    }

    pub fn is_number(&self) -> bool {
        matches!(self.as_str(), "number" | "integer")
    }

    pub fn is_boolean(&self) -> bool {
        self.as_str() == "boolean"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaDefinition {
    #[serde(rename = "type")]
    pub schema_type: Option<String>,
    #[serde(default)]
    pub properties: HashMap<String, SchemaProperty>,
    pub required: Option<Vec<String>>,
}

impl Default for JsonSchema {
    fn default() -> Self {
        Self {
            schema_type: Some("object".to_string()),
            properties: Some(HashMap::new()),
            required: Some(Vec::new()),
            definitions: HashMap::new(),
            extra: HashMap::new(),
        }
    }
}
