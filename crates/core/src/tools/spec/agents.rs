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
        "message".to_owned(),
        JsonSchema::String {
            description: Some(
                "Initial task for the new agent. Include scope, constraints, and the expected output.".to_owned(),
            ),
        },
    );
    properties.insert(
        "agent_type".to_owned(),
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
        "id".to_owned(),
        JsonSchema::String {
            description: Some("Agent id to message (from spawn_agent).".to_owned()),
        },
    );
    properties.insert(
        "message".to_owned(),
        JsonSchema::String {
            description: Some("Message to send to the agent.".to_owned()),
        },
    );
    properties.insert(
        "interrupt".to_owned(),
        JsonSchema::Boolean {
            description: Some(
                "When true, stop the agent's current task and handle this immediately. When false (default), queue this message.".to_owned(),
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
        "ids".to_owned(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: Some(
                "Agent ids to wait on. Pass multiple ids to wait for whichever finishes first."
                    .to_owned(),
            ),
        },
    );
    properties.insert(
        "timeout_ms".to_owned(),
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
        "label".to_owned(),
        JsonSchema::String {
            description: Some("User-facing label (1-5 words).".to_owned()),
        },
    );
    option_props.insert(
        "description".to_owned(),
        JsonSchema::String {
            description: Some(
                "One short sentence explaining impact/tradeoff if selected.".to_owned(),
            ),
        },
    );

    let options_schema = JsonSchema::Array {
        description: Some(
            "Provide 2-3 mutually exclusive choices. Put the recommended option first and suffix its label with \"(Recommended)\". Do not include an \"Other\" option in this list; the client will add a free-form \"Other\" option automatically.".to_owned(),
        ),
        items: Box::new(JsonSchema::Object {
            properties: option_props,
            required: Some(vec!["label".to_owned(), "description".to_owned()]),
            additional_properties: Some(false.into()),
        }),
    };

    let mut question_props = BTreeMap::new();
    question_props.insert(
        "id".to_owned(),
        JsonSchema::String {
            description: Some("Stable identifier for mapping answers (snake_case).".to_owned()),
        },
    );
    question_props.insert(
        "header".to_owned(),
        JsonSchema::String {
            description: Some("Short header label shown in the UI (12 or fewer chars).".to_owned()),
        },
    );
    question_props.insert(
        "question".to_owned(),
        JsonSchema::String {
            description: Some("Single-sentence prompt shown to the user.".to_owned()),
        },
    );
    question_props.insert("options".to_owned(), options_schema);

    let questions_schema = JsonSchema::Array {
        description: Some("Questions to show the user. Prefer 1 and do not exceed 3".to_owned()),
        items: Box::new(JsonSchema::Object {
            properties: question_props,
            required: Some(vec![
                "id".to_owned(),
                "header".to_owned(),
                "question".to_owned(),
                "options".to_owned(),
            ]),
            additional_properties: Some(false.into()),
        }),
    };

    let mut properties = BTreeMap::new();
    properties.insert("questions".to_owned(), questions_schema);

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
        "id".to_owned(),
        JsonSchema::String {
            description: Some("Agent id to close (from spawn_agent).".to_owned()),
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
            "id".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Identifier shared by concurrent calls that should rendezvous".to_owned(),
                ),
            },
        ),
        (
            "participants".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Number of tool calls that must arrive before the barrier opens".to_owned(),
                ),
            },
        ),
        (
            "timeout_ms".to_owned(),
            JsonSchema::Number {
                description: Some("Maximum time in milliseconds to wait at the barrier".to_owned()),
            },
        ),
    ]);

    let properties = BTreeMap::from([
        (
            "sleep_before_ms".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Optional delay in milliseconds before any other action".to_owned(),
                ),
            },
        ),
        (
            "sleep_after_ms".to_owned(),
            JsonSchema::Number {
                description: Some(
                    "Optional delay in milliseconds after completing the barrier".to_owned(),
                ),
            },
        ),
        (
            "barrier".to_owned(),
            JsonSchema::Object {
                properties: barrier_properties,
                required: Some(vec!["id".to_owned(), "participants".to_owned()]),
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
        "filter".to_owned(),
        JsonSchema::String {
            description: Some("Optional filter for agent status.".to_owned()),
        },
    )]);

    function_tool(FunctionToolDecl {
        name: "agents_list",
        description: "List all active agents and their status, model, and session information.",
        properties,
        required: &[],
    })
}
