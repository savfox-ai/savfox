mod access;
mod agents;
mod declarations;
mod execution;
mod orchestration;
mod platform;
mod renderers;
mod schema;

use std::collections::{BTreeMap, HashMap};

use savfox_protocol::config_types::WebSearchMode;
use savfox_protocol::dynamic_tools::DynamicToolSpec;
use savfox_protocol::openai_models::{ApplyPatchToolType, ConfigShellToolType, ModelInfo};
use serde::{Deserialize, Serialize};

use self::access::{
    create_grep_files_tool, create_list_dir_tool, create_read_file_tool, create_view_image_tool,
    create_web_fetch_tool, create_web_search_provider_tool, create_write_file_tool,
};
use self::agents::{
    create_agents_list_tool, create_close_agent_tool, create_request_user_input_tool,
    create_send_input_tool, create_spawn_agent_tool, create_test_sync_tool, create_wait_tool,
};
use self::declarations::{FunctionToolDecl, function_tool};
use self::execution::{
    create_exec_command_tool, create_shell_command_tool, create_shell_tool, create_write_stdin_tool,
};
use self::orchestration::{
    create_agent_step_tool, create_list_mcp_resource_templates_tool,
    create_list_mcp_resources_tool, create_read_mcp_resource_tool, create_session_status_tool,
    create_sessions_history_tool, create_sessions_list_tool, create_sessions_send_a2a_tool,
    create_sessions_send_tool, create_sessions_spawn_tool,
};
use self::platform::{
    create_channel_tools_tool, create_discord_actions_tool, create_message_tool,
    create_slack_actions_tool, create_telegram_actions_tool, create_whatsapp_actions_tool,
};
pub use self::renderers::create_tools_json_for_responses_api;
pub(crate) use self::renderers::{
    create_tools_json_for_anthropic_api, create_tools_json_for_chat_completions_api,
};
pub use self::schema::parse_tool_input_schema;
use self::schema::{dynamic_tool_to_openai_tool, mcp_tool_to_openai_tool};
use crate::client_common::tools::ToolSpec;
use crate::features::{Feature, Features};
use crate::tools::handlers::PLAN_TOOL;
use crate::tools::handlers::apply_patch::{
    create_apply_patch_freeform_tool, create_apply_patch_json_tool,
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
            if savfox_utils::pty::conpty_supported() {
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
        items: Box<Self>,

        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Object {
        properties: BTreeMap<String, Self>,
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

fn create_cron_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Action to perform: \"list\" (show all jobs), \"add\" (create a job), \
                     \"remove\" (delete a job), \"next\" (show next run times).".to_owned(),
                ),
            },
        ),
        (
            "name".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Name/identifier for the cron job (required for add, remove, next).".to_owned(),
                ),
            },
        ),
        (
            "schedule".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Cron schedule expression, e.g. \"0 0 * * * *\" (required for add). \
                     Uses 6-field format: sec min hour day month weekday.".to_owned(),
                ),
            },
        ),
        (
            "command".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Shell command to execute when the job triggers (required for add).".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "cron",
        description: "Manage scheduled cron jobs. Supports listing, adding, removing, and querying next run times for periodic tasks.",
        properties,
        required: &["action"],
    })
}

fn create_tts_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "text".to_owned(),
            JsonSchema::String {
                description: Some("Text to convert to speech.".to_owned()),
            },
        ),
        (
            "voice".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Voice name: \"alloy\" (default), \"echo\", \"fable\", \"onyx\", \"nova\", \"shimmer\".".to_owned(),
                ),
            },
        ),
        (
            "output_path".to_owned(),
            JsonSchema::String {
                description: Some("File path where the audio (MP3) will be saved.".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "tts",
        description: "Convert text to speech using the OpenAI TTS API. Requires OPENAI_API_KEY environment variable.",
        properties,
        required: &["text", "output_path"],
    })
}

fn create_image_analyze_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "path".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Path to the local image file to analyze.".to_owned(),
                ),
            },
        ),
        (
            "prompt".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Analysis prompt describing what to look for in the image.".to_owned(),
                ),
            },
        ),
        (
            "detail".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Detail level: \"low\" for a brief analysis, \"high\" (default) for a thorough analysis.".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "image_analyze",
        description: "Analyze a local image using the vision model. The image is loaded and injected into the conversation along with your analysis prompt.",
        properties,
        required: &["path", "prompt"],
    })
}

// === Phase B-H tool creation functions ===

