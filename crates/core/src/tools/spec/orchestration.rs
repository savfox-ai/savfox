use std::collections::BTreeMap;

use super::JsonSchema;
use super::declarations::{FunctionToolDecl, function_tool};
use crate::client_common::tools::ToolSpec;

pub(super) fn create_sessions_list_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "filter".to_owned(),
        JsonSchema::String {
            description: Some("Optional filter for session type.".to_owned()),
        },
    )]);

    function_tool(FunctionToolDecl {
        name: "sessions_list",
        description: "List active sessions connected to the gateway server.",
        properties,
        required: &[],
    })
}

pub(super) fn create_sessions_history_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "session_id".to_owned(),
            JsonSchema::String {
                description: Some("Session ID to get history for.".to_owned()),
            },
        ),
        (
            "limit".to_owned(),
            JsonSchema::Number {
                description: Some("Maximum number of messages to return (default 50).".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "sessions_history",
        description: "Get persisted chat history for a gateway session.",
        properties,
        required: &["session_id"],
    })
}

pub(super) fn create_session_status_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "session_id".to_owned(),
        JsonSchema::String {
            description: Some("Session ID to check status for.".to_owned()),
        },
    )]);

    function_tool(FunctionToolDecl {
        name: "session_status",
        description: "Check whether a gateway session is currently active.",
        properties,
        required: &["session_id"],
    })
}

pub(super) fn create_list_mcp_resources_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "server".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Optional MCP server name. When omitted, lists resources from every configured server.".to_owned(),
                ),
            },
        ),
        (
            "cursor".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Opaque cursor returned by a previous list_mcp_resources call for the same server.".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "list_mcp_resources",
        description: "Lists resources provided by MCP servers. Resources allow servers to share data that provides context to language models, such as files, database schemas, or application-specific information. Prefer resources over web search when possible.",
        properties,
        required: &[],
    })
}

pub(super) fn create_list_mcp_resource_templates_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "server".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Optional MCP server name. When omitted, lists resource templates from all configured servers.".to_owned(),
                ),
            },
        ),
        (
            "cursor".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Opaque cursor returned by a previous list_mcp_resource_templates call for the same server.".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "list_mcp_resource_templates",
        description: "Lists resource templates provided by MCP servers. Parameterized resource templates allow servers to share data that takes parameters and provides context to language models, such as files, database schemas, or application-specific information. Prefer resource templates over web search when possible.",
        properties,
        required: &[],
    })
}

pub(super) fn create_read_mcp_resource_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "server".to_owned(),
            JsonSchema::String {
                description: Some(
                    "MCP server name exactly as configured. Must match the 'server' field returned by list_mcp_resources.".to_owned(),
                ),
            },
        ),
        (
            "uri".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Resource URI to read. Must be one of the URIs returned by list_mcp_resources.".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "read_mcp_resource",
        description: "Read a specific resource from an MCP server given the server name and resource URI.",
        properties,
        required: &["server", "uri"],
    })
}

pub(super) fn create_agent_step_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "session_id".to_owned(),
            JsonSchema::String {
                description: Some("Session key or agent ID to invoke.".to_owned()),
            },
        ),
        (
            "prompt".to_owned(),
            JsonSchema::String {
                description: Some("The prompt or message to send to the agent.".to_owned()),
            },
        ),
        (
            "system".to_owned(),
            JsonSchema::String {
                description: Some("Optional system prompt override for this step.".to_owned()),
            },
        ),
        (
            "model".to_owned(),
            JsonSchema::String {
                description: Some("Optional model override.".to_owned()),
            },
        ),
        (
            "timeout_secs".to_owned(),
            JsonSchema::Number {
                description: Some("Maximum seconds to wait (default: 60).".to_owned()),
            },
        ),
        (
            "history_limit".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Maximum history messages to scan for the reply (default: 50).".to_owned(),
                ),
            },
        ),
        (
            "lane".to_owned(),
            JsonSchema::String {
                description: Some("Agent lane (e.g., \"nested\" for sub-agent calls).".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "agent_step",
        description: "Run a single agent processing step: send a prompt to a session, wait for the agent to complete, and return the assistant's reply. Core building block for agent-to-agent orchestration.",
        properties,
        required: &["session_id", "prompt"],
    })
}
