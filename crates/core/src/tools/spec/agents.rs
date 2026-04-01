use std::collections::BTreeMap;

use super::JsonSchema;
use super::declarations::{FunctionToolDecl, function_tool};
use crate::agent::AgentRole;
use crate::client_common::tools::ToolSpec;
use crate::tools::handlers::collab::{
    DEFAULT_WAIT_TIMEOUT_MS, MAX_WAIT_TIMEOUT_MS, MIN_WAIT_TIMEOUT_MS,
};

pub(super) fn create_spawn_agent_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "message".to_string(),
        JsonSchema::String {
            description: Some(
                "Initial task for the new agent. Include scope, constraints, and the expected output."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "agent_type".to_string(),
        JsonSchema::String {
            description: Some(format!(
                "Optional agent type ({}). Use an explicit type when delegating.",
                AgentRole::enum_values().join(", ")
            )),
        },
    );

    function_tool(FunctionToolDecl {
        name: "spawn_agent",
        description: "Spawn a sub-agent for a well-scoped task. Returns the agent id to use to communicate with this agent.",
        properties,
        required: &["message"],
    })
}

pub(super) fn create_send_input_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "id".to_string(),
        JsonSchema::String {
            description: Some("Agent id to message (from spawn_agent).".to_string()),
        },
    );
    properties.insert(
        "message".to_string(),
        JsonSchema::String {
            description: Some("Message to send to the agent.".to_string()),
        },
    );
    properties.insert(
        "interrupt".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "When true, stop the agent's current task and handle this immediately. When false (default), queue this message."
                    .to_string(),
            ),
        },
    );

    function_tool(FunctionToolDecl {
        name: "send_input",
        description: "Send a message to an existing agent. Use interrupt=true to redirect work immediately.",
        properties,
        required: &["id", "message"],
    })
}

pub(super) fn create_wait_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "ids".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: Some(
                "Agent ids to wait on. Pass multiple ids to wait for whichever finishes first."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "timeout_ms".to_string(),
        JsonSchema::Number {
            description: Some(format!(
                "Optional timeout in milliseconds. Defaults to {DEFAULT_WAIT_TIMEOUT_MS}, min {MIN_WAIT_TIMEOUT_MS}, max {MAX_WAIT_TIMEOUT_MS}. Prefer longer waits (minutes) to avoid busy polling."
            )),
        },
    );

    function_tool(FunctionToolDecl {
        name: "wait",
        description: "Wait for agents to reach a final status. Completed statuses may include the agent's final message. Returns empty status when timed out.",
        properties,
        required: &["ids"],
    })
}

pub(super) fn create_request_user_input_tool() -> ToolSpec {
    let mut option_props = BTreeMap::new();
    option_props.insert(
        "label".to_string(),
        JsonSchema::String {
            description: Some("User-facing label (1-5 words).".to_string()),
        },
    );
    option_props.insert(
        "description".to_string(),
        JsonSchema::String {
            description: Some(
                "One short sentence explaining impact/tradeoff if selected.".to_string(),
            ),
        },
    );

    let options_schema = JsonSchema::Array {
        description: Some(
            "Provide 2-3 mutually exclusive choices. Put the recommended option first and suffix its label with \"(Recommended)\". Do not include an \"Other\" option in this list; the client will add a free-form \"Other\" option automatically."
                .to_string(),
        ),
        items: Box::new(JsonSchema::Object {
            properties: option_props,
            required: Some(vec!["label".to_string(), "description".to_string()]),
            additional_properties: Some(false.into()),
        }),
    };

    let mut question_props = BTreeMap::new();
    question_props.insert(
        "id".to_string(),
        JsonSchema::String {
            description: Some("Stable identifier for mapping answers (snake_case).".to_string()),
        },
    );
    question_props.insert(
        "header".to_string(),
        JsonSchema::String {
            description: Some(
                "Short header label shown in the UI (12 or fewer chars).".to_string(),
            ),
        },
    );
    question_props.insert(
        "question".to_string(),
        JsonSchema::String {
            description: Some("Single-sentence prompt shown to the user.".to_string()),
        },
    );
    question_props.insert("options".to_string(), options_schema);

    let questions_schema = JsonSchema::Array {
        description: Some("Questions to show the user. Prefer 1 and do not exceed 3".to_string()),
        items: Box::new(JsonSchema::Object {
            properties: question_props,
            required: Some(vec![
                "id".to_string(),
                "header".to_string(),
                "question".to_string(),
                "options".to_string(),
            ]),
            additional_properties: Some(false.into()),
        }),
    };

    let mut properties = BTreeMap::new();
    properties.insert("questions".to_string(), questions_schema);

    function_tool(FunctionToolDecl {
        name: "request_user_input",
        description: "Request user input for one to three short questions and wait for the response.",
        properties,
        required: &["questions"],
    })
}

pub(super) fn create_close_agent_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "id".to_string(),
        JsonSchema::String {
            description: Some("Agent id to close (from spawn_agent).".to_string()),
        },
    );

    function_tool(FunctionToolDecl {
        name: "close_agent",
        description: "Close an agent when it is no longer needed and return its last known status.",
        properties,
        required: &["id"],
    })
}

pub(super) fn create_test_sync_tool() -> ToolSpec {
    let barrier_properties = BTreeMap::from([
        (
            "id".to_string(),
            JsonSchema::String {
                description: Some(
                    "Identifier shared by concurrent calls that should rendezvous".to_string(),
                ),
            },
        ),
        (
            "participants".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Number of tool calls that must arrive before the barrier opens".to_string(),
                ),
            },
        ),
        (
            "timeout_ms".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Maximum time in milliseconds to wait at the barrier".to_string(),
                ),
            },
        ),
    ]);

    let properties = BTreeMap::from([
        (
            "sleep_before_ms".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Optional delay in milliseconds before any other action".to_string(),
                ),
            },
        ),
        (
            "sleep_after_ms".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Optional delay in milliseconds after completing the barrier".to_string(),
                ),
            },
        ),
        (
            "barrier".to_string(),
            JsonSchema::Object {
                properties: barrier_properties,
                required: Some(vec!["id".to_string(), "participants".to_string()]),
                additional_properties: Some(false.into()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "test_sync_tool",
        description: "Internal synchronization helper used by Savfox integration tests.",
        properties,
        required: &[],
    })
}

pub(super) fn create_agents_list_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "filter".to_string(),
        JsonSchema::String {
            description: Some("Optional filter for agent status.".to_string()),
        },
    )]);

    function_tool(FunctionToolDecl {
        name: "agents_list",
        description: "List all active agents and their status, model, and session information.",
        properties,
        required: &[],
    })
}