fn create_process_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Action to perform: \"list\", \"poll\", \"read_log\", \"write\", \"send_keys\", \"kill\".".to_owned(),
                ),
            },
        ),
        (
            "process_id".to_owned(),
            JsonSchema::String {
                description: Some("Identifier of the target process.".to_owned()),
            },
        ),
        (
            "input".to_owned(),
            JsonSchema::String {
                description: Some("Text to write to stdin (for \"write\" action).".to_owned()),
            },
        ),
        (
            "keys".to_owned(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some(
                    "Key names to send (for \"send_keys\" action): \"ctrl-c\", \"ctrl-d\", \"enter\", \"tab\", \"up\", \"down\", \"left\", \"right\", \"escape\", \"f1\"-\"f4\", \"home\", \"end\".".to_owned(),
                ),
            },
        ),
        (
            "signal".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Signal to send (for \"kill\" action): \"SIGTERM\" (default) or \"SIGINT\".".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "process",
        description: "Manage background processes: list active processes, read their output, write to stdin, send key sequences, or terminate them.",
        properties,
        required: &["action"],
    })
}

fn create_gateway_status_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Gateway action: \"status\" (connected clients), \"config\" (configuration), \
                     \"health\" (health check), \"config.get\" (full config with schema), \
                     \"config.patch\" (merge update config), \"config.apply\" (replace config), \
                     \"restart\" (restart gateway).".to_owned(),
                ),
            },
        ),
        (
            "raw".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Config content (YAML/JSON) for config.patch and config.apply actions.".to_owned(),
                ),
            },
        ),
        (
            "note".to_owned(),
            JsonSchema::String {
                description: Some("Change description note for config operations.".to_owned()),
            },
        ),
        (
            "delay_ms".to_owned(),
            JsonSchema::Number {
                description: Some("Restart delay in milliseconds.".to_owned()),
            },
        ),
        (
            "reason".to_owned(),
            JsonSchema::String {
                description: Some("Reason for restart.".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "gateway_status",
        description: "Query and manage the gateway server: check status/health/config, update configuration (patch/apply), or restart the gateway.",
        properties,
        required: &["action"],
    })
}

fn create_memory_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Memory action: \"search\", \"get\", \"add\", \"delete\", \"list\".".to_owned(),
                ),
            },
        ),
        (
            "query".to_owned(),
            JsonSchema::String {
                description: Some("Search query text (for \"search\" action).".to_owned()),
            },
        ),
        (
            "key".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Memory entry key (for \"get\", \"add\", \"delete\").".to_owned(),
                ),
            },
        ),
        (
            "content".to_owned(),
            JsonSchema::String {
                description: Some("Memory entry content (for \"add\" action).".to_owned()),
            },
        ),
        (
            "tags".to_owned(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some("Tags to associate with the memory entry.".to_owned()),
            },
        ),
        (
            "limit".to_owned(),
            JsonSchema::Number {
                description: Some("Maximum results to return (default 10).".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "memory",
        description: "Persistent memory store with BM25 text search. Store, retrieve, search, and manage memory entries that persist across sessions.",
        properties,
        required: &["action"],
    })
}

fn create_md_memory_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Action: \"list\", \"get\", \"create\", \"update\", \"delete\", \"search\", \"promote\".".to_owned(),
                ),
            },
        ),
        (
            "slug".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Memory entry slug (kebab-case filename without .md).".to_owned(),
                ),
            },
        ),
        (
            "layer".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Memory layer: \"global\", \"project\", \"agent\", \"session\".".to_owned(),
                ),
            },
        ),
        (
            "target_layer".to_owned(),
            JsonSchema::String {
                description: Some("Target layer for \"promote\" action.".to_owned()),
            },
        ),
        (
            "content".to_owned(),
            JsonSchema::String {
                description: Some("Markdown content for \"create\" or \"update\".".to_owned()),
            },
        ),
        (
            "tags".to_owned(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some("Tags for the memory entry.".to_owned()),
            },
        ),
        (
            "priority".to_owned(),
            JsonSchema::Number {
                description: Some("Priority 1-10 (higher = more important, default 5).".to_owned()),
            },
        ),
        (
            "query".to_owned(),
            JsonSchema::String {
                description: Some("Search query text (for \"search\" action).".to_owned()),
            },
        ),
        (
            "limit".to_owned(),
            JsonSchema::Number {
                description: Some("Maximum results to return (default 10).".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "md_memory",
        description: "Markdown memory system with 4 layers (global, project, agent, session). Manage user-editable .md knowledge files that are injected into system prompts. Use this to store architecture decisions, coding conventions, and project knowledge.",
        properties,
        required: &["action"],
    })
}

