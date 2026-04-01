use std::collections::BTreeMap;

use super::JsonSchema;
use super::declarations::{FunctionToolDecl, function_tool};
use crate::client_common::tools::ToolSpec;

pub(super) fn create_sessions_list_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "filter".to_string(),
        JsonSchema::String {
            description: Some("Optional filter for session type.".to_string()),
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
            "session_id".to_string(),
            JsonSchema::String {
                description: Some("Session ID to get history for.".to_string()),
            },
        ),
        (
            "limit".to_string(),
            JsonSchema::Number {
                description: Some("Maximum number of messages to return (default 50).".to_string()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "sessions_history",
        description: "Get the message history for a specific session.",
        properties,
        required: &["session_id"],
    })
}

pub(super) fn create_sessions_send_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "session_id".to_string(),
            JsonSchema::String {
                description: Some("Session ID to send the message to.".to_string()),
            },
        ),
        (
            "message".to_string(),
            JsonSchema::String {
                description: Some("The message text to send.".to_string()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "sessions_send",
        description: "Send a message to another active session.",
        properties,
        required: &["session_id", "message"],
    })
}

pub(super) fn create_session_status_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "session_id".to_string(),
        JsonSchema::String {
            description: Some("Session ID to check status for.".to_string()),
        },
    )]);

    function_tool(FunctionToolDecl {
        name: "session_status",
        description: "Get metadata and status information for a specific session.",
        properties,
        required: &["session_id"],
    })
}

pub(super) fn create_list_mcp_resources_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "server".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional MCP server name. When omitted, lists resources from every configured server."
                        .to_string(),
                ),
            },
        ),
        (
            "cursor".to_string(),
            JsonSchema::String {
                description: Some(
                    "Opaque cursor returned by a previous list_mcp_resources call for the same server."
                        .to_string(),
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
            "server".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional MCP server name. When omitted, lists resource templates from all configured servers."
                        .to_string(),
                ),
            },
        ),
        (
            "cursor".to_string(),
            JsonSchema::String {
                description: Some(
                    "Opaque cursor returned by a previous list_mcp_resource_templates call for the same server."
                        .to_string(),
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
            "server".to_string(),
            JsonSchema::String {
                description: Some(
                    "MCP server name exactly as configured. Must match the 'server' field returned by list_mcp_resources."
                        .to_string(),
                ),
            },
        ),
        (
            "uri".to_string(),
            JsonSchema::String {
                description: Some(
                    "Resource URI to read. Must be one of the URIs returned by list_mcp_resources."
                        .to_string(),
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

pub(super) fn create_sessions_spawn_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "prompt".to_string(),
            JsonSchema::String {
                description: Some("The prompt/task for the spawned sub-agent.".to_string()),
            },
        ),
        (
            "model".to_string(),
            JsonSchema::String {
                description: Some("Optional model override for the sub-agent.".to_string()),
            },
        ),
        (
            "instructions".to_string(),
            JsonSchema::String {
                description: Some("Optional system instructions for the sub-agent.".to_string()),
            },
        ),
        (
            "timeout_secs".to_string(),
            JsonSchema::Number {
                description: Some("Timeout in seconds (default 300).".to_string()),
            },
        ),
        (
            "cleanup".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether to clean up the agent session on completion (default true)."
                        .to_string(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "sessions_spawn",
        description: "Spawn a new sub-agent session with an optional model override and custom instructions. The agent processes the given prompt and returns results.",
        properties,
        required: &["prompt"],
    })
}

pub(super) fn create_agent_step_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "session_id".to_string(),
            JsonSchema::String {
                description: Some("Session key or agent ID to invoke.".to_string()),
            },
        ),
        (
            "prompt".to_string(),
            JsonSchema::String {
                description: Some("The prompt or message to send to the agent.".to_string()),
            },
        ),
        (
            "system".to_string(),
            JsonSchema::String {
                description: Some("Optional system prompt override for this step.".to_string()),
            },
        ),
        (
            "model".to_string(),
            JsonSchema::String {
                description: Some("Optional model override.".to_string()),
            },
        ),
        (
            "timeout_secs".to_string(),
            JsonSchema::Number {
                description: Some("Maximum seconds to wait (default: 60).".to_string()),
            },
        ),
        (
            "history_limit".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Maximum history messages to scan for the reply (default: 50).".to_string(),
                ),
            },
        ),
        (
            "lane".to_string(),
            JsonSchema::String {
                description: Some("Agent lane (e.g., \"nested\" for sub-agent calls).".to_string()),
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

pub(super) fn create_sessions_send_a2a_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "source_session_id".to_string(),
            JsonSchema::String {
                description: Some("Source session/agent ID (the requester).".to_string()),
            },
        ),
        (
            "target_session_id".to_string(),
            JsonSchema::String {
                description: Some("Target session/agent ID (the responder).".to_string()),
            },
        ),
        (
            "message".to_string(),
            JsonSchema::String {
                description: Some("Initial message to send from source to target.".to_string()),
            },
        ),
        (
            "max_ping_pong_turns".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Maximum ping-pong turns between agents (0 = single shot, max 10).".to_string(),
                ),
            },
        ),
        (
            "announce".to_string(),
            JsonSchema::Boolean {
                description: Some("Whether to announce the final result to a channel.".to_string()),
            },
        ),
        (
            "announce_channel".to_string(),
            JsonSchema::String {
                description: Some(
                    "Channel to announce to (e.g., \"discord:channel_id\"). If omitted and \
                     `announce=true`, uses source session's recent active channel when available."
                        .to_string(),
                ),
            },
        ),
        (
            "target_system".to_string(),
            JsonSchema::String {
                description: Some("Optional system prompt for the target agent.".to_string()),
            },
        ),
        (
            "turn_timeout_secs".to_string(),
            JsonSchema::Number {
                description: Some("Timeout per turn in seconds (default: 60).".to_string()),
            },
        ),
        (
            "message_type".to_string(),
            JsonSchema::String {
                description: Some(
                    "A2A message type: \"request\" (expects response), \"response\" (reply to prior request), \
                     or \"notification\" (fire-and-forget). Defaults to \"request\"."
                        .to_string(),
                ),
            },
        ),
        (
            "correlation_id".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional correlation ID for request-response matching. Auto-generated for requests when omitted."
                        .to_string(),
                ),
            },
        ),
        (
            "timeout_ms".to_string(),
            JsonSchema::Number {
                description: Some("Optional per-message timeout in milliseconds.".to_string()),
            },
        ),
        (
            "delegation_purpose".to_string(),
            JsonSchema::String {
                description: Some(
                    "Human-readable description of why this delegation is occurring. Recorded in the delegation chain."
                        .to_string(),
                ),
            },
        ),
        (
            "delegation_chain".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String {
                    description: Some("Agent ID in the delegation chain.".to_string()),
                }),
                description: Some(
                    "Ordered list of agent IDs forming the delegation chain. The source agent is auto-appended."
                        .to_string(),
                ),
            },
        ),
        (
            "result_injection_format".to_string(),
            JsonSchema::String {
                description: Some(
                    "Shape of the returned result payload: \"summary\" (default) or \
                     \"full_transcript\"."
                        .to_string(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "sessions_send_a2a",
        description: "Agent-to-agent structured messaging: send a typed A2AMessage (request, response, or notification) from one agent session to another. Supports correlation IDs for request-response matching, delegation chain tracking, multi-turn ping-pong conversation, and channel announcements.",
        properties,
        required: &["source_session_id", "target_session_id", "message"],
    })
}
