use std::collections::BTreeMap;

use super::{AdditionalProperties, JsonSchema};
use crate::client_common::tools::{ResponsesApiTool, ToolSpec};

pub(super) struct FunctionToolDecl<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub properties: BTreeMap<String, JsonSchema>,
    pub required: &'a [&'a str],
}

pub(super) fn function_tool(decl: FunctionToolDecl<'_>) -> ToolSpec {
    let required = if decl.required.is_empty() {
        None
    } else {
        Some(
            decl.required
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        )
    };

    ToolSpec::Function(ResponsesApiTool {
        name: decl.name.to_string(),
        description: decl.description.to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties: decl.properties,
            required,
            additional_properties: Some(AdditionalProperties::Boolean(false)),
        },
    })
}
