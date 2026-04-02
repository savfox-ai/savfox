use std::path::Path;

use savfox_config::types::RawMcpServerConfig;
use schemars::generate::SchemaSettings;
use schemars::{Schema, SchemaGenerator};
use serde_json::{Map, Value};

use crate::config::ConfigToml;
use crate::features::FEATURES;

/// Schema for the `[features]` map with known + legacy keys only.
pub(crate) fn features_schema(schema_gen: &mut SchemaGenerator) -> Schema {
    let mut properties = Map::new();
    for feature in FEATURES {
        let bool_schema = schema_gen.subschema_for::<bool>();
        properties.insert(feature.key.to_owned(), bool_schema.into());
    }
    for legacy_key in crate::features::legacy_feature_keys() {
        let bool_schema = schema_gen.subschema_for::<bool>();
        properties.insert(legacy_key.to_owned(), bool_schema.into());
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    })
    .try_into()
    .expect("valid schema value")
}

/// Schema for the `[mcp_servers]` map using the raw input shape.
pub(crate) fn mcp_servers_schema(schema_gen: &mut SchemaGenerator) -> Schema {
    let additional: Value = schema_gen.subschema_for::<RawMcpServerConfig>().into();
    serde_json::json!({
        "type": "object",
        "additionalProperties": additional,
    })
    .try_into()
    .expect("valid schema value")
}

/// Build the config schema for `config.toml`.
#[must_use]
pub fn config_schema() -> Schema {
    SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<ConfigToml>()
}

/// Canonicalize a JSON value by sorting its keys.
fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let mut sorted = Map::with_capacity(map.len());
            for (key, child) in entries {
                sorted.insert(key.clone(), canonicalize(child));
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

/// Render the config schema as pretty-printed JSON.
pub fn config_schema_json() -> anyhow::Result<Vec<u8>> {
    let schema = config_schema();
    let value: Value = schema.into();
    let value = canonicalize(&value);
    let json = serde_json::to_vec_pretty(&value)?;
    Ok(json)
}

/// Write the config schema fixture to disk.
pub fn write_config_schema(out_path: &Path) -> anyhow::Result<()> {
    let json = config_schema_json()?;
    std::fs::write(out_path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use similar::TextDiff;

    use super::{canonicalize, config_schema_json};

    #[test]
    fn config_schema_matches_fixture() {
        let fixture_path = savfox_utils::cargo_bin::find_resource!("config.schema.json")
            .expect("resolve config schema fixture path");
        let fixture = std::fs::read_to_string(fixture_path).expect("read config schema fixture");
        let fixture_value: serde_json::Value =
            serde_json::from_str(&fixture).expect("parse config schema fixture");
        let schema_json = config_schema_json().expect("serialize config schema");
        let schema_value: serde_json::Value =
            serde_json::from_slice(&schema_json).expect("decode schema json");
        let fixture_value = canonicalize(&fixture_value);
        let schema_value = canonicalize(&schema_value);
        if fixture_value != schema_value {
            let expected =
                serde_json::to_string_pretty(&fixture_value).expect("serialize fixture json");
            let actual =
                serde_json::to_string_pretty(&schema_value).expect("serialize schema json");
            let diff = TextDiff::from_lines(&expected, &actual)
                .unified_diff()
                .header("fixture", "generated")
                .to_string();
            panic!(
                "Current schema for `config.toml` doesn't match the fixture. \
Run `just write-config-schema` to overwrite with your changes.\n\n{diff}"
            );
        }
    }
}