fn create_image_generate_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "prompt".to_owned(),
            JsonSchema::String {
                description: Some("Image generation prompt describing what to create.".to_owned()),
            },
        ),
        (
            "output_path".to_owned(),
            JsonSchema::String {
                description: Some("File path where the generated image will be saved.".to_owned()),
            },
        ),
        (
            "size".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Image dimensions: \"1024x1024\" (default), \"1792x1024\", \"1024x1792\".".to_owned(),
                ),
            },
        ),
        (
            "model".to_owned(),
            JsonSchema::String {
                description: Some("Model to use: \"dall-e-3\" (default).".to_owned()),
            },
        ),
        (
            "quality".to_owned(),
            JsonSchema::String {
                description: Some("Quality: \"standard\" (default) or \"hd\".".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "image_generate",
        description: "Generate an image using the DALL-E API and save it to a file. Requires OPENAI_API_KEY environment variable.",
        properties,
        required: &["prompt", "output_path"],
    })
}

fn create_nodes_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Node action: \"list\", \"status\", \"run_command\", \"camera_capture\", \"get_location\", \"send_notification\".".to_owned(),
                ),
            },
        ),
        (
            "node_id".to_owned(),
            JsonSchema::String {
                description: Some("Target node identifier.".to_owned()),
            },
        ),
        (
            "command".to_owned(),
            JsonSchema::String {
                description: Some("Command to run on the node (for \"run_command\").".to_owned()),
            },
        ),
        (
            "message".to_owned(),
            JsonSchema::String {
                description: Some("Notification message text.".to_owned()),
            },
        ),
        (
            "title".to_owned(),
            JsonSchema::String {
                description: Some("Notification title.".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "nodes",
        description: "Interact with paired device nodes: list devices, check status, run commands, capture camera, get location, or send notifications.",
        properties,
        required: &["action"],
    })
}

fn create_browser_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Browser action: \"status\", \"start\", \"stop\", \"profiles\", \"tabs\", \
                     \"open\", \"focus\", \"close\", \"snapshot\", \"screenshot\", \"navigate\", \
                     \"console\", \"pdf\", \"upload\", \"dialog\", \"act\".".to_owned(),
                ),
            },
        ),
        (
            "profile".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Browser profile: \"default\", \"chrome\", or custom name.".to_owned(),
                ),
            },
        ),
        (
            "target_id".to_owned(),
            JsonSchema::String {
                description: Some("Tab/page target identifier.".to_owned()),
            },
        ),
        (
            "target_url".to_owned(),
            JsonSchema::String {
                description: Some("URL for navigate/open actions.".to_owned()),
            },
        ),
        (
            "format".to_owned(),
            JsonSchema::String {
                description: Some("Snapshot format: \"ai\" or \"aria\".".to_owned()),
            },
        ),
        (
            "refs".to_owned(),
            JsonSchema::String {
                description: Some("Ref strategy for snapshots: \"role\" or \"aria\".".to_owned()),
            },
        ),
        (
            "max_chars".to_owned(),
            JsonSchema::Number {
                description: Some("Maximum characters for snapshot output.".to_owned()),
            },
        ),
        (
            "selector".to_owned(),
            JsonSchema::String {
                description: Some("CSS selector for targeted operations.".to_owned()),
            },
        ),
        (
            "full_page".to_owned(),
            JsonSchema::Boolean {
                description: Some("Capture full page screenshot (default: false).".to_owned()),
            },
        ),
        (
            "image_type".to_owned(),
            JsonSchema::String {
                description: Some("Screenshot image type: \"png\" or \"jpeg\".".to_owned()),
            },
        ),
        (
            "request".to_owned(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
        (
            "paths".to_owned(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some("File paths for upload action.".to_owned()),
            },
        ),
        (
            "accept".to_owned(),
            JsonSchema::Boolean {
                description: Some("Whether to accept a dialog.".to_owned()),
            },
        ),
        (
            "prompt_text".to_owned(),
            JsonSchema::String {
                description: Some("Text to enter in a prompt dialog.".to_owned()),
            },
        ),
        (
            "level".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Console log level filter: \"error\", \"warning\", \"log\", \"info\".".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "browser",
        description: "Browser automation: navigate pages, take screenshots, capture DOM snapshots, interact with UI elements (click, type, drag), handle dialogs and file uploads. Requires an external browser service at SAVFOX_BROWSER_URL (default: http://127.0.0.1:9222).",
        properties,
        required: &["action"],
    })
}

