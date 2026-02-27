use std::collections::{BTreeMap, HashMap};

use savfox_protocol::config_types::WebSearchMode;
use savfox_protocol::dynamic_tools::DynamicToolSpec;
use savfox_protocol::models::VIEW_IMAGE_TOOL_NAME;
use savfox_protocol::openai_models::{ApplyPatchToolType, ConfigShellToolType, ModelInfo};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::agent::AgentRole;
use crate::client_common::tools::{ResponsesApiTool, ToolSpec};
use crate::features::{Feature, Features};
use crate::tools::handlers::PLAN_TOOL;
use crate::tools::handlers::apply_patch::{
    create_apply_patch_freeform_tool, create_apply_patch_json_tool,
};
use crate::tools::handlers::collab::{
    DEFAULT_WAIT_TIMEOUT_MS, MAX_WAIT_TIMEOUT_MS, MIN_WAIT_TIMEOUT_MS,
};
use crate::tools::registry::ToolRegistryBuilder;

#[derive(Debug, Clone)]
pub(crate) struct ToolsConfig {
    pub shell_type: ConfigShellToolType,
    pub apply_patch_tool_type: Option<ApplyPatchToolType>,
    pub web_search_mode: Option<WebSearchMode>,
    pub collab_tools: bool,
    pub collaboration_modes_tools: bool,
    pub request_rule_enabled: bool,
    pub experimental_supported_tools: Vec<String>,
}

pub(crate) struct ToolsConfigParams<'a> {
    pub(crate) model_info: &'a ModelInfo,
    pub(crate) features: &'a Features,
    pub(crate) web_search_mode: Option<WebSearchMode>,
}

impl ToolsConfig {
    pub fn new(params: &ToolsConfigParams) -> Self {
        let ToolsConfigParams {
            model_info,
            features,
            web_search_mode,
        } = params;
        let include_apply_patch_tool = features.enabled(Feature::ApplyPatchFreeform);
        let include_collab_tools = features.enabled(Feature::Collab);
        let include_collaboration_modes_tools = features.enabled(Feature::CollaborationModes);
        let request_rule_enabled = features.enabled(Feature::RequestRule);

        let shell_type = if !features.enabled(Feature::ShellTool) {
            ConfigShellToolType::Disabled
        } else if features.enabled(Feature::UnifiedExec) {
            // If ConPTY not supported (for old Windows versions), fallback on ShellCommand.
            if savfox_utils_pty::conpty_supported() {
                ConfigShellToolType::UnifiedExec
            } else {
                ConfigShellToolType::ShellCommand
            }
        } else {
            model_info.shell_type
        };

        let apply_patch_tool_type = match model_info.apply_patch_tool_type {
            Some(ApplyPatchToolType::Freeform) => Some(ApplyPatchToolType::Freeform),
            Some(ApplyPatchToolType::Function) => Some(ApplyPatchToolType::Function),
            None => {
                if include_apply_patch_tool {
                    Some(ApplyPatchToolType::Freeform)
                } else {
                    None
                }
            }
        };

        Self {
            shell_type,
            apply_patch_tool_type,
            web_search_mode: *web_search_mode,
            collab_tools: include_collab_tools,
            collaboration_modes_tools: include_collaboration_modes_tools,
            request_rule_enabled,
            experimental_supported_tools: model_info.experimental_supported_tools.clone(),
        }
    }
}

