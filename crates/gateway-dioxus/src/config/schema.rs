use std::collections::HashMap;

use crate::config::types::*;

#[derive(Clone, Debug, PartialEq)]
pub struct AnalyzedSchema {
    pub sections: Vec<SchemaSection>,
    pub fields: HashMap<String, SchemaField>,
    pub order: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SchemaSection {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub fields: Vec<String>,
    pub order: i32,
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SchemaField {
    pub path: String,
    pub field_type: FieldType,
    pub label: String,
    pub description: Option<String>,
    pub default: Option<serde_json::Value>,
    pub required: bool,
    pub sensitive: bool,
    pub placeholder: Option<String>,
    pub options: Option<Vec<String>>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub multiline: bool,
    pub order: i32,
    pub group: Option<String>,
    pub children: Option<Vec<SchemaField>>,
    pub item_type: Option<Box<FieldType>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldType {
    String,
    Number,
    Integer,
    Boolean,
    Enum(Vec<String>),
    Array(Box<FieldType>),
    Object(HashMap<String, SchemaField>),
    Text,
    Password,
    Secret,
    Color,
    Url,
    Email,
    Code(String), // language
    Unknown(String),
}

impl FieldType {
    pub fn from_str(s: &str) -> Self {
        match s {
            "string" => FieldType::String,
            "number" | "float" | "double" => FieldType::Number,
            "integer" | "int" | "int64" | "int32" => FieldType::Integer,
            "boolean" | "bool" => FieldType::Boolean,
            "text" | "multiline" => FieldType::Text,
            "password" | "secret" => FieldType::Password,
            "color" => FieldType::Color,
            "url" | "uri" => FieldType::Url,
            "email" => FieldType::Email,
            "array" => FieldType::Array(Box::new(FieldType::String)),
            "object" => FieldType::Object(HashMap::new()),
            other => FieldType::Unknown(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            FieldType::String => "string",
            FieldType::Number => "number",
            FieldType::Integer => "integer",
            FieldType::Boolean => "boolean",
            FieldType::Enum(_) => "enum",
            FieldType::Array(_) => "array",
            FieldType::Object(_) => "object",
            FieldType::Text => "text",
            FieldType::Password => "password",
            FieldType::Secret => "secret",
            FieldType::Color => "color",
            FieldType::Url => "url",
            FieldType::Email => "email",
            FieldType::Code(_) => "code",
            FieldType::Unknown(s) => s.as_str(),
        }
    }
}

pub struct SchemaAnalyzer {
    schema: JsonSchema,
}

impl SchemaAnalyzer {
    pub fn new(schema: JsonSchema) -> Self {
        Self { schema }
    }

    pub fn analyze(&self) -> AnalyzedSchema {
        let mut fields = HashMap::new();
        let mut sections: Vec<SchemaSection> = Vec::new();
        let mut order = Vec::new();

        if let Some(properties) = &self.schema.properties {
            let mut field_order: Vec<(String, &SchemaProperty)> =
                properties.iter().map(|(k, v)| (k.clone(), v)).collect();

            // Sort by ui:order
            field_order.sort_by(|a, b| {
                let order_a = a.1.ui_order.unwrap_or(999);
                let order_b = b.1.ui_order.unwrap_or(999);
                order_a.cmp(&order_b)
            });

            for (key, prop) in field_order {
                order.push(key.clone());
                let field = self.property_to_field(&key, prop);

                // Group by ui:group
                let group = prop.ui_group.clone();
                fields.insert(key.clone(), field);

                // Add to sections
                if let Some(group_name) = group {
                    if let Some(section) = sections.iter_mut().find(|s| s.id == group_name) {
                        section.fields.push(key.clone());
                    } else {
                        sections.push(SchemaSection {
                            id: group_name.clone(),
                            title: humanize(&group_name),
                            description: None,
                            fields: vec![key.clone()],
                            order: sections.len() as i32,
                            icon: icon_for_section(&group_name),
                        });
                    }
                }
            }
        }

        // Sort sections by order
        sections.sort_by_key(|section| section.order);

        AnalyzedSchema {
            sections,
            fields,
            order,
        }
    }

    fn property_to_field(&self, path: &str, prop: &SchemaProperty) -> SchemaField {
        let field_type = self.infer_field_type(prop);
        let required = self
            .schema
            .required
            .as_ref()
            .map(|r| r.contains(&path.to_string()))
            .unwrap_or(false);

        let children = if matches!(field_type, FieldType::Object(_)) {
            if let Some(nested) = &prop.properties {
                Some(
                    nested
                        .iter()
                        .map(|(k, v)| self.property_to_field(&format!("{}.{}", path, k), v))
                        .collect(),
                )
            } else {
                None
            }
        } else {
            None
        };

        let item_type = if let FieldType::Array(inner) = &field_type {
            Some(inner.clone())
        } else if let Some(items) = &prop.items {
            Some(Box::new(self.infer_field_type(items)))
        } else {
            None
        };

        SchemaField {
            path: path.to_string(),
            field_type,
            label: prop
                .ui_label
                .clone()
                .or(prop.title.clone())
                .unwrap_or_else(|| humanize(path)),
            description: prop.description.clone().or(prop.ui_help.clone()),
            default: prop.default.clone(),
            required,
            sensitive: prop.ui_sensitive.unwrap_or(false),
            placeholder: prop.ui_placeholder.clone(),
            options: prop.options.clone().or(prop.enum_values.clone()),
            min: prop.ui_min,
            max: prop.ui_max,
            step: prop.ui_step,
            multiline: prop.ui_multiline.unwrap_or(false),
            order: prop.ui_order.unwrap_or(999),
            group: prop.ui_group.clone(),
            children,
            item_type,
        }
    }

    fn infer_field_type(&self, prop: &SchemaProperty) -> FieldType {
        // Check for enum first
        if let Some(enum_vals) = &prop.enum_values {
            return FieldType::Enum(enum_vals.clone());
        }

        if let Some(options) = &prop.options {
            return FieldType::Enum(options.clone());
        }

        // Check for sensitive fields
        if prop.ui_sensitive.unwrap_or(false) {
            if prop.ui_multiline.unwrap_or(false) {
                return FieldType::Secret;
            }
            return FieldType::Password;
        }

        // Check for multiline text
        if prop.ui_multiline.unwrap_or(false) {
            return FieldType::Text;
        }

        // Infer from type
        if let Some(schema_type) = &prop.schema_type {
            match schema_type {
                SchemaType::Single(t) => FieldType::from_str(t.as_str()),
                SchemaType::Multiple(types) => {
                    // Use first non-null type
                    for t in types {
                        if t != "null" {
                            return FieldType::from_str(t.as_str());
                        }
                    }
                    FieldType::Unknown("multiple".to_string())
                }
            }
        } else {
            FieldType::Unknown("unknown".to_string())
        }
    }
}

pub fn humanize(s: &str) -> String {
    s.replace('_', " ")
        .replace('-', " ")
        .replace('.', " > ")
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn icon_for_section(section: &str) -> Option<String> {
    match section.to_lowercase().as_str() {
        "environment" | "env" => Some("settings".to_string()),
        "updates" => Some("refresh-cw".to_string()),
        "agents" => Some("users".to_string()),
        "authentication" | "auth" => Some("plug".to_string()),
        "channels" => Some("globe".to_string()),
        "messages" => Some("message-square".to_string()),
        "commands" => Some("wrench".to_string()),
        "hooks" => Some("link".to_string()),
        "skills" => Some("puzzle".to_string()),
        "tools" => Some("wrench".to_string()),
        "gateway" => Some("monitor".to_string()),
        "wizard" => Some("book".to_string()),
        "logging" | "logs" => Some("scroll-text".to_string()),
        "browser" => Some("globe".to_string()),
        "models" => Some("brain".to_string()),
        "audio" | "tts" => Some("radio".to_string()),
        "cron" => Some("clock".to_string()),
        "session" => Some("database".to_string()),
        "canvas" => Some("image".to_string()),
        "talk" => Some("radio".to_string()),
        "plugins" => Some("puzzle".to_string()),
        "memory" => Some("book".to_string()),
        _ => None,
    }
}

pub fn analyze_schema(schema: JsonSchema) -> AnalyzedSchema {
    SchemaAnalyzer::new(schema).analyze()
}