fn create_canvas_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Canvas action: \"present\" (show canvas), \"hide\", \"navigate\" (load URL), \
                     \"eval\" (run JavaScript), \"snapshot\" (capture screenshot), \
                     \"a2ui_push\" (push A2UI component), \"a2ui_reset\".".to_owned(),
                ),
            },
        ),
        (
            "node_id".to_owned(),
            JsonSchema::String {
                description: Some("Target node ID.".to_owned()),
            },
        ),
        (
            "url".to_owned(),
            JsonSchema::String {
                description: Some("URL for navigate action.".to_owned()),
            },
        ),
        (
            "code".to_owned(),
            JsonSchema::String {
                description: Some("JavaScript code for eval action.".to_owned()),
            },
        ),
        (
            "format".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Image format for snapshot: \"png\", \"jpg\", \"jpeg\".".to_owned(),
                ),
            },
        ),
        (
            "command".to_owned(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
        (
            "component".to_owned(),
            JsonSchema::String {
                description: Some("A2UI component ID or path.".to_owned()),
            },
        ),
        (
            "props".to_owned(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
        (
            "title".to_owned(),
            JsonSchema::String {
                description: Some("Title for the canvas presentation.".to_owned()),
            },
        ),
        (
            "width".to_owned(),
            JsonSchema::Number {
                description: Some("Width of the canvas.".to_owned()),
            },
        ),
        (
            "height".to_owned(),
            JsonSchema::Number {
                description: Some("Height of the canvas.".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "canvas",
        description: "A2UI canvas rendering on connected node displays: present/hide canvas, navigate to URLs, evaluate JavaScript, capture snapshots, and push A2UI components. Requires canvas service at SAVFOX_CANVAS_URL (default: http://127.0.0.1:9300).",
        properties,
        required: &["action"],
    })
}

fn create_gateway_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "method".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Gateway JSON-RPC method to call (e.g., \"agents.list\", \"chat.send\", \
                     \"sessions.list\", \"cron.add\", \"config.get\", \"tools.invoke\").".to_owned(),
                ),
            },
        ),
        (
            "params".to_owned(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "gateway",
        description: "Invoke any gateway server JSON-RPC method: agent management, session control, chat operations, cron scheduling, node control, channel management, TTS, and configuration. Requires SAVFOX_GATEWAY_URL and optionally SAVFOX_GATEWAY_TOKEN.",
        properties,
        required: &["method"],
    })
}

fn create_llm_task_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "prompt".to_owned(),
            JsonSchema::String {
                description: Some("The task prompt to send to the LLM.".to_owned()),
            },
        ),
        (
            "model".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Optional model to use (defaults to the agent's model).".to_owned(),
                ),
            },
        ),
        (
            "system".to_owned(),
            JsonSchema::String {
                description: Some("Optional system prompt for the task.".to_owned()),
            },
        ),
        (
            "output_schema".to_owned(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
        (
            "temperature".to_owned(),
            JsonSchema::Number {
                description: Some("Temperature for sampling (0.0 to 2.0).".to_owned()),
            },
        ),
        (
            "max_tokens".to_owned(),
            JsonSchema::Number {
                description: Some("Maximum tokens for the response.".to_owned()),
            },
        ),
        (
            "timeout_secs".to_owned(),
            JsonSchema::Number {
                description: Some("Timeout in seconds (default: 120).".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "llm_task",
        description: "Run a standalone LLM inference task with a specific prompt, optional model, system prompt, and JSON schema validation on the output. Routes through the gateway's OpenAI-compatible endpoint.",
        properties,
        required: &["prompt"],
    })
}

/// TODO(dylan): deprecate once we get rid of json tool
#[derive(Serialize, Deserialize)]
pub(crate) struct ApplyPatchToolArgs {
    pub(crate) input: String,
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

    /// Register an experimental tool if its name is present in the config.
    macro_rules! register_experimental {
        ($name:literal, $handler:expr, $spec:expr) => {
            if config
                .experimental_supported_tools
                .contains(&$name.to_string())
            {
                builder.push_spec($spec);
                builder.register_handler($name, Arc::new($handler));
            }
        };
        ($name:literal, $handler:expr, $spec:expr,parallel) => {
            if config
                .experimental_supported_tools
                .contains(&$name.to_string())
            {
                builder.push_spec_with_parallel_support($spec, true);
                builder.register_handler($name, Arc::new($handler));
            }
        };
    }

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

    register_experimental!(
        "grep_files",
        GrepFilesHandler,
        create_grep_files_tool(),
        parallel
    );
    register_experimental!(
        "read_file",
        ReadFileHandler,
        create_read_file_tool(),
        parallel
    );
    register_experimental!("list_dir", ListDirHandler, create_list_dir_tool(), parallel);
    register_experimental!(
        "test_sync_tool",
        TestSyncHandler,
        create_test_sync_tool(),
        parallel
    );
    register_experimental!("write_file", WriteFileHandler, create_write_file_tool());
    register_experimental!(
        "web_fetch",
        WebFetchHandler,
        create_web_fetch_tool(),
        parallel
    );
    register_experimental!(
        "web_search_provider",
        WebSearchHandler,
        create_web_search_provider_tool()
    );
    register_experimental!("message", MessageHandler, create_message_tool());

    if config
        .experimental_supported_tools
        .contains(&"sessions".to_owned())
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

    register_experimental!(
        "image_analyze",
        ImageAnalyzeHandler,
        create_image_analyze_tool()
    );
    register_experimental!("cron", CronHandler::new(), create_cron_tool());
    register_experimental!("tts", TtsHandler, create_tts_tool());
    register_experimental!("process", ProcessHandler, create_process_tool());
    register_experimental!(
        "discord_actions",
        DiscordActionsHandler,
        create_discord_actions_tool()
    );
    register_experimental!(
        "slack_actions",
        SlackActionsHandler,
        create_slack_actions_tool()
    );
    register_experimental!(
        "telegram_actions",
        TelegramActionsHandler,
        create_telegram_actions_tool()
    );
    register_experimental!(
        "sessions_spawn",
        SessionsSpawnHandler,
        create_sessions_spawn_tool()
    );
    register_experimental!(
        "gateway_status",
        GatewayStatusHandler,
        create_gateway_status_tool()
    );
    register_experimental!("memory", MemoryHandler, create_memory_tool());
    register_experimental!("md_memory", MdMemoryHandler, create_md_memory_tool());
    register_experimental!(
        "image_generate",
        ImageGenerateHandler,
        create_image_generate_tool()
    );
    register_experimental!("agents_list", AgentsListHandler, create_agents_list_tool());
    register_experimental!("nodes", NodesHandler, create_nodes_tool());
    register_experimental!("browser", BrowserHandler, create_browser_tool());
    register_experimental!(
        "whatsapp_actions",
        WhatsAppActionsHandler,
        create_whatsapp_actions_tool()
    );
    register_experimental!("canvas", CanvasHandler, create_canvas_tool());
    register_experimental!("gateway", GatewayToolHandler, create_gateway_tool());
    register_experimental!(
        "channel_tools",
        ChannelToolsHandler,
        create_channel_tools_tool()
    );
    register_experimental!("llm_task", LlmTaskHandler, create_llm_task_tool());
    register_experimental!("agent_step", AgentStepHandler, create_agent_step_tool());
    register_experimental!(
        "sessions_send_a2a",
        SessionsSendA2AHandler,
        create_sessions_send_a2a_tool()
    );

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
    use serde_json::json;

    use super::*;
    use crate::client_common::tools::{FreeformTool, ResponsesApiTool};
    use crate::config::test_config;
    use crate::models_manager::manager::ModelsManager;
    use crate::tools::registry::ConfiguredToolSpec;

    fn mcp_tool(
        name: &str,
        description: &str,
        input_schema: serde_json::Value,
    ) -> rmcp::model::Tool {
        rmcp::model::Tool::new_with_raw(
            name.to_string(),
            Some(description.to_string().into()),
            std::sync::Arc::new(rmcp::model::object(input_schema)),
        )
    }

    #[test]
    fn mcp_tool_to_openai_tool_inserts_empty_properties() {
        let mut schema = rmcp::model::JsonObject::new();
        schema.insert("type".to_string(), serde_json::json!("object"));

        let tool = rmcp::model::Tool::new_with_raw(
            "no_props".to_string(),
            Some("No properties".to_string().into()),
            std::sync::Arc::new(schema),
        );

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