/// Generic JSON‑Schema subset needed for our tool definitions
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum JsonSchema {
    Boolean {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    String {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// MCP schema allows "number" | "integer" for Number
    #[serde(alias = "integer")]
    Number {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Array {
        items: Box<JsonSchema>,

        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Object {
        properties: BTreeMap<String, JsonSchema>,
        #[serde(skip_serializing_if = "Option::is_none")]
        required: Option<Vec<String>>,
        #[serde(
            rename = "additionalProperties",
            skip_serializing_if = "Option::is_none"
        )]
        additional_properties: Option<AdditionalProperties>,
    },
}

/// Whether additional properties are allowed, and if so, any required schema
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AdditionalProperties {
    Boolean(bool),
    Schema(Box<JsonSchema>),
}

impl From<bool> for AdditionalProperties {
    fn from(b: bool) -> Self {
        Self::Boolean(b)
    }
}

impl From<JsonSchema> for AdditionalProperties {
    fn from(s: JsonSchema) -> Self {
        Self::Schema(Box::new(s))
    }
}

fn create_approval_parameters(include_prefix_rule: bool) -> BTreeMap<String, JsonSchema> {
    let mut properties = BTreeMap::from([
        (
            "sandbox_permissions".to_string(),
            JsonSchema::String {
                description: Some(
                    "Sandbox permissions for the command. Set to \"require_escalated\" to request running without sandbox restrictions; defaults to \"use_default\"."
                        .to_string(),
                ),
            },
        ),
        (
            "justification".to_string(),
            JsonSchema::String {
                description: Some(
                    r#"Only set if sandbox_permissions is \"require_escalated\". 
                    Request approval from the user to run this command outside the sandbox. 
                    Phrased as a simple question that summarizes the purpose of the 
                    command as it relates to the task at hand - e.g. 'Do you want to 
                    fetch and pull the latest version of this git branch?'"#
                    .to_string(),
                ),
            },
        ),
    ]);

    if include_prefix_rule {
        properties.insert(
            "prefix_rule".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some(
                    r#"Only specify when sandbox_permissions is `require_escalated`. 
                    Suggest a prefix command pattern that will allow you to fulfill similar requests from the user in the future.
                    Should be a short but reasonable prefix, e.g. [\"git\", \"pull\"] or [\"uv\", \"run\"] or [\"pytest\"]."#.to_string(),
                ),
            });
    }

    properties
}

fn create_exec_command_tool(include_prefix_rule: bool) -> ToolSpec {
    let mut properties = BTreeMap::from([
        (
            "cmd".to_string(),
            JsonSchema::String {
                description: Some("Shell command to execute.".to_string()),
            },
        ),
        (
            "workdir".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional working directory to run the command in; defaults to the turn cwd."
                        .to_string(),
                ),
            },
        ),
        (
            "shell".to_string(),
            JsonSchema::String {
                description: Some("Shell binary to launch. Defaults to the user's default shell.".to_string()),
            },
        ),
        (
            "login".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether to run the shell with -l/-i semantics. Defaults to true.".to_string(),
                ),
            },
        ),
        (
            "tty".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether to allocate a TTY for the command. Defaults to false (plain pipes); set to true to open a PTY and access TTY process."
                        .to_string(),
                ),
            }
        ),
        (
            "yield_time_ms".to_string(),
            JsonSchema::Number {
                description: Some(
                    "How long to wait (in milliseconds) for output before yielding.".to_string(),
                ),
            },
        ),
        (
            "max_output_tokens".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Maximum number of tokens to return. Excess output will be truncated."
                        .to_string(),
                ),
            },
        ),
    ]);
    properties.extend(create_approval_parameters(include_prefix_rule));

    ToolSpec::Function(ResponsesApiTool {
        name: "exec_command".to_string(),
        description:
            "Runs a command in a PTY, returning output or a session ID for ongoing interaction."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["cmd".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_write_stdin_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "session_id".to_string(),
            JsonSchema::Number {
                description: Some("Identifier of the running unified exec session.".to_string()),
            },
        ),
        (
            "chars".to_string(),
            JsonSchema::String {
                description: Some("Bytes to write to stdin (may be empty to poll).".to_string()),
            },
        ),
        (
            "yield_time_ms".to_string(),
            JsonSchema::Number {
                description: Some(
                    "How long to wait (in milliseconds) for output before yielding.".to_string(),
                ),
            },
        ),
        (
            "max_output_tokens".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Maximum number of tokens to return. Excess output will be truncated."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "write_stdin".to_string(),
        description:
            "Writes characters to an existing unified exec session and returns recent output."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["session_id".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_shell_tool(include_prefix_rule: bool) -> ToolSpec {
    let mut properties = BTreeMap::from([
        (
            "command".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some("The command to execute".to_string()),
            },
        ),
        (
            "workdir".to_string(),
            JsonSchema::String {
                description: Some("The working directory to execute the command in".to_string()),
            },
        ),
        (
            "timeout_ms".to_string(),
            JsonSchema::Number {
                description: Some("The timeout for the command in milliseconds".to_string()),
            },
        ),
    ]);
    properties.extend(create_approval_parameters(include_prefix_rule));

    let description  = if cfg!(windows) {
        r#"Runs a Powershell command (Windows) and returns its output. Arguments to `shell` will be passed to CreateProcessW(). Most commands should be prefixed with ["powershell.exe", "-Command"].
        
Examples of valid command strings:

- ls -a (show hidden): ["powershell.exe", "-Command", "Get-ChildItem -Force"]
- recursive find by name: ["powershell.exe", "-Command", "Get-ChildItem -Recurse -Filter *.py"]
- recursive grep: ["powershell.exe", "-Command", "Get-ChildItem -Path C:\\myrepo -Recurse | Select-String -Pattern 'TODO' -CaseSensitive"]
- ps aux | grep python: ["powershell.exe", "-Command", "Get-Process | Where-Object { $_.ProcessName -like '*python*' }"]
- setting an env var: ["powershell.exe", "-Command", "$env:FOO='bar'; echo $env:FOO"]
- running an inline Python script: ["powershell.exe", "-Command", "@'\\nprint('Hello, world!')\\n'@ | python -"]"#
    } else {
        r#"Runs a shell command and returns its output.
- The arguments to `shell` will be passed to execvp(). Most terminal commands should be prefixed with ["bash", "-lc"].
- Always set the `workdir` param when using the shell function. Do not use `cd` unless absolutely necessary."#
    }.to_string();

    ToolSpec::Function(ResponsesApiTool {
        name: "shell".to_string(),
        description,
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["command".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_shell_command_tool(include_prefix_rule: bool) -> ToolSpec {
    let mut properties = BTreeMap::from([
        (
            "command".to_string(),
            JsonSchema::String {
                description: Some(
                    "The shell script to execute in the user's default shell".to_string(),
                ),
            },
        ),
        (
            "workdir".to_string(),
            JsonSchema::String {
                description: Some("The working directory to execute the command in".to_string()),
            },
        ),
        (
            "login".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "Whether to run the shell with login shell semantics. Defaults to true."
                        .to_string(),
                ),
            },
        ),
        (
            "timeout_ms".to_string(),
            JsonSchema::Number {
                description: Some("The timeout for the command in milliseconds".to_string()),
            },
        ),
    ]);
    properties.extend(create_approval_parameters(include_prefix_rule));

    let description = if cfg!(windows) {
        r#"Runs a Powershell command (Windows) and returns its output.
        
Examples of valid command strings:

- ls -a (show hidden): "Get-ChildItem -Force"
- recursive find by name: "Get-ChildItem -Recurse -Filter *.py"
- recursive grep: "Get-ChildItem -Path C:\\myrepo -Recurse | Select-String -Pattern 'TODO' -CaseSensitive"
- ps aux | grep python: "Get-Process | Where-Object { $_.ProcessName -like '*python*' }"
- setting an env var: "$env:FOO='bar'; echo $env:FOO"
- running an inline Python script: "@'\\nprint('Hello, world!')\\n'@ | python -"#
    } else {
        r#"Runs a shell command and returns its output.
- Always set the `workdir` param when using the shell_command function. Do not use `cd` unless absolutely necessary."#
    }.to_string();

    ToolSpec::Function(ResponsesApiTool {
        name: "shell_command".to_string(),
        description,
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["command".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_view_image_tool() -> ToolSpec {
    // Support only local filesystem path.
    let properties = BTreeMap::from([(
        "path".to_string(),
        JsonSchema::String {
            description: Some("Local filesystem path to an image file".to_string()),
        },
    )]);

    ToolSpec::Function(ResponsesApiTool {
        name: VIEW_IMAGE_TOOL_NAME.to_string(),
        description: "View a local image from the filesystem (only use if given a full filepath by the user, and the image isn't already attached to the session context within <image ...> tags)."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["path".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_spawn_agent_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "spawn_agent".to_string(),
        description:
            "Spawn a sub-agent for a well-scoped task. Returns the agent id to use to communicate with this agent."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["message".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_send_input_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "send_input".to_string(),
        description:
            "Send a message to an existing agent. Use interrupt=true to redirect work immediately."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["id".to_string(), "message".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_wait_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "wait".to_string(),
        description: "Wait for agents to reach a final status. Completed statuses may include the agent's final message. Returns empty status when timed out."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["ids".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_request_user_input_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "request_user_input".to_string(),
        description:
            "Request user input for one to three short questions and wait for the response."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["questions".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_close_agent_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "id".to_string(),
        JsonSchema::String {
            description: Some("Agent id to close (from spawn_agent).".to_string()),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "close_agent".to_string(),
        description: "Close an agent when it is no longer needed and return its last known status."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["id".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_test_sync_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "test_sync_tool".to_string(),
        description: "Internal synchronization helper used by Savfox integration tests."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
    })
}

fn create_grep_files_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "pattern".to_string(),
            JsonSchema::String {
                description: Some("Regular expression pattern to search for.".to_string()),
            },
        ),
        (
            "include".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional glob that limits which files are searched (e.g. \"*.rs\" or \
                     \"*.{ts,tsx}\")."
                        .to_string(),
                ),
            },
        ),
        (
            "path".to_string(),
            JsonSchema::String {
                description: Some(
                    "Directory or file path to search. Defaults to the session's working directory."
                        .to_string(),
                ),
            },
        ),
        (
            "limit".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Maximum number of file paths to return (defaults to 100).".to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "grep_files".to_string(),
        description: "Finds files whose contents match the pattern and lists them by modification \
                      time."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["pattern".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_read_file_tool() -> ToolSpec {
    let indentation_properties = BTreeMap::from([
        (
            "anchor_line".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Anchor line to center the indentation lookup on (defaults to offset)."
                        .to_string(),
                ),
            },
        ),
        (
            "max_levels".to_string(),
            JsonSchema::Number {
                description: Some(
                    "How many parent indentation levels (smaller indents) to include.".to_string(),
                ),
            },
        ),
        (
            "include_siblings".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "When true, include additional blocks that share the anchor indentation."
                        .to_string(),
                ),
            },
        ),
        (
            "include_header".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "Include doc comments or attributes directly above the selected block."
                        .to_string(),
                ),
            },
        ),
        (
            "max_lines".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Hard cap on the number of lines returned when using indentation mode."
                        .to_string(),
                ),
            },
        ),
    ]);

    let properties = BTreeMap::from([
        (
            "file_path".to_string(),
            JsonSchema::String {
                description: Some("Absolute path to the file".to_string()),
            },
        ),
        (
            "offset".to_string(),
            JsonSchema::Number {
                description: Some(
                    "The line number to start reading from. Must be 1 or greater.".to_string(),
                ),
            },
        ),
        (
            "limit".to_string(),
            JsonSchema::Number {
                description: Some("The maximum number of lines to return.".to_string()),
            },
        ),
        (
            "mode".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional mode selector: \"slice\" for simple ranges (default) or \"indentation\" \
                     to expand around an anchor line."
                        .to_string(),
                ),
            },
        ),
        (
            "indentation".to_string(),
            JsonSchema::Object {
                properties: indentation_properties,
                required: None,
                additional_properties: Some(false.into()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "read_file".to_string(),
        description:
            "Reads a local file with 1-indexed line numbers, supporting slice and indentation-aware block modes."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["file_path".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_list_dir_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "dir_path".to_string(),
            JsonSchema::String {
                description: Some("Absolute path to the directory to list.".to_string()),
            },
        ),
        (
            "offset".to_string(),
            JsonSchema::Number {
                description: Some(
                    "The entry number to start listing from. Must be 1 or greater.".to_string(),
                ),
            },
        ),
        (
            "limit".to_string(),
            JsonSchema::Number {
                description: Some("The maximum number of entries to return.".to_string()),
            },
        ),
        (
            "depth".to_string(),
            JsonSchema::Number {
                description: Some(
                    "The maximum directory depth to traverse. Must be 1 or greater.".to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "list_dir".to_string(),
        description:
            "Lists entries in a local directory with 1-indexed entry numbers and simple type labels."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["dir_path".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_write_file_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "file_path".to_string(),
            JsonSchema::String {
                description: Some("Absolute path of the file to write.".to_string()),
            },
        ),
        (
            "content".to_string(),
            JsonSchema::String {
                description: Some("The content to write to the file.".to_string()),
            },
        ),
        (
            "create_dirs".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "When true, create intermediate directories if they don't exist. Defaults to false."
                        .to_string(),
                ),
            },
        ),
        (
            "overwrite".to_string(),
            JsonSchema::Boolean {
                description: Some(
                    "When true (default), overwrite an existing file. When false, fail if the file already exists."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "write_file".to_string(),
        description: "Write content to a file at the given absolute path. Use this for creating new files or completely replacing file contents."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["file_path".to_string(), "content".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_web_fetch_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "url".to_string(),
            JsonSchema::String {
                description: Some("URL to fetch (http or https).".to_string()),
            },
        ),
        (
            "extract_mode".to_string(),
            JsonSchema::String {
                description: Some(
                    "How to process the response: \"markdown\" (default, converts HTML to markdown), \"text\" (plain text), or \"raw\" (unprocessed body)."
                        .to_string(),
                ),
            },
        ),
        (
            "max_length".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Maximum character length of the returned content. Defaults to 50000."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "web_fetch".to_string(),
        description: "Fetch the content of a URL and return it as text. Useful for reading web pages, APIs, or documentation. HTML is converted to readable markdown by default."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["url".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_web_search_provider_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "query".to_string(),
            JsonSchema::String {
                description: Some("The search query.".to_string()),
            },
        ),
        (
            "limit".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Maximum number of results to return (default 5, max 20).".to_string(),
                ),
            },
        ),
        (
            "site".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional domain filter to restrict results to a specific site.".to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "web_search_provider".to_string(),
        description: "Search the web using a search engine API. Returns structured results with title, URL, and snippet. Requires SAVFOX_WEB_SEARCH_API_KEY environment variable."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["query".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_cron_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Action to perform: \"list\" (show all jobs), \"add\" (create a job), \
                     \"remove\" (delete a job), \"next\" (show next run times)."
                        .to_string(),
                ),
            },
        ),
        (
            "name".to_string(),
            JsonSchema::String {
                description: Some(
                    "Name/identifier for the cron job (required for add, remove, next)."
                        .to_string(),
                ),
            },
        ),
        (
            "schedule".to_string(),
            JsonSchema::String {
                description: Some(
                    "Cron schedule expression, e.g. \"0 0 * * * *\" (required for add). \
                     Uses 6-field format: sec min hour day month weekday."
                        .to_string(),
                ),
            },
        ),
        (
            "command".to_string(),
            JsonSchema::String {
                description: Some(
                    "Shell command to execute when the job triggers (required for add)."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "cron".to_string(),
        description: "Manage scheduled cron jobs. Supports listing, adding, removing, \
                      and querying next run times for periodic tasks."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_tts_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "text".to_string(),
            JsonSchema::String {
                description: Some("Text to convert to speech.".to_string()),
            },
        ),
        (
            "voice".to_string(),
            JsonSchema::String {
                description: Some(
                    "Voice name: \"alloy\" (default), \"echo\", \"fable\", \"onyx\", \"nova\", \"shimmer\"."
                        .to_string(),
                ),
            },
        ),
        (
            "output_path".to_string(),
            JsonSchema::String {
                description: Some("File path where the audio (MP3) will be saved.".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "tts".to_string(),
        description: "Convert text to speech using the OpenAI TTS API. \
                      Requires OPENAI_API_KEY environment variable."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["text".to_string(), "output_path".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_image_analyze_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "path".to_string(),
            JsonSchema::String {
                description: Some(
                    "Path to the local image file to analyze.".to_string(),
                ),
            },
        ),
        (
            "prompt".to_string(),
            JsonSchema::String {
                description: Some(
                    "Analysis prompt describing what to look for in the image.".to_string(),
                ),
            },
        ),
        (
            "detail".to_string(),
            JsonSchema::String {
                description: Some(
                    "Detail level: \"low\" for a brief analysis, \"high\" (default) for a thorough analysis."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "image_analyze".to_string(),
        description:
            "Analyze a local image using the vision model. \
             The image is loaded and injected into the conversation along with your analysis prompt."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["path".to_string(), "prompt".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_message_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "channel".to_string(),
            JsonSchema::String {
                description: Some(
                    "Channel identifier in the format `platform:id` (e.g., `discord:12345`, `telegram:67890`, `slack:C01ABC`)."
                        .to_string(),
                ),
            },
        ),
        (
            "text".to_string(),
            JsonSchema::String {
                description: Some("The message text to send.".to_string()),
            },
        ),
        (
            "format".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional format hint: \"plain\" (default), \"markdown\", or \"html\"."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "message".to_string(),
        description: "Send a message to a chat platform channel via the gateway server. \
             The channel is specified as `platform:id`."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["channel".to_string(), "text".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_sessions_list_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "filter".to_string(),
        JsonSchema::String {
            description: Some("Optional filter for session type.".to_string()),
        },
    )]);

    ToolSpec::Function(ResponsesApiTool {
        name: "sessions_list".to_string(),
        description: "List active sessions connected to the gateway server.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
    })
}

fn create_sessions_history_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "sessions_history".to_string(),
        description: "Get the message history for a specific session.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["session_id".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_sessions_send_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "sessions_send".to_string(),
        description: "Send a message to another active session.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["session_id".to_string(), "message".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_session_status_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "session_id".to_string(),
        JsonSchema::String {
            description: Some("Session ID to check status for.".to_string()),
        },
    )]);

    ToolSpec::Function(ResponsesApiTool {
        name: "session_status".to_string(),
        description: "Get metadata and status information for a specific session.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["session_id".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_list_mcp_resources_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "list_mcp_resources".to_string(),
        description: "Lists resources provided by MCP servers. Resources allow servers to share data that provides context to language models, such as files, database schemas, or application-specific information. Prefer resources over web search when possible.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
    })
}

fn create_list_mcp_resource_templates_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "list_mcp_resource_templates".to_string(),
        description: "Lists resource templates provided by MCP servers. Parameterized resource templates allow servers to share data that takes parameters and provides context to language models, such as files, database schemas, or application-specific information. Prefer resource templates over web search when possible.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
    })
}

fn create_read_mcp_resource_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "read_mcp_resource".to_string(),
        description:
            "Read a specific resource from an MCP server given the server name and resource URI."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["server".to_string(), "uri".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}
// === Phase B-H tool creation functions ===

fn create_process_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Action to perform: \"list\", \"poll\", \"read_log\", \"write\", \"send_keys\", \"kill\"."
                        .to_string(),
                ),
            },
        ),
        (
            "process_id".to_string(),
            JsonSchema::String {
                description: Some("Identifier of the target process.".to_string()),
            },
        ),
        (
            "input".to_string(),
            JsonSchema::String {
                description: Some("Text to write to stdin (for \"write\" action).".to_string()),
            },
        ),
        (
            "keys".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some(
                    "Key names to send (for \"send_keys\" action): \"ctrl-c\", \"ctrl-d\", \"enter\", \"tab\", \"up\", \"down\", \"left\", \"right\", \"escape\", \"f1\"-\"f4\", \"home\", \"end\"."
                        .to_string(),
                ),
            },
        ),
        (
            "signal".to_string(),
            JsonSchema::String {
                description: Some(
                    "Signal to send (for \"kill\" action): \"SIGTERM\" (default) or \"SIGINT\"."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "process".to_string(),
        description: "Manage background processes: list active processes, read their output, \
                      write to stdin, send key sequences, or terminate them."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_discord_actions_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Discord action: \"send_message\", \"edit_message\", \"delete_message\", \"add_reaction\", \"remove_reaction\", \"pin_message\", \"unpin_message\", \"create_session\", \"list_members\", \"kick_member\", \"ban_member\", \"get_channel_info\", \"get_message_history\", \"set_channel_topic\"."
                        .to_string(),
                ),
            },
        ),
        (
            "channel_id".to_string(),
            JsonSchema::String {
                description: Some("Discord channel ID.".to_string()),
            },
        ),
        (
            "message_id".to_string(),
            JsonSchema::String {
                description: Some("Discord message ID (for message-specific actions).".to_string()),
            },
        ),
        (
            "guild_id".to_string(),
            JsonSchema::String {
                description: Some("Discord guild/server ID (for guild-specific actions).".to_string()),
            },
        ),
        (
            "content".to_string(),
            JsonSchema::String {
                description: Some("Message content or text payload.".to_string()),
            },
        ),
        (
            "emoji".to_string(),
            JsonSchema::String {
                description: Some("Emoji for reaction actions (URL-encoded).".to_string()),
            },
        ),
        (
            "topic".to_string(),
            JsonSchema::String {
                description: Some("Channel topic text.".to_string()),
            },
        ),
        (
            "user_id".to_string(),
            JsonSchema::String {
                description: Some("Target user ID (for moderation actions).".to_string()),
            },
        ),
        (
            "reason".to_string(),
            JsonSchema::String {
                description: Some("Reason for moderation action.".to_string()),
            },
        ),
        (
            "limit".to_string(),
            JsonSchema::Number {
                description: Some("Maximum number of items to return.".to_string()),
            },
        ),
        (
            "name".to_string(),
            JsonSchema::String {
                description: Some("Name for created resources (e.g. session name).".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "discord_actions".to_string(),
        description:
            "Perform Discord platform actions: send/edit/delete messages, reactions, pins, \
                      sessions, member management, and channel operations. \
                      Requires DISCORD_BOT_TOKEN environment variable."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_slack_actions_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Slack action: \"send_message\", \"update_message\", \"delete_message\", \"add_reaction\", \"remove_reaction\", \"pin_item\", \"unpin_item\", \"get_channel_history\", \"set_channel_topic\"."
                        .to_string(),
                ),
            },
        ),
        (
            "channel_id".to_string(),
            JsonSchema::String {
                description: Some("Slack channel ID.".to_string()),
            },
        ),
        (
            "message_id".to_string(),
            JsonSchema::String {
                description: Some("Slack message timestamp (ts) for message-specific actions.".to_string()),
            },
        ),
        (
            "content".to_string(),
            JsonSchema::String {
                description: Some("Message content or text payload.".to_string()),
            },
        ),
        (
            "emoji".to_string(),
            JsonSchema::String {
                description: Some("Emoji name for reaction actions (without colons).".to_string()),
            },
        ),
        (
            "topic".to_string(),
            JsonSchema::String {
                description: Some("Channel topic text.".to_string()),
            },
        ),
        (
            "limit".to_string(),
            JsonSchema::Number {
                description: Some("Maximum number of items to return.".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "slack_actions".to_string(),
        description:
            "Perform Slack platform actions: send/update/delete messages, reactions, pins, \
                      channel history, and topic management. \
                      Requires SLACK_BOT_TOKEN environment variable."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_telegram_actions_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Telegram action: \"send_message\", \"edit_message\", \"delete_message\", \"send_sticker\", \"get_chat_info\"."
                        .to_string(),
                ),
            },
        ),
        (
            "channel_id".to_string(),
            JsonSchema::String {
                description: Some("Telegram chat ID.".to_string()),
            },
        ),
        (
            "message_id".to_string(),
            JsonSchema::String {
                description: Some("Telegram message ID for message-specific actions.".to_string()),
            },
        ),
        (
            "content".to_string(),
            JsonSchema::String {
                description: Some("Message content or text payload.".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "telegram_actions".to_string(),
        description:
            "Perform Telegram platform actions: send/edit/delete messages, send stickers, \
                      and get chat information. \
                      Requires TELEGRAM_BOT_TOKEN environment variable."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_sessions_spawn_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "sessions_spawn".to_string(),
        description: "Spawn a new sub-agent session with an optional model override and custom instructions. \
                      The agent processes the given prompt and returns results."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["prompt".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_gateway_status_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Gateway action: \"status\" (connected clients), \"config\" (configuration), \
                     \"health\" (health check), \"config.get\" (full config with schema), \
                     \"config.patch\" (merge update config), \"config.apply\" (replace config), \
                     \"restart\" (restart gateway)."
                        .to_string(),
                ),
            },
        ),
        (
            "raw".to_string(),
            JsonSchema::String {
                description: Some(
                    "Config content (YAML/JSON) for config.patch and config.apply actions."
                        .to_string(),
                ),
            },
        ),
        (
            "note".to_string(),
            JsonSchema::String {
                description: Some("Change description note for config operations.".to_string()),
            },
        ),
        (
            "delay_ms".to_string(),
            JsonSchema::Number {
                description: Some("Restart delay in milliseconds.".to_string()),
            },
        ),
        (
            "reason".to_string(),
            JsonSchema::String {
                description: Some("Reason for restart.".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "gateway_status".to_string(),
        description: "Query and manage the gateway server: check status/health/config, \
                      update configuration (patch/apply), or restart the gateway."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_memory_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Memory action: \"search\", \"get\", \"add\", \"delete\", \"list\"."
                        .to_string(),
                ),
            },
        ),
        (
            "query".to_string(),
            JsonSchema::String {
                description: Some("Search query text (for \"search\" action).".to_string()),
            },
        ),
        (
            "key".to_string(),
            JsonSchema::String {
                description: Some(
                    "Memory entry key (for \"get\", \"add\", \"delete\").".to_string(),
                ),
            },
        ),
        (
            "content".to_string(),
            JsonSchema::String {
                description: Some("Memory entry content (for \"add\" action).".to_string()),
            },
        ),
        (
            "tags".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some("Tags to associate with the memory entry.".to_string()),
            },
        ),
        (
            "limit".to_string(),
            JsonSchema::Number {
                description: Some("Maximum results to return (default 10).".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "memory".to_string(),
        description: "Persistent memory store with BM25 text search. Store, retrieve, search, \
                      and manage memory entries that persist across sessions."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_md_memory_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Action: \"list\", \"get\", \"create\", \"update\", \"delete\", \"search\", \"promote\"."
                        .to_string(),
                ),
            },
        ),
        (
            "slug".to_string(),
            JsonSchema::String {
                description: Some(
                    "Memory entry slug (kebab-case filename without .md).".to_string(),
                ),
            },
        ),
        (
            "layer".to_string(),
            JsonSchema::String {
                description: Some(
                    "Memory layer: \"global\", \"project\", \"agent\", \"session\".".to_string(),
                ),
            },
        ),
        (
            "target_layer".to_string(),
            JsonSchema::String {
                description: Some("Target layer for \"promote\" action.".to_string()),
            },
        ),
        (
            "content".to_string(),
            JsonSchema::String {
                description: Some("Markdown content for \"create\" or \"update\".".to_string()),
            },
        ),
        (
            "tags".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some("Tags for the memory entry.".to_string()),
            },
        ),
        (
            "priority".to_string(),
            JsonSchema::Number {
                description: Some("Priority 1-10 (higher = more important, default 5).".to_string()),
            },
        ),
        (
            "query".to_string(),
            JsonSchema::String {
                description: Some("Search query text (for \"search\" action).".to_string()),
            },
        ),
        (
            "limit".to_string(),
            JsonSchema::Number {
                description: Some("Maximum results to return (default 10).".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "md_memory".to_string(),
        description: "Markdown memory system with 4 layers (global, project, agent, session). \
                      Manage user-editable .md knowledge files that are injected into system prompts. \
                      Use this to store architecture decisions, coding conventions, and project knowledge."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_image_generate_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "prompt".to_string(),
            JsonSchema::String {
                description: Some("Image generation prompt describing what to create.".to_string()),
            },
        ),
        (
            "output_path".to_string(),
            JsonSchema::String {
                description: Some("File path where the generated image will be saved.".to_string()),
            },
        ),
        (
            "size".to_string(),
            JsonSchema::String {
                description: Some(
                    "Image dimensions: \"1024x1024\" (default), \"1792x1024\", \"1024x1792\"."
                        .to_string(),
                ),
            },
        ),
        (
            "model".to_string(),
            JsonSchema::String {
                description: Some("Model to use: \"dall-e-3\" (default).".to_string()),
            },
        ),
        (
            "quality".to_string(),
            JsonSchema::String {
                description: Some("Quality: \"standard\" (default) or \"hd\".".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "image_generate".to_string(),
        description: "Generate an image using the DALL-E API and save it to a file. \
                      Requires OPENAI_API_KEY environment variable."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["prompt".to_string(), "output_path".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_agents_list_tool() -> ToolSpec {
    let properties = BTreeMap::from([(
        "filter".to_string(),
        JsonSchema::String {
            description: Some("Optional filter for agent status.".to_string()),
        },
    )]);

    ToolSpec::Function(ResponsesApiTool {
        name: "agents_list".to_string(),
        description: "List all active agents and their status, model, and session information."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
    })
}

fn create_nodes_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Node action: \"list\", \"status\", \"run_command\", \"camera_capture\", \"get_location\", \"send_notification\"."
                        .to_string(),
                ),
            },
        ),
        (
            "node_id".to_string(),
            JsonSchema::String {
                description: Some("Target node identifier.".to_string()),
            },
        ),
        (
            "command".to_string(),
            JsonSchema::String {
                description: Some("Command to run on the node (for \"run_command\").".to_string()),
            },
        ),
        (
            "message".to_string(),
            JsonSchema::String {
                description: Some("Notification message text.".to_string()),
            },
        ),
        (
            "title".to_string(),
            JsonSchema::String {
                description: Some("Notification title.".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "nodes".to_string(),
        description:
            "Interact with paired device nodes: list devices, check status, run commands, \
                      capture camera, get location, or send notifications."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_browser_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Browser action: \"status\", \"start\", \"stop\", \"profiles\", \"tabs\", \
                     \"open\", \"focus\", \"close\", \"snapshot\", \"screenshot\", \"navigate\", \
                     \"console\", \"pdf\", \"upload\", \"dialog\", \"act\"."
                        .to_string(),
                ),
            },
        ),
        (
            "profile".to_string(),
            JsonSchema::String {
                description: Some(
                    "Browser profile: \"default\", \"chrome\", or custom name.".to_string(),
                ),
            },
        ),
        (
            "target_id".to_string(),
            JsonSchema::String {
                description: Some("Tab/page target identifier.".to_string()),
            },
        ),
        (
            "target_url".to_string(),
            JsonSchema::String {
                description: Some("URL for navigate/open actions.".to_string()),
            },
        ),
        (
            "format".to_string(),
            JsonSchema::String {
                description: Some("Snapshot format: \"ai\" or \"aria\".".to_string()),
            },
        ),
        (
            "refs".to_string(),
            JsonSchema::String {
                description: Some("Ref strategy for snapshots: \"role\" or \"aria\".".to_string()),
            },
        ),
        (
            "max_chars".to_string(),
            JsonSchema::Number {
                description: Some("Maximum characters for snapshot output.".to_string()),
            },
        ),
        (
            "selector".to_string(),
            JsonSchema::String {
                description: Some("CSS selector for targeted operations.".to_string()),
            },
        ),
        (
            "full_page".to_string(),
            JsonSchema::Boolean {
                description: Some("Capture full page screenshot (default: false).".to_string()),
            },
        ),
        (
            "image_type".to_string(),
            JsonSchema::String {
                description: Some("Screenshot image type: \"png\" or \"jpeg\".".to_string()),
            },
        ),
        (
            "request".to_string(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
        (
            "paths".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some("File paths for upload action.".to_string()),
            },
        ),
        (
            "accept".to_string(),
            JsonSchema::Boolean {
                description: Some("Whether to accept a dialog.".to_string()),
            },
        ),
        (
            "prompt_text".to_string(),
            JsonSchema::String {
                description: Some("Text to enter in a prompt dialog.".to_string()),
            },
        ),
        (
            "level".to_string(),
            JsonSchema::String {
                description: Some(
                    "Console log level filter: \"error\", \"warning\", \"log\", \"info\"."
                        .to_string(),
                ),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "browser".to_string(),
        description: "Browser automation: navigate pages, take screenshots, capture DOM snapshots, \
                      interact with UI elements (click, type, drag), handle dialogs and file uploads. \
                      Requires an external browser service at SAVFOX_BROWSER_URL (default: http://127.0.0.1:9222)."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_whatsapp_actions_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some("WhatsApp action: \"react\".".to_string()),
            },
        ),
        (
            "chat_jid".to_string(),
            JsonSchema::String {
                description: Some(
                    "Chat JID identifier (e.g., \"1234567890@s.whatsapp.net\").".to_string(),
                ),
            },
        ),
        (
            "message_id".to_string(),
            JsonSchema::String {
                description: Some("Message ID to react to.".to_string()),
            },
        ),
        (
            "emoji".to_string(),
            JsonSchema::String {
                description: Some("Emoji for reaction.".to_string()),
            },
        ),
        (
            "remove".to_string(),
            JsonSchema::Boolean {
                description: Some("Whether to remove an existing reaction.".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "whatsapp_actions".to_string(),
        description: "WhatsApp message actions: send emoji reactions. \
                      Requires WHATSAPP_API_URL environment variable."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_agent_step_tool() -> ToolSpec {
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

    ToolSpec::Function(ResponsesApiTool {
        name: "agent_step".to_string(),
        description: "Run a single agent processing step: send a prompt to a session, \
                      wait for the agent to complete, and return the assistant's reply. \
                      Core building block for agent-to-agent orchestration."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["session_id".to_string(), "prompt".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_sessions_send_a2a_tool() -> ToolSpec {
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
                description: Some(
                    "Whether to announce the final result to a channel.".to_string(),
                ),
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
                description: Some(
                    "Optional system prompt for the target agent.".to_string(),
                ),
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
                description: Some(
                    "Optional per-message timeout in milliseconds.".to_string(),
                ),
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

    ToolSpec::Function(ResponsesApiTool {
        name: "sessions_send_a2a".to_string(),
        description: "Agent-to-agent structured messaging: send a typed A2AMessage (request, response, \
                      or notification) from one agent session to another. Supports correlation IDs for \
                      request-response matching, delegation chain tracking, multi-turn ping-pong \
                      conversation, and channel announcements."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![
                "source_session_id".to_string(),
                "target_session_id".to_string(),
                "message".to_string(),
            ]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_canvas_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Canvas action: \"present\" (show canvas), \"hide\", \"navigate\" (load URL), \
                     \"eval\" (run JavaScript), \"snapshot\" (capture screenshot), \
                     \"a2ui_push\" (push A2UI component), \"a2ui_reset\"."
                        .to_string(),
                ),
            },
        ),
        (
            "node_id".to_string(),
            JsonSchema::String {
                description: Some("Target node ID.".to_string()),
            },
        ),
        (
            "url".to_string(),
            JsonSchema::String {
                description: Some("URL for navigate action.".to_string()),
            },
        ),
        (
            "code".to_string(),
            JsonSchema::String {
                description: Some("JavaScript code for eval action.".to_string()),
            },
        ),
        (
            "format".to_string(),
            JsonSchema::String {
                description: Some(
                    "Image format for snapshot: \"png\", \"jpg\", \"jpeg\".".to_string(),
                ),
            },
        ),
        (
            "command".to_string(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
        (
            "component".to_string(),
            JsonSchema::String {
                description: Some("A2UI component ID or path.".to_string()),
            },
        ),
        (
            "props".to_string(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
        (
            "title".to_string(),
            JsonSchema::String {
                description: Some("Title for the canvas presentation.".to_string()),
            },
        ),
        (
            "width".to_string(),
            JsonSchema::Number {
                description: Some("Width of the canvas.".to_string()),
            },
        ),
        (
            "height".to_string(),
            JsonSchema::Number {
                description: Some("Height of the canvas.".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "canvas".to_string(),
        description: "A2UI canvas rendering on connected node displays: present/hide canvas, \
                      navigate to URLs, evaluate JavaScript, capture snapshots, and push A2UI components. \
                      Requires canvas service at SAVFOX_CANVAS_URL (default: http://127.0.0.1:9300)."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_gateway_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "method".to_string(),
            JsonSchema::String {
                description: Some(
                    "Gateway JSON-RPC method to call (e.g., \"agents.list\", \"chat.send\", \
                     \"sessions.list\", \"cron.add\", \"config.get\", \"tools.invoke\")."
                        .to_string(),
                ),
            },
        ),
        (
            "params".to_string(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "gateway".to_string(),
        description: "Invoke any gateway server JSON-RPC method: agent management, session control, \
                      chat operations, cron scheduling, node control, channel management, TTS, \
                      and configuration. Requires SAVFOX_GATEWAY_URL and optionally SAVFOX_GATEWAY_TOKEN."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["method".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_channel_tools_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::String {
                description: Some(
                    "Channel action: \"send\", \"react\", \"edit\", \"delete\", \"history\", \
                     \"list_channels\"."
                        .to_string(),
                ),
            },
        ),
        (
            "platform".to_string(),
            JsonSchema::String {
                description: Some(
                    "Target platform: \"discord\", \"slack\", \"telegram\", \"whatsapp\", \"webhook\"."
                        .to_string(),
                ),
            },
        ),
        (
            "channel_id".to_string(),
            JsonSchema::String {
                description: Some("Channel/chat identifier.".to_string()),
            },
        ),
        (
            "content".to_string(),
            JsonSchema::String {
                description: Some("Message content (for send/edit).".to_string()),
            },
        ),
        (
            "message_id".to_string(),
            JsonSchema::String {
                description: Some("Message ID (for edit/delete/react).".to_string()),
            },
        ),
        (
            "emoji".to_string(),
            JsonSchema::String {
                description: Some("Emoji for reactions.".to_string()),
            },
        ),
        (
            "session_id".to_string(),
            JsonSchema::String {
                description: Some("Session/reply-to ID.".to_string()),
            },
        ),
        (
            "limit".to_string(),
            JsonSchema::Number {
                description: Some("Maximum messages to fetch for history (default: 50).".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "channel_tools".to_string(),
        description: "Unified multi-platform channel tool: send messages, reactions, edit/delete messages, \
                      fetch history, and list channels across Discord, Slack, Telegram, WhatsApp, and webhooks. \
                      Routes through the gateway server."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["action".to_string(), "platform".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

fn create_llm_task_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "prompt".to_string(),
            JsonSchema::String {
                description: Some("The task prompt to send to the LLM.".to_string()),
            },
        ),
        (
            "model".to_string(),
            JsonSchema::String {
                description: Some(
                    "Optional model to use (defaults to the agent's model).".to_string(),
                ),
            },
        ),
        (
            "system".to_string(),
            JsonSchema::String {
                description: Some("Optional system prompt for the task.".to_string()),
            },
        ),
        (
            "output_schema".to_string(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
        (
            "temperature".to_string(),
            JsonSchema::Number {
                description: Some("Temperature for sampling (0.0 to 2.0).".to_string()),
            },
        ),
        (
            "max_tokens".to_string(),
            JsonSchema::Number {
                description: Some("Maximum tokens for the response.".to_string()),
            },
        ),
        (
            "timeout_secs".to_string(),
            JsonSchema::Number {
                description: Some("Timeout in seconds (default: 120).".to_string()),
            },
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "llm_task".to_string(),
        description: "Run a standalone LLM inference task with a specific prompt, optional model, \
                      system prompt, and JSON schema validation on the output. \
                      Routes through the gateway's OpenAI-compatible endpoint."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["prompt".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

/// TODO(dylan): deprecate once we get rid of json tool
#[derive(Serialize, Deserialize)]
pub(crate) struct ApplyPatchToolArgs {
    pub(crate) input: String,
}

/// Returns JSON values that are compatible with Function Calling in the
/// Responses API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=responses
pub fn create_tools_json_for_responses_api(
    tools: &[ToolSpec],
) -> crate::error::Result<Vec<serde_json::Value>> {
    let mut tools_json = Vec::new();

    for tool in tools {
        let json = serde_json::to_value(tool)?;
        tools_json.push(json);
    }

    Ok(tools_json)
}
/// Returns JSON values that are compatible with Function Calling in the
/// Chat Completions API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=chat
pub(crate) fn create_tools_json_for_chat_completions_api(
    tools: &[ToolSpec],
) -> crate::error::Result<Vec<serde_json::Value>> {
    // We start with the JSON for the Responses API and than rewrite it to match
    // the chat completions tool call format.
    let responses_api_tools_json = create_tools_json_for_responses_api(tools)?;
    let tools_json = responses_api_tools_json
        .into_iter()
        .filter_map(|mut tool| {
            if tool.get("type") != Some(&serde_json::Value::String("function".to_string())) {
                return None;
            }

            if let Some(map) = tool.as_object_mut() {
                let name = map
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                // Remove "type" field as it is not needed in chat completions.
                map.remove("type");
                Some(json!({
                    "type": "function",
                    "name": name,
                    "function": map,
                }))
            } else {
                None
            }
        })
        .collect::<Vec<serde_json::Value>>();
    Ok(tools_json)
}

/// Returns JSON values that are compatible with the Anthropic Messages API tool format:
/// `{"name": "...", "description": "...", "input_schema": {...}}`
///
/// Starts from the Responses API format and restructures:
/// - Removes the `type: "function"` wrapper
/// - Renames `parameters` to `input_schema`
pub(crate) fn create_tools_json_for_anthropic_api(
    tools: &[ToolSpec],
) -> crate::error::Result<Vec<serde_json::Value>> {
    let responses_api_tools_json = create_tools_json_for_responses_api(tools)?;
    let tools_json = responses_api_tools_json
        .into_iter()
        .filter_map(|tool| {
            if tool.get("type") != Some(&serde_json::Value::String("function".to_string())) {
                return None;
            }

            let name = tool
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let description = tool
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let input_schema = tool
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));

            Some(json!({
                "name": name,
                "description": description,
                "input_schema": input_schema
            }))
        })
        .collect::<Vec<serde_json::Value>>();
    Ok(tools_json)
}

pub(crate) fn mcp_tool_to_openai_tool(
    fully_qualified_name: String,
    tool: rmcp::model::Tool,
) -> Result<ResponsesApiTool, serde_json::Error> {
    let rmcp::model::Tool {
        description,
        input_schema,
        ..
    } = tool;

    let mut serialized_input_schema = serde_json::Value::Object(input_schema.as_ref().clone());

    // OpenAI models mandate the "properties" field in the schema. Some MCP
    // servers omit it (or set it to null), so we insert an empty object to
    // match the behavior of the Agents SDK.
    if let serde_json::Value::Object(obj) = &mut serialized_input_schema
        && obj.get("properties").is_none_or(serde_json::Value::is_null)
    {
        obj.insert(
            "properties".to_string(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
    }

    // Serialize to a raw JSON value so we can sanitize schemas coming from MCP
    // servers. Some servers omit the top-level or nested `type` in JSON
    // Schemas (e.g. using enum/anyOf), or use unsupported variants like
    // `integer`. Our internal JsonSchema is a small subset and requires
    // `type`, so we coerce/sanitize here for compatibility.
    sanitize_json_schema(&mut serialized_input_schema);
    let input_schema = serde_json::from_value::<JsonSchema>(serialized_input_schema)?;

    Ok(ResponsesApiTool {
        name: fully_qualified_name,
        description: description.map(Into::into).unwrap_or_default(),
        strict: false,
        parameters: input_schema,
    })
}

fn dynamic_tool_to_openai_tool(
    tool: &DynamicToolSpec,
) -> Result<ResponsesApiTool, serde_json::Error> {
    let input_schema = parse_tool_input_schema(&tool.input_schema)?;

    Ok(ResponsesApiTool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        strict: false,
        parameters: input_schema,
    })
}

/// Parse the tool input_schema or return an error for invalid schema
pub fn parse_tool_input_schema(input_schema: &JsonValue) -> Result<JsonSchema, serde_json::Error> {
    let mut input_schema = input_schema.clone();
    sanitize_json_schema(&mut input_schema);
    serde_json::from_value::<JsonSchema>(input_schema)
}

/// Sanitize a JSON Schema (as serde_json::Value) so it can fit our limited
/// JsonSchema enum. This function:
/// - Ensures every schema object has a "type". If missing, infers it from common keywords
///   (properties => object, items => array, enum/const/format => string) and otherwise defaults to
///   "string".
/// - Fills required child fields (e.g. array items, object properties) with permissive defaults
///   when absent.
fn sanitize_json_schema(value: &mut JsonValue) {
    match value {
        JsonValue::Bool(_) => {
            // JSON Schema boolean form: true/false. Coerce to an accept-all string.
            *value = json!({ "type": "string" });
        }
        JsonValue::Array(arr) => {
            for v in arr.iter_mut() {
                sanitize_json_schema(v);
            }
        }
        JsonValue::Object(map) => {
            // First, recursively sanitize known nested schema holders
            if let Some(props) = map.get_mut("properties")
                && let Some(props_map) = props.as_object_mut()
            {
                for (_k, v) in props_map.iter_mut() {
                    sanitize_json_schema(v);
                }
            }
            if let Some(items) = map.get_mut("items") {
                sanitize_json_schema(items);
            }
            // Some schemas use oneOf/anyOf/allOf - sanitize their entries
            for combiner in ["oneOf", "anyOf", "allOf", "prefixItems"] {
                if let Some(v) = map.get_mut(combiner) {
                    sanitize_json_schema(v);
                }
            }

            // Normalize/ensure type
            let mut ty = map.get("type").and_then(|v| v.as_str()).map(str::to_string);

            // If type is an array (union), pick first supported; else leave to inference
            if ty.is_none()
                && let Some(JsonValue::Array(types)) = map.get("type")
            {
                for t in types {
                    if let Some(tt) = t.as_str()
                        && matches!(
                            tt,
                            "object" | "array" | "string" | "number" | "integer" | "boolean"
                        )
                    {
                        ty = Some(tt.to_string());
                        break;
                    }
                }
            }

            // Infer type if still missing
            if ty.is_none() {
                if map.contains_key("properties")
                    || map.contains_key("required")
                    || map.contains_key("additionalProperties")
                {
                    ty = Some("object".to_string());
                } else if map.contains_key("items") || map.contains_key("prefixItems") {
                    ty = Some("array".to_string());
                } else if map.contains_key("enum")
                    || map.contains_key("const")
                    || map.contains_key("format")
                {
                    ty = Some("string".to_string());
                } else if map.contains_key("minimum")
                    || map.contains_key("maximum")
                    || map.contains_key("exclusiveMinimum")
                    || map.contains_key("exclusiveMaximum")
                    || map.contains_key("multipleOf")
                {
                    ty = Some("number".to_string());
                }
            }
            // If we still couldn't infer, default to string
            let ty = ty.unwrap_or_else(|| "string".to_string());
            map.insert("type".to_string(), JsonValue::String(ty.to_string()));

            // Ensure object schemas have properties map
            if ty == "object" {
                if !map.contains_key("properties") {
                    map.insert(
                        "properties".to_string(),
                        JsonValue::Object(serde_json::Map::new()),
                    );
                }
                // If additionalProperties is an object schema, sanitize it too.
                // Leave booleans as-is, since JSON Schema allows boolean here.
                if let Some(ap) = map.get_mut("additionalProperties") {
                    let is_bool = matches!(ap, JsonValue::Bool(_));
                    if !is_bool {
                        sanitize_json_schema(ap);
                    }
                }
            }

            // Ensure array schemas have items
            if ty == "array" && !map.contains_key("items") {
                map.insert("items".to_string(), json!({ "type": "string" }));
            }
        }
        _ => {}
    }
}

/// Builds the tool registry builder while collecting tool specs for later serialization.
pub(crate) fn build_specs(
    config: &ToolsConfig,
    mcp_tools: Option<HashMap<String, rmcp::model::Tool>>,
    dynamic_tools: &[DynamicToolSpec],
) -> ToolRegistryBuilder {
    use std::sync::Arc;

    use crate::tools::handlers::{
        AgentStepHandler, AgentsListHandler, ApplyPatchHandler, BrowserHandler, CanvasHandler,
        ChannelToolsHandler, CollabHandler, CronHandler, DiscordActionsHandler, DynamicToolHandler,
        GatewayStatusHandler, GatewayToolHandler, GrepFilesHandler, ImageAnalyzeHandler,
        ImageGenerateHandler, ListDirHandler, LlmTaskHandler, McpHandler, McpResourceHandler,
        MdMemoryHandler, MemoryHandler, MessageHandler, NodesHandler, PlanHandler, ProcessHandler,
        ReadFileHandler, RequestUserInputHandler, SessionsHandler, SessionsSendA2AHandler,
        SessionsSpawnHandler, ShellCommandHandler, ShellHandler, SlackActionsHandler,
        TelegramActionsHandler, TestSyncHandler, TtsHandler, UnifiedExecHandler, ViewImageHandler,
        WebFetchHandler, WebSearchHandler, WhatsAppActionsHandler, WriteFileHandler,
    };

    let mut builder = ToolRegistryBuilder::new();

    let shell_handler = Arc::new(ShellHandler);
    let unified_exec_handler = Arc::new(UnifiedExecHandler);
    let plan_handler = Arc::new(PlanHandler);
    let apply_patch_handler = Arc::new(ApplyPatchHandler);
    let dynamic_tool_handler = Arc::new(DynamicToolHandler);
    let view_image_handler = Arc::new(ViewImageHandler);
    let mcp_handler = Arc::new(McpHandler);
    let mcp_resource_handler = Arc::new(McpResourceHandler);
    let shell_command_handler = Arc::new(ShellCommandHandler);
    let request_user_input_handler = Arc::new(RequestUserInputHandler);

    match &config.shell_type {
        ConfigShellToolType::Default => {
            builder.push_spec(create_shell_tool(config.request_rule_enabled));
        }
        ConfigShellToolType::Local => {
            builder.push_spec(ToolSpec::LocalShell {});
        }
        ConfigShellToolType::UnifiedExec => {
            builder.push_spec(create_exec_command_tool(config.request_rule_enabled));
            builder.push_spec(create_write_stdin_tool());
            builder.register_handler("exec_command", unified_exec_handler.clone());
            builder.register_handler("write_stdin", unified_exec_handler);
        }
        ConfigShellToolType::Disabled => {
            // Do nothing.
        }
        ConfigShellToolType::ShellCommand => {
            builder.push_spec(create_shell_command_tool(config.request_rule_enabled));
        }
    }

    if config.shell_type != ConfigShellToolType::Disabled {
        // Always register shell aliases so older prompts remain compatible.
        builder.register_handler("shell", shell_handler.clone());
        builder.register_handler("container.exec", shell_handler.clone());
        builder.register_handler("local_shell", shell_handler);
        builder.register_handler("shell_command", shell_command_handler);
    }

    builder.push_spec_with_parallel_support(create_list_mcp_resources_tool(), true);
    builder.push_spec_with_parallel_support(create_list_mcp_resource_templates_tool(), true);
    builder.push_spec_with_parallel_support(create_read_mcp_resource_tool(), true);
    builder.register_handler("list_mcp_resources", mcp_resource_handler.clone());
    builder.register_handler("list_mcp_resource_templates", mcp_resource_handler.clone());
    builder.register_handler("read_mcp_resource", mcp_resource_handler);

    builder.push_spec(PLAN_TOOL.clone());
    builder.register_handler("update_plan", plan_handler);

    if config.collaboration_modes_tools {
        builder.push_spec(create_request_user_input_tool());
        builder.register_handler("request_user_input", request_user_input_handler);
    }

    if let Some(apply_patch_tool_type) = &config.apply_patch_tool_type {
        match apply_patch_tool_type {
            ApplyPatchToolType::Freeform => {
                builder.push_spec(create_apply_patch_freeform_tool());
            }
            ApplyPatchToolType::Function => {
                builder.push_spec(create_apply_patch_json_tool());
            }
        }
        builder.register_handler("apply_patch", apply_patch_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"grep_files".to_string())
    {
        let grep_files_handler = Arc::new(GrepFilesHandler);
        builder.push_spec_with_parallel_support(create_grep_files_tool(), true);
        builder.register_handler("grep_files", grep_files_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"read_file".to_string())
    {
        let read_file_handler = Arc::new(ReadFileHandler);
        builder.push_spec_with_parallel_support(create_read_file_tool(), true);
        builder.register_handler("read_file", read_file_handler);
    }

    if config
        .experimental_supported_tools
        .iter()
        .any(|tool| tool == "list_dir")
    {
        let list_dir_handler = Arc::new(ListDirHandler);
        builder.push_spec_with_parallel_support(create_list_dir_tool(), true);
        builder.register_handler("list_dir", list_dir_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"test_sync_tool".to_string())
    {
        let test_sync_handler = Arc::new(TestSyncHandler);
        builder.push_spec_with_parallel_support(create_test_sync_tool(), true);
        builder.register_handler("test_sync_tool", test_sync_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"write_file".to_string())
    {
        let write_file_handler = Arc::new(WriteFileHandler);
        builder.push_spec(create_write_file_tool());
        builder.register_handler("write_file", write_file_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"web_fetch".to_string())
    {
        let web_fetch_handler = Arc::new(WebFetchHandler);
        builder.push_spec_with_parallel_support(create_web_fetch_tool(), true);
        builder.register_handler("web_fetch", web_fetch_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"web_search_provider".to_string())
    {
        let web_search_handler = Arc::new(WebSearchHandler);
        builder.push_spec(create_web_search_provider_tool());
        builder.register_handler("web_search_provider", web_search_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"message".to_string())
    {
        let message_handler = Arc::new(MessageHandler);
        builder.push_spec(create_message_tool());
        builder.register_handler("message", message_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"sessions".to_string())
    {
        let sessions_handler = Arc::new(SessionsHandler);
        builder.push_spec(create_sessions_list_tool());
        builder.push_spec(create_sessions_history_tool());
        builder.push_spec(create_sessions_send_tool());
        builder.push_spec(create_session_status_tool());
        builder.register_handler("sessions_list", sessions_handler.clone());
        builder.register_handler("sessions_history", sessions_handler.clone());
        builder.register_handler("sessions_send", sessions_handler.clone());
        builder.register_handler("session_status", sessions_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"image_analyze".to_string())
    {
        let image_analyze_handler = Arc::new(ImageAnalyzeHandler);
        builder.push_spec(create_image_analyze_tool());
        builder.register_handler("image_analyze", image_analyze_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"cron".to_string())
    {
        let cron_handler = Arc::new(CronHandler::new());
        builder.push_spec(create_cron_tool());
        builder.register_handler("cron", cron_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"tts".to_string())
    {
        let tts_handler = Arc::new(TtsHandler);
        builder.push_spec(create_tts_tool());
        builder.register_handler("tts", tts_handler);
    }

    // --- Phase B-H: New tool registrations ---

    if config
        .experimental_supported_tools
        .contains(&"process".to_string())
    {
        let process_handler = Arc::new(ProcessHandler);
        builder.push_spec(create_process_tool());
        builder.register_handler("process", process_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"discord_actions".to_string())
    {
        let discord_actions_handler = Arc::new(DiscordActionsHandler);
        builder.push_spec(create_discord_actions_tool());
        builder.register_handler("discord_actions", discord_actions_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"slack_actions".to_string())
    {
        let slack_actions_handler = Arc::new(SlackActionsHandler);
        builder.push_spec(create_slack_actions_tool());
        builder.register_handler("slack_actions", slack_actions_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"telegram_actions".to_string())
    {
        let telegram_actions_handler = Arc::new(TelegramActionsHandler);
        builder.push_spec(create_telegram_actions_tool());
        builder.register_handler("telegram_actions", telegram_actions_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"sessions_spawn".to_string())
    {
        let sessions_spawn_handler = Arc::new(SessionsSpawnHandler);
        builder.push_spec(create_sessions_spawn_tool());
        builder.register_handler("sessions_spawn", sessions_spawn_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"gateway_status".to_string())
    {
        let gateway_status_handler = Arc::new(GatewayStatusHandler);
        builder.push_spec(create_gateway_status_tool());
        builder.register_handler("gateway_status", gateway_status_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"memory".to_string())
    {
        let memory_handler = Arc::new(MemoryHandler);
        builder.push_spec(create_memory_tool());
        builder.register_handler("memory", memory_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"md_memory".to_string())
    {
        let md_memory_handler = Arc::new(MdMemoryHandler);
        builder.push_spec(create_md_memory_tool());
        builder.register_handler("md_memory", md_memory_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"image_generate".to_string())
    {
        let image_generate_handler = Arc::new(ImageGenerateHandler);
        builder.push_spec(create_image_generate_tool());
        builder.register_handler("image_generate", image_generate_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"agents_list".to_string())
    {
        let agents_list_handler = Arc::new(AgentsListHandler);
        builder.push_spec(create_agents_list_tool());
        builder.register_handler("agents_list", agents_list_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"nodes".to_string())
    {
        let nodes_handler = Arc::new(NodesHandler);
        builder.push_spec(create_nodes_tool());
        builder.register_handler("nodes", nodes_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"browser".to_string())
    {
        let browser_handler = Arc::new(BrowserHandler);
        builder.push_spec(create_browser_tool());
        builder.register_handler("browser", browser_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"whatsapp_actions".to_string())
    {
        let whatsapp_actions_handler = Arc::new(WhatsAppActionsHandler);
        builder.push_spec(create_whatsapp_actions_tool());
        builder.register_handler("whatsapp_actions", whatsapp_actions_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"canvas".to_string())
    {
        let canvas_handler = Arc::new(CanvasHandler);
        builder.push_spec(create_canvas_tool());
        builder.register_handler("canvas", canvas_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"gateway".to_string())
    {
        let gateway_tool_handler = Arc::new(GatewayToolHandler);
        builder.push_spec(create_gateway_tool());
        builder.register_handler("gateway", gateway_tool_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"channel_tools".to_string())
    {
        let channel_tools_handler = Arc::new(ChannelToolsHandler);
        builder.push_spec(create_channel_tools_tool());
        builder.register_handler("channel_tools", channel_tools_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"llm_task".to_string())
    {
        let llm_task_handler = Arc::new(LlmTaskHandler);
        builder.push_spec(create_llm_task_tool());
        builder.register_handler("llm_task", llm_task_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"agent_step".to_string())
    {
        let agent_step_handler = Arc::new(AgentStepHandler);
        builder.push_spec(create_agent_step_tool());
        builder.register_handler("agent_step", agent_step_handler);
    }

    if config
        .experimental_supported_tools
        .contains(&"sessions_send_a2a".to_string())
    {
        let sessions_send_a2a_handler = Arc::new(SessionsSendA2AHandler);
        builder.push_spec(create_sessions_send_a2a_tool());
        builder.register_handler("sessions_send_a2a", sessions_send_a2a_handler);
    }

    match config.web_search_mode {
        Some(WebSearchMode::Cached) => {
            builder.push_spec(ToolSpec::WebSearch {
                external_web_access: Some(false),
            });
        }
        Some(WebSearchMode::Live) => {
            builder.push_spec(ToolSpec::WebSearch {
                external_web_access: Some(true),
            });
        }
        Some(WebSearchMode::Disabled) | None => {}
    }

    builder.push_spec_with_parallel_support(create_view_image_tool(), true);
    builder.register_handler("view_image", view_image_handler);

    if config.collab_tools {
        let collab_handler = Arc::new(CollabHandler);
        builder.push_spec(create_spawn_agent_tool());
        builder.push_spec(create_send_input_tool());
        builder.push_spec(create_wait_tool());
        builder.push_spec(create_close_agent_tool());
        builder.register_handler("spawn_agent", collab_handler.clone());
        builder.register_handler("send_input", collab_handler.clone());
        builder.register_handler("wait", collab_handler.clone());
        builder.register_handler("close_agent", collab_handler);
    }

    if let Some(mcp_tools) = mcp_tools {
        let mut entries: Vec<(String, rmcp::model::Tool)> = mcp_tools.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        for (name, tool) in entries.into_iter() {
            match mcp_tool_to_openai_tool(name.clone(), tool.clone()) {
                Ok(converted_tool) => {
                    builder.push_spec(ToolSpec::Function(converted_tool));
                    builder.register_handler(name, mcp_handler.clone());
                }
                Err(e) => {
                    tracing::error!("Failed to convert {name:?} MCP tool to OpenAI tool: {e:?}");
                }
            }
        }
    }

    if !dynamic_tools.is_empty() {
        for tool in dynamic_tools {
            match dynamic_tool_to_openai_tool(tool) {
                Ok(converted_tool) => {
                    builder.push_spec(ToolSpec::Function(converted_tool));
                    builder.register_handler(tool.name.clone(), dynamic_tool_handler.clone());
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to convert dynamic tool {:?} to OpenAI tool: {e:?}",
                        tool.name
                    );
                }
            }
        }
    }

    builder
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::client_common::tools::FreeformTool;
    use crate::config::test_config;
    use crate::models_manager::manager::ModelsManager;
    use crate::tools::registry::ConfiguredToolSpec;

    fn mcp_tool(
        name: &str,
        description: &str,
        input_schema: serde_json::Value,
    ) -> rmcp::model::Tool {
        rmcp::model::Tool {
            name: name.to_string().into(),
            title: None,
            description: Some(description.to_string().into()),
            input_schema: std::sync::Arc::new(rmcp::model::object(input_schema)),
            output_schema: None,
            annotations: None,
            icons: None,
            meta: None,
        }
    }

    #[test]
    fn mcp_tool_to_openai_tool_inserts_empty_properties() {
        let mut schema = rmcp::model::JsonObject::new();
        schema.insert("type".to_string(), serde_json::json!("object"));

        let tool = rmcp::model::Tool {
            name: "no_props".to_string().into(),
            title: None,
            description: Some("No properties".to_string().into()),
            input_schema: std::sync::Arc::new(schema),
            output_schema: None,
            annotations: None,
            icons: None,
            meta: None,
        };

        let openai_tool =
            mcp_tool_to_openai_tool("server/no_props".to_string(), tool).expect("convert tool");
        let parameters = serde_json::to_value(openai_tool.parameters).expect("serialize schema");

        assert_eq!(parameters.get("properties"), Some(&serde_json::json!({})));
    }

    fn tool_name(tool: &ToolSpec) -> &str {
        match tool {
            ToolSpec::Function(ResponsesApiTool { name, .. }) => name,
            ToolSpec::LocalShell {} => "local_shell",
            ToolSpec::WebSearch { .. } => "web_search",
            ToolSpec::Freeform(FreeformTool { name, .. }) => name,
        }
    }

    // Avoid order-based assertions; compare via set containment instead.
    fn assert_contains_tool_names(tools: &[ConfiguredToolSpec], expected_subset: &[&str]) {
        use std::collections::HashSet;
        let mut names = HashSet::new();
        let mut duplicates = Vec::new();
        for name in tools.iter().map(|t| tool_name(&t.spec)) {
            if !names.insert(name) {
                duplicates.push(name);
            }
        }
        assert!(
            duplicates.is_empty(),
            "duplicate tool entries detected: {duplicates:?}"
        );
        for expected in expected_subset {
            assert!(
                names.contains(expected),
                "expected tool {expected} to be present; had: {names:?}"
            );
        }
    }

    fn shell_tool_name(config: &ToolsConfig) -> Option<&'static str> {
        match config.shell_type {
            ConfigShellToolType::Default => Some("shell"),
            ConfigShellToolType::Local => Some("local_shell"),
            ConfigShellToolType::UnifiedExec => None,
            ConfigShellToolType::Disabled => None,
            ConfigShellToolType::ShellCommand => Some("shell_command"),
        }
    }

    fn find_tool<'a>(
        tools: &'a [ConfiguredToolSpec],
        expected_name: &str,
    ) -> &'a ConfiguredToolSpec {
        tools
            .iter()
            .find(|tool| tool_name(&tool.spec) == expected_name)
            .unwrap_or_else(|| panic!("expected tool {expected_name}"))
    }

    fn strip_descriptions_schema(schema: &mut JsonSchema) {
        match schema {
            JsonSchema::Boolean { description }
            | JsonSchema::String { description }
            | JsonSchema::Number { description } => {
                *description = None;
            }
            JsonSchema::Array { items, description } => {
                strip_descriptions_schema(items);
                *description = None;
            }
            JsonSchema::Object {
                properties,
                required: _,
                additional_properties,
            } => {
                for v in properties.values_mut() {
                    strip_descriptions_schema(v);
                }
                if let Some(AdditionalProperties::Schema(s)) = additional_properties {
                    strip_descriptions_schema(s);
                }
            }
        }
    }

    fn strip_descriptions_tool(spec: &mut ToolSpec) {
        match spec {
            ToolSpec::Function(ResponsesApiTool { parameters, .. }) => {
                strip_descriptions_schema(parameters);
            }
            ToolSpec::Freeform(_) | ToolSpec::LocalShell {} | ToolSpec::WebSearch { .. } => {}
        }
    }

    #[test]
    fn test_full_toolset_specs_for_gpt5_savfox_unified_exec_web_search() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("gpt-5-savfox", &config);
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        features.enable(Feature::CollaborationModes);
        let config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Live),
        });
        let (tools, _) = build_specs(&config, None, &[]).build();

        // Build actual map name -> spec
        use std::collections::{BTreeMap, HashSet};
        let mut actual: BTreeMap<String, ToolSpec> = BTreeMap::from([]);
        let mut duplicate_names = Vec::new();
        for t in &tools {
            let name = tool_name(&t.spec).to_string();
            if actual.insert(name.clone(), t.spec.clone()).is_some() {
                duplicate_names.push(name);
            }
        }
        assert!(
            duplicate_names.is_empty(),
            "duplicate tool entries detected: {duplicate_names:?}"
        );

        // Build expected from the same helpers used by the builder.
        let mut expected: BTreeMap<String, ToolSpec> = BTreeMap::from([]);
        for spec in [
            create_exec_command_tool(true),
            create_write_stdin_tool(),
            create_list_mcp_resources_tool(),
            create_list_mcp_resource_templates_tool(),
            create_read_mcp_resource_tool(),
            PLAN_TOOL.clone(),
            create_request_user_input_tool(),
            create_apply_patch_freeform_tool(),
            ToolSpec::WebSearch {
                external_web_access: Some(true),
            },
            create_view_image_tool(),
        ] {
            expected.insert(tool_name(&spec).to_string(), spec);
        }

        // Exact name set match — this is the only test allowed to fail when tools change.
        let actual_names: HashSet<_> = actual.keys().cloned().collect();
        let expected_names: HashSet<_> = expected.keys().cloned().collect();
        assert_eq!(actual_names, expected_names, "tool name set mismatch");

        // Compare specs ignoring human-readable descriptions.
        for name in expected.keys() {
            let mut a = actual.get(name).expect("present").clone();
            let mut e = expected.get(name).expect("present").clone();
            strip_descriptions_tool(&mut a);
            strip_descriptions_tool(&mut e);
            assert_eq!(a, e, "spec mismatch for {name}");
        }
    }

    #[test]
    fn test_build_specs_collab_tools_enabled() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("gpt-5-savfox", &config);
        let mut features = Features::with_defaults();
        features.enable(Feature::Collab);
        features.enable(Feature::CollaborationModes);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });
        let (tools, _) = build_specs(&tools_config, None, &[]).build();
        assert_contains_tool_names(
            &tools,
            &["spawn_agent", "send_input", "wait", "close_agent"],
        );
    }

    #[test]
    fn request_user_input_requires_collaboration_modes_feature() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("gpt-5-savfox", &config);
        let mut features = Features::with_defaults();
        features.disable(Feature::CollaborationModes);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });
        let (tools, _) = build_specs(&tools_config, None, &[]).build();
        assert!(
            !tools.iter().any(|t| t.spec.name() == "request_user_input"),
            "request_user_input should be disabled when collaboration_modes feature is off"
        );

        features.enable(Feature::CollaborationModes);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });
        let (tools, _) = build_specs(&tools_config, None, &[]).build();
        assert_contains_tool_names(&tools, &["request_user_input"]);
    }

    fn assert_model_tools(
        model_slug: &str,
        features: &Features,
        web_search_mode: Option<WebSearchMode>,
        expected_tools: &[&str],
    ) {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline(model_slug, &config);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features,
            web_search_mode,
        });
        let (tools, _) = build_specs(&tools_config, Some(HashMap::new()), &[]).build();
        let tool_names = tools.iter().map(|t| t.spec.name()).collect::<Vec<_>>();
        assert_eq!(&tool_names, &expected_tools,);
    }

    #[test]
    fn web_search_mode_cached_sets_external_web_access_false() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("gpt-5-savfox", &config);
        let features = Features::with_defaults();

        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });
        let (tools, _) = build_specs(&tools_config, None, &[]).build();

        let tool = find_tool(&tools, "web_search");
        assert_eq!(
            tool.spec,
            ToolSpec::WebSearch {
                external_web_access: Some(false),
            }
        );
    }

    #[test]
    fn web_search_mode_live_sets_external_web_access_true() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("gpt-5-savfox", &config);
        let features = Features::with_defaults();

        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Live),
        });
        let (tools, _) = build_specs(&tools_config, None, &[]).build();

        let tool = find_tool(&tools, "web_search");
        assert_eq!(
            tool.spec,
            ToolSpec::WebSearch {
                external_web_access: Some(true),
            }
        );
    }

    #[test]
    fn test_build_specs_gpt5_savfox_default() {
        let mut features = Features::with_defaults();
        features.enable(Feature::CollaborationModes);
        assert_model_tools(
            "gpt-5-savfox",
            &features,
            Some(WebSearchMode::Cached),
            &[
                "shell_command",
                "list_mcp_resources",
                "list_mcp_resource_templates",
                "read_mcp_resource",
                "update_plan",
                "request_user_input",
                "apply_patch",
                "web_search",
                "view_image",
            ],
        );
    }

    #[test]
    fn test_build_specs_gpt51_savfox_default() {
        let mut features = Features::with_defaults();
        features.enable(Feature::CollaborationModes);
        assert_model_tools(
            "gpt-5.1-savfox",
            &features,
            Some(WebSearchMode::Cached),
            &[
                "shell_command",
                "list_mcp_resources",
                "list_mcp_resource_templates",
                "read_mcp_resource",
                "update_plan",
                "request_user_input",
                "apply_patch",
                "web_search",
                "view_image",
            ],
        );
    }

    #[test]
    fn test_build_specs_gpt5_savfox_unified_exec_web_search() {
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        features.enable(Feature::CollaborationModes);
        assert_model_tools(
            "gpt-5-savfox",
            &features,
            Some(WebSearchMode::Live),
            &[
                "exec_command",
                "write_stdin",
                "list_mcp_resources",
                "list_mcp_resource_templates",
                "read_mcp_resource",
                "update_plan",
                "request_user_input",
                "apply_patch",
                "web_search",
                "view_image",
            ],
        );
    }

    #[test]
    fn test_build_specs_gpt51_savfox_unified_exec_web_search() {
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        features.enable(Feature::CollaborationModes);
        assert_model_tools(
            "gpt-5.1-savfox",
            &features,
            Some(WebSearchMode::Live),
            &[
                "exec_command",
                "write_stdin",
                "list_mcp_resources",
                "list_mcp_resource_templates",
                "read_mcp_resource",
                "update_plan",
                "request_user_input",
                "apply_patch",
                "web_search",
                "view_image",
            ],
        );
    }

    #[test]
    fn test_savfox_mini_defaults() {
        let mut features = Features::with_defaults();
        features.enable(Feature::CollaborationModes);
        assert_model_tools(
            "savfox-mini-latest",
            &features,
            Some(WebSearchMode::Cached),
            &[
                "local_shell",
                "list_mcp_resources",
                "list_mcp_resource_templates",
                "read_mcp_resource",
                "update_plan",
                "request_user_input",
                "web_search",
                "view_image",
            ],
        );
    }

    #[test]
    fn test_savfox_5_1_mini_defaults() {
        let mut features = Features::with_defaults();
        features.enable(Feature::CollaborationModes);
        assert_model_tools(
            "gpt-5.1-savfox-mini",
            &features,
            Some(WebSearchMode::Cached),
            &[
                "shell_command",
                "list_mcp_resources",
                "list_mcp_resource_templates",
                "read_mcp_resource",
                "update_plan",
                "request_user_input",
                "apply_patch",
                "web_search",
                "view_image",
            ],
        );
    }

    #[test]
    fn test_gpt_5_defaults() {
        let mut features = Features::with_defaults();
        features.enable(Feature::CollaborationModes);
        assert_model_tools(
            "gpt-5",
            &features,
            Some(WebSearchMode::Cached),
            &[
                "shell",
                "list_mcp_resources",
                "list_mcp_resource_templates",
                "read_mcp_resource",
                "update_plan",
                "request_user_input",
                "web_search",
                "view_image",
            ],
        );
    }

    #[test]
    fn test_gpt_5_1_defaults() {
        let mut features = Features::with_defaults();
        features.enable(Feature::CollaborationModes);
        assert_model_tools(
            "gpt-5.1",
            &features,
            Some(WebSearchMode::Cached),
            &[
                "shell_command",
                "list_mcp_resources",
                "list_mcp_resource_templates",
                "read_mcp_resource",
                "update_plan",
                "request_user_input",
                "apply_patch",
                "web_search",
                "view_image",
            ],
        );
    }

    #[test]
    fn test_exp_5_1_defaults() {
        let mut features = Features::with_defaults();
        features.enable(Feature::CollaborationModes);
        assert_model_tools(
            "exp-5.1",
            &features,
            Some(WebSearchMode::Cached),
            &[
                "exec_command",
                "write_stdin",
                "list_mcp_resources",
                "list_mcp_resource_templates",
                "read_mcp_resource",
                "update_plan",
                "request_user_input",
                "apply_patch",
                "web_search",
                "view_image",
            ],
        );
    }

    #[test]
    fn test_savfox_mini_unified_exec_web_search() {
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        features.enable(Feature::CollaborationModes);
        assert_model_tools(
            "savfox-mini-latest",
            &features,
            Some(WebSearchMode::Live),
            &[
                "exec_command",
                "write_stdin",
                "list_mcp_resources",
                "list_mcp_resource_templates",
                "read_mcp_resource",
                "update_plan",
                "request_user_input",
                "web_search",
                "view_image",
            ],
        );
    }

    #[test]
    fn test_build_specs_default_shell_present() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("o3", &config);
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Live),
        });
        let (tools, _) = build_specs(&tools_config, Some(HashMap::new()), &[]).build();

        // Only check the shell variant and a couple of core tools.
        let mut subset = vec!["exec_command", "write_stdin", "update_plan"];
        if let Some(shell_tool) = shell_tool_name(&tools_config) {
            subset.push(shell_tool);
        }
        assert_contains_tool_names(&tools, &subset);
    }

    #[test]
    #[ignore]
    fn test_parallel_support_flags() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("gpt-5-savfox", &config);
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });
        let (tools, _) = build_specs(&tools_config, None, &[]).build();

        assert!(!find_tool(&tools, "exec_command").supports_parallel_tool_calls);
        assert!(!find_tool(&tools, "write_stdin").supports_parallel_tool_calls);
        assert!(find_tool(&tools, "grep_files").supports_parallel_tool_calls);
        assert!(find_tool(&tools, "list_dir").supports_parallel_tool_calls);
        assert!(find_tool(&tools, "read_file").supports_parallel_tool_calls);
    }

    #[test]
    fn test_test_model_info_includes_sync_tool() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("test-gpt-5-savfox", &config);
        let features = Features::with_defaults();
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });
        let (tools, _) = build_specs(&tools_config, None, &[]).build();

        assert!(
            tools
                .iter()
                .any(|tool| tool_name(&tool.spec) == "test_sync_tool")
        );
        assert!(
            tools
                .iter()
                .any(|tool| tool_name(&tool.spec) == "read_file")
        );
        assert!(
            tools
                .iter()
                .any(|tool| tool_name(&tool.spec) == "grep_files")
        );
        assert!(tools.iter().any(|tool| tool_name(&tool.spec) == "list_dir"));
    }

    #[test]
    fn test_build_specs_mcp_tools_converted() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("o3", &config);
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Live),
        });
        let (tools, _) = build_specs(
            &tools_config,
            Some(HashMap::from([(
                "test_server/do_something_cool".to_string(),
                mcp_tool(
                    "do_something_cool",
                    "Do something cool",
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                            "string_argument": { "type": "string" },
                            "number_argument": { "type": "number" },
                            "object_argument": {
                                "type": "object",
                                "properties": {
                                    "string_property": { "type": "string" },
                                    "number_property": { "type": "number" },
                                },
                                "required": ["string_property", "number_property"],
                                "additionalProperties": false,
                            },
                        },
                    }),
                ),
            )])),
            &[],
        )
        .build();

        let tool = find_tool(&tools, "test_server/do_something_cool");
        assert_eq!(
            &tool.spec,
            &ToolSpec::Function(ResponsesApiTool {
                name: "test_server/do_something_cool".to_string(),
                parameters: JsonSchema::Object {
                    properties: BTreeMap::from([
                        (
                            "string_argument".to_string(),
                            JsonSchema::String { description: None }
                        ),
                        (
                            "number_argument".to_string(),
                            JsonSchema::Number { description: None }
                        ),
                        (
                            "object_argument".to_string(),
                            JsonSchema::Object {
                                properties: BTreeMap::from([
                                    (
                                        "string_property".to_string(),
                                        JsonSchema::String { description: None }
                                    ),
                                    (
                                        "number_property".to_string(),
                                        JsonSchema::Number { description: None }
                                    ),
                                ]),
                                required: Some(vec![
                                    "string_property".to_string(),
                                    "number_property".to_string(),
                                ]),
                                additional_properties: Some(false.into()),
                            },
                        ),
                    ]),
                    required: None,
                    additional_properties: None,
                },
                description: "Do something cool".to_string(),
                strict: false,
            })
        );
    }

    #[test]
    fn test_build_specs_mcp_tools_sorted_by_name() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("o3", &config);
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });

        // Intentionally construct a map with keys that would sort alphabetically.
        let tools_map: HashMap<String, rmcp::model::Tool> = HashMap::from([
            (
                "test_server/do".to_string(),
                mcp_tool("a", "a", serde_json::json!({"type": "object"})),
            ),
            (
                "test_server/something".to_string(),
                mcp_tool("b", "b", serde_json::json!({"type": "object"})),
            ),
            (
                "test_server/cool".to_string(),
                mcp_tool("c", "c", serde_json::json!({"type": "object"})),
            ),
        ]);

        let (tools, _) = build_specs(&tools_config, Some(tools_map), &[]).build();

        // Only assert that the MCP tools themselves are sorted by fully-qualified name.
        let mcp_names: Vec<_> = tools
            .iter()
            .map(|t| tool_name(&t.spec).to_string())
            .filter(|n| n.starts_with("test_server/"))
            .collect();
        let expected = vec![
            "test_server/cool".to_string(),
            "test_server/do".to_string(),
            "test_server/something".to_string(),
        ];
        assert_eq!(mcp_names, expected);
    }

    #[test]
    fn test_mcp_tool_property_missing_type_defaults_to_string() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("gpt-5-savfox", &config);
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });

        let (tools, _) = build_specs(
            &tools_config,
            Some(HashMap::from([(
                "dash/search".to_string(),
                mcp_tool(
                    "search",
                    "Search docs",
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                            "query": {"description": "search query"}
                        }
                    }),
                ),
            )])),
            &[],
        )
        .build();

        let tool = find_tool(&tools, "dash/search");
        assert_eq!(
            tool.spec,
            ToolSpec::Function(ResponsesApiTool {
                name: "dash/search".to_string(),
                parameters: JsonSchema::Object {
                    properties: BTreeMap::from([(
                        "query".to_string(),
                        JsonSchema::String {
                            description: Some("search query".to_string())
                        }
                    )]),
                    required: None,
                    additional_properties: None,
                },
                description: "Search docs".to_string(),
                strict: false,
            })
        );
    }

    #[test]
    fn test_mcp_tool_integer_normalized_to_number() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("gpt-5-savfox", &config);
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });

        let (tools, _) = build_specs(
            &tools_config,
            Some(HashMap::from([(
                "dash/paginate".to_string(),
                mcp_tool(
                    "paginate",
                    "Pagination",
                    serde_json::json!({
                        "type": "object",
                        "properties": {"page": {"type": "integer"}}
                    }),
                ),
            )])),
            &[],
        )
        .build();

        let tool = find_tool(&tools, "dash/paginate");
        assert_eq!(
            tool.spec,
            ToolSpec::Function(ResponsesApiTool {
                name: "dash/paginate".to_string(),
                parameters: JsonSchema::Object {
                    properties: BTreeMap::from([(
                        "page".to_string(),
                        JsonSchema::Number { description: None }
                    )]),
                    required: None,
                    additional_properties: None,
                },
                description: "Pagination".to_string(),
                strict: false,
            })
        );
    }

    #[test]
    fn test_mcp_tool_array_without_items_gets_default_string_items() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("gpt-5-savfox", &config);
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        features.enable(Feature::ApplyPatchFreeform);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });

        let (tools, _) = build_specs(
            &tools_config,
            Some(HashMap::from([(
                "dash/tags".to_string(),
                mcp_tool(
                    "tags",
                    "Tags",
                    serde_json::json!({
                        "type": "object",
                        "properties": {"tags": {"type": "array"}}
                    }),
                ),
            )])),
            &[],
        )
        .build();

        let tool = find_tool(&tools, "dash/tags");
        assert_eq!(
            tool.spec,
            ToolSpec::Function(ResponsesApiTool {
                name: "dash/tags".to_string(),
                parameters: JsonSchema::Object {
                    properties: BTreeMap::from([(
                        "tags".to_string(),
                        JsonSchema::Array {
                            items: Box::new(JsonSchema::String { description: None }),
                            description: None
                        }
                    )]),
                    required: None,
                    additional_properties: None,
                },
                description: "Tags".to_string(),
                strict: false,
            })
        );
    }

    #[test]
    fn test_mcp_tool_anyof_defaults_to_string() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("gpt-5-savfox", &config);
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });

        let (tools, _) = build_specs(
            &tools_config,
            Some(HashMap::from([(
                "dash/value".to_string(),
                mcp_tool(
                    "value",
                    "AnyOf Value",
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                            "value": {"anyOf": [{"type": "string"}, {"type": "number"}]}
                        }
                    }),
                ),
            )])),
            &[],
        )
        .build();

        let tool = find_tool(&tools, "dash/value");
        assert_eq!(
            tool.spec,
            ToolSpec::Function(ResponsesApiTool {
                name: "dash/value".to_string(),
                parameters: JsonSchema::Object {
                    properties: BTreeMap::from([(
                        "value".to_string(),
                        JsonSchema::String { description: None }
                    )]),
                    required: None,
                    additional_properties: None,
                },
                description: "AnyOf Value".to_string(),
                strict: false,
            })
        );
    }

    #[test]
    fn test_shell_tool() {
        let tool = super::create_shell_tool(true);
        let ToolSpec::Function(ResponsesApiTool {
            description, name, ..
        }) = &tool
        else {
            panic!("expected function tool");
        };
        assert_eq!(name, "shell");

        let expected = if cfg!(windows) {
            r#"Runs a Powershell command (Windows) and returns its output. Arguments to `shell` will be passed to CreateProcessW(). Most commands should be prefixed with ["powershell.exe", "-Command"].
        
Examples of valid command strings:

- ls -a (show hidden): ["powershell.exe", "-Command", "Get-ChildItem -Force"]
- recursive find by name: ["powershell.exe", "-Command", "Get-ChildItem -Recurse -Filter *.py"]
- recursive grep: ["powershell.exe", "-Command", "Get-ChildItem -Path C:\\myrepo -Recurse | Select-String -Pattern 'TODO' -CaseSensitive"]
- ps aux | grep python: ["powershell.exe", "-Command", "Get-Process | Where-Object { $_.ProcessName -like '*python*' }"]
- setting an env var: ["powershell.exe", "-Command", "$env:FOO='bar'; echo $env:FOO"]
- running an inline Python script: ["powershell.exe", "-Command", "@'\\nprint('Hello, world!')\\n'@ | python -"]"#
        } else {
            r#"Runs a shell command and returns its output.
- The arguments to `shell` will be passed to execvp(). Most terminal commands should be prefixed with ["bash", "-lc"].
- Always set the `workdir` param when using the shell function. Do not use `cd` unless absolutely necessary."#
        }.to_string();
        assert_eq!(description, &expected);
    }

    #[test]
    fn test_shell_command_tool() {
        let tool = super::create_shell_command_tool(true);
        let ToolSpec::Function(ResponsesApiTool {
            description, name, ..
        }) = &tool
        else {
            panic!("expected function tool");
        };
        assert_eq!(name, "shell_command");

        let expected = if cfg!(windows) {
            r#"Runs a Powershell command (Windows) and returns its output.
        
Examples of valid command strings:

- ls -a (show hidden): "Get-ChildItem -Force"
- recursive find by name: "Get-ChildItem -Recurse -Filter *.py"
- recursive grep: "Get-ChildItem -Path C:\\myrepo -Recurse | Select-String -Pattern 'TODO' -CaseSensitive"
- ps aux | grep python: "Get-Process | Where-Object { $_.ProcessName -like '*python*' }"
- setting an env var: "$env:FOO='bar'; echo $env:FOO"
- running an inline Python script: "@'\\nprint('Hello, world!')\\n'@ | python -"#.to_string()
        } else {
            r#"Runs a shell command and returns its output.
- Always set the `workdir` param when using the shell_command function. Do not use `cd` unless absolutely necessary."#.to_string()
        };
        assert_eq!(description, &expected);
    }

    #[test]
    fn test_get_openai_tools_mcp_tools_with_additional_properties_schema() {
        let config = test_config();
        let model_info = ModelsManager::construct_model_info_offline("gpt-5-savfox", &config);
        let mut features = Features::with_defaults();
        features.enable(Feature::UnifiedExec);
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            features: &features,
            web_search_mode: Some(WebSearchMode::Cached),
        });
        let (tools, _) = build_specs(
            &tools_config,
            Some(HashMap::from([(
                "test_server/do_something_cool".to_string(),
                mcp_tool(
                    "do_something_cool",
                    "Do something cool",
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                            "string_argument": {"type": "string"},
                            "number_argument": {"type": "number"},
                            "object_argument": {
                                "type": "object",
                                "properties": {
                                    "string_property": {"type": "string"},
                                    "number_property": {"type": "number"}
                                },
                                "required": ["string_property", "number_property"],
                                "additionalProperties": {
                                    "type": "object",
                                    "properties": {
                                        "addtl_prop": {"type": "string"}
                                    },
                                    "required": ["addtl_prop"],
                                    "additionalProperties": false
                                }
                            }
                        }
                    }),
                ),
            )])),
            &[],
        )
        .build();

        let tool = find_tool(&tools, "test_server/do_something_cool");
        assert_eq!(
            tool.spec,
            ToolSpec::Function(ResponsesApiTool {
                name: "test_server/do_something_cool".to_string(),
                parameters: JsonSchema::Object {
                    properties: BTreeMap::from([
                        (
                            "string_argument".to_string(),
                            JsonSchema::String { description: None }
                        ),
                        (
                            "number_argument".to_string(),
                            JsonSchema::Number { description: None }
                        ),
                        (
                            "object_argument".to_string(),
                            JsonSchema::Object {
                                properties: BTreeMap::from([
                                    (
                                        "string_property".to_string(),
                                        JsonSchema::String { description: None }
                                    ),
                                    (
                                        "number_property".to_string(),
                                        JsonSchema::Number { description: None }
                                    ),
                                ]),
                                required: Some(vec![
                                    "string_property".to_string(),
                                    "number_property".to_string(),
                                ]),
                                additional_properties: Some(
                                    JsonSchema::Object {
                                        properties: BTreeMap::from([(
                                            "addtl_prop".to_string(),
                                            JsonSchema::String { description: None }
                                        ),]),
                                        required: Some(vec!["addtl_prop".to_string(),]),
                                        additional_properties: Some(false.into()),
                                    }
                                    .into()
                                ),
                            },
                        ),
                    ]),
                    required: None,
                    additional_properties: None,
                },
                description: "Do something cool".to_string(),
                strict: false,
            })
        );
    }

    #[test]
    fn chat_tools_include_top_level_name() {
        let properties =
            BTreeMap::from([("foo".to_string(), JsonSchema::String { description: None })]);
        let tools = vec![ToolSpec::Function(ResponsesApiTool {
            name: "demo".to_string(),
            description: "A demo tool".to_string(),
            strict: false,
            parameters: JsonSchema::Object {
                properties,
                required: None,
                additional_properties: None,
            },
        })];

        let responses_json = create_tools_json_for_responses_api(&tools).unwrap();
        assert_eq!(
            responses_json,
            vec![json!({
                "type": "function",
                "name": "demo",
                "description": "A demo tool",
                "strict": false,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "foo": { "type": "string" }
                    },
                },
            })]
        );

        let tools_json = create_tools_json_for_chat_completions_api(&tools).unwrap();

        assert_eq!(
            tools_json,
            vec![json!({
                "type": "function",
                "name": "demo",
                "function": {
                    "name": "demo",
                    "description": "A demo tool",
                    "strict": false,
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "foo": { "type": "string" }
                        },
                    },
                }
            })]
        );
    }
}
