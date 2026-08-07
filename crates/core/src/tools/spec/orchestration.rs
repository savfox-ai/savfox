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

pub(super) fn create_sessions_send_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "session_id".to_owned(),
            JsonSchema::String {
                description: Some("Session ID to send the message to.".to_owned()),
            },
        ),
        (
            "message".to_owned(),
            JsonSchema::String {
                description: Some("The message text to send.".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "sessions_send",
        description: "Placeholder: direct session-to-session messaging is not wired up yet and currently returns an error. Do not rely on it.",
        properties,
        required: &["session_id", "message"],
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

pub(super) fn create_sessions_spawn_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "prompt".to_owned(),
            JsonSchema::String {
                description: Some("The prompt/task for the spawned sub-agent.".to_owned()),
            },
        ),
        (
            "model".to_owned(),
            JsonSchema::String {
                description: Some("Optional model override for the sub-agent.".to_owned()),
            },
        ),
        (
            "instructions".to_owned(),
            JsonSchema::String {
                description: Some("Optional system instructions for the sub-agent.".to_owned()),
            },
        ),
        (
            "timeout_secs".to_owned(),
            JsonSchema::Number {
                description: Some("Timeout in seconds (default 300).".to_owned()),
            },
        ),
        (
            "cleanup".to_owned(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether to clean up the agent session on completion (default true)."
                        .to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "sessions_spawn",
        description: "Placeholder: sub-agent spawning is not wired up yet and currently returns an error. Do not rely on it.",
        properties,
        required: &["prompt"],
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

pub(super) fn create_sessions_send_a2a_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "source_session_id".to_owned(),
            JsonSchema::String {
                description: Some("Source session/agent ID (the requester).".to_owned()),
            },
        ),
        (
            "target_session_id".to_owned(),
            JsonSchema::String {
                description: Some("Target session/agent ID (the responder).".to_owned()),
            },
        ),
        (
            "message".to_owned(),
            JsonSchema::String {
                description: Some("Initial message to send from source to target.".to_owned()),
            },
        ),
        (
            "max_ping_pong_turns".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Maximum ping-pong turns between agents (0 = single shot, max 10).".to_owned(),
                ),
            },
        ),
        (
            "announce".to_owned(),
            JsonSchema::Boolean {
                description: Some("Whether to announce the final result to a channel.".to_owned()),
            },
        ),
        (
            "announce_channel".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Channel to announce to (e.g., \"discord:channel_id\"). If omitted and \
                     `announce=true`, uses source session's recent active channel when available.".to_owned(),
                ),
            },
        ),
        (
            "target_system".to_owned(),
            JsonSchema::String {
                description: Some("Optional system prompt for the target agent.".to_owned()),
            },
        ),
        (
            "turn_timeout_secs".to_owned(),
            JsonSchema::Number {
                description: Some("Timeout per turn in seconds (default: 60).".to_owned()),
            },
        ),
        (
            "message_type".to_owned(),
            JsonSchema::String {
                description: Some(
                    "A2A message type: \"request\" (expects response), \"response\" (reply to prior request), \
                     or \"notification\" (fire-and-forget). Defaults to \"request\".".to_owned(),
                ),
            },
        ),
        (
            "correlation_id".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Optional correlation ID for request-response matching. Auto-generated for requests when omitted.".to_owned(),
                ),
            },
        ),
        (
            "timeout_ms".to_owned(),
            JsonSchema::Number {
                description: Some("Optional per-message timeout in milliseconds.".to_owned()),
            },
        ),
        (
            "delegation_purpose".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Human-readable description of why this delegation is occurring. Recorded in the delegation chain.".to_owned(),
                ),
            },
        ),
        (
            "delegation_chain".to_owned(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String {
                    description: Some("Agent ID in the delegation chain.".to_owned()),
                }),
                description: Some(
                    "Ordered list of agent IDs forming the delegation chain. The source agent is auto-appended.".to_owned(),
                ),
            },
        ),
        (
            "result_injection_format".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Shape of the returned result payload: \"summary\" (default) or \
                     \"full_transcript\".".to_owned(),
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
