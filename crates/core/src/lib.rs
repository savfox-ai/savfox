//! Root of the `savfox-core` library.

// Prevent accidental direct writes to stdout/stderr in library code. All
// user-visible output must go through the appropriate abstraction (e.g.,
// the TUI or the tracing stack).
#![deny(clippy::print_stdout, clippy::print_stderr)]
#![allow(unreachable_pub)]

mod analytics_client;
pub mod api_bridge;
mod apply_patch;
pub mod auth;
pub mod auth_profiles;
pub mod bash;
mod client;
mod client_common;
mod compact_remote;
#[path = "savfox_agent.rs"]
pub mod savfox;
#[path = "session_state.rs"]
mod savfox_session;
pub use savfox_session::{SavfoxSession, SessionConfigSnapshot};
mod agent;
mod command_safety;
pub mod config;
pub mod config_loader;
pub mod connectors;
mod context_manager;
pub mod custom_prompts;
pub mod embedding;
pub mod env;
mod environment_context;
pub mod error;
pub mod exec;
pub mod exec_env;
mod exec_policy;
pub mod external_content;
pub mod features;
mod flags;
pub mod git_info;
pub mod instructions;
pub mod landlock;
pub mod mcp;
mod mcp_connection_manager;
pub mod md_memory;
pub mod models_manager;
#[path = "agent_delegate.rs"]
mod savfox_delegate;
mod transport_manager;
pub use mcp_connection_manager::{
    MCP_SANDBOX_STATE_CAPABILITY, MCP_SANDBOX_STATE_METHOD, SandboxState,
};
mod mcp_tool_call;
mod mentions;
mod message_history;
pub mod model_fallback;
mod model_identifiers;
mod model_provider_info;
pub mod parse_command;
pub mod path_utils;
pub mod personality_migration;
pub mod powershell;
mod proposed_plan_parser;
pub mod sandboxing;
mod session_prefix;
mod stream_events_utils;
mod tagged_block_parser;
mod text_encoding;
pub mod token_data;
mod truncate;
mod unified_exec;
pub mod windows_sandbox;
pub use model_identifiers::{parse_provider_prefixed_model, request_model_for_provider};
pub use model_provider_info::{
    CHAT_WIRE_API_DEPRECATION_SUMMARY, DEFAULT_LMSTUDIO_PORT, DEFAULT_OLLAMA_PORT,
    LMSTUDIO_OSS_PROVIDER_ID, ModelProviderInfo, OLLAMA_CHAT_PROVIDER_ID, OLLAMA_OSS_PROVIDER_ID,
    WireApi, built_in_model_providers, create_oss_provider_with_base_url,
    get_bearer_token_override, inject_provider_auth_overrides_from_store,
    remove_bearer_token_override, remove_env_override, set_bearer_token_override, set_env_override,
};
mod event_mapping;
pub mod review_format;
pub mod review_prompts;
mod session_manager;
pub mod web_search;
pub use savfox_protocol::protocol::InitialHistory;
pub use session_manager::{NewSession, SessionManager};
#[deprecated(note = "use SessionManager")]
pub type ConversationManager = SessionManager;
#[deprecated(note = "use NewSession")]
pub type NewConversation = NewSession;
#[deprecated(note = "use SavfoxSession")]
pub type SavfoxConversation = SavfoxSession;
// Re-export common auth types for workspace consumers
pub use auth::{AuthManager, SavfoxAuth};
pub mod default_client;
pub mod project_doc;
pub mod rollout;
pub(crate) mod safety;
pub mod seatbelt;
pub mod shell;
pub mod shell_snapshot;
pub mod skills;
pub mod spawn;
pub mod state_db;
pub mod subagent;
pub mod terminal;
mod tools;
pub mod transcript_policy;
/// Agent-to-agent communication types, delegation chain tracking,
/// and capability discovery structures.
pub mod a2a {
    pub use crate::tools::handlers::a2a_types::*;
}
pub mod turn_diff_tracker;
mod turn_metadata;
pub mod updater;
#[deprecated(note = "use find_session_path_by_id_str")]
pub use rollout::find_conversation_path_by_id_str;
pub use rollout::list::{
    Cursor, SessionItem, SessionSortKey, SessionsPage, parse_cursor, read_head_for_summary,
    read_session_meta_line,
};
pub use rollout::session_index::find_session_names_by_ids;
pub use rollout::{
    ARCHIVED_SESSIONS_SUBDIR, INTERACTIVE_SESSION_SOURCES, RolloutRecorder, RolloutRecorderParams,
    SESSIONS_SUBDIR, SessionMeta, find_archived_session_path_by_id_str,
    find_session_path_by_id_str, find_session_path_by_name_str, rollout_date_parts,
};
pub use transport_manager::TransportManager;
mod function_tool;
mod state;
mod tasks;
mod user_notification;
mod user_shell_command;
pub mod util;

pub use apply_patch::SAVFOX_APPLY_PATCH_ARG1;
pub use client::{
    ModelClient, ModelClientSession, WEB_SEARCH_ELIGIBLE_HEADER, X_SAVFOX_TURN_METADATA_HEADER,
};
pub use client_common::{Prompt, REVIEW_PROMPT, ResponseEvent, ResponseStream};
pub use command_safety::{is_dangerous_command, is_safe_command};
pub use compact::content_items_to_text;
pub use event_mapping::parse_turn_item;
pub use exec_policy::{ExecPolicyError, check_execpolicy_for_warnings, load_exec_policy};
// Re-export the protocol types from the standalone `savfox-protocol` crate so existing
// `savfox_core::protocol::...` references continue to work across the workspace.
pub use safety::get_platform_sandbox;
// Re-export protocol config enums to ensure call sites can use the same types
// as those in the protocol crate when constructing protocol messages.
pub use savfox_protocol::config_types as protocol_config_types;
pub use savfox_protocol::models::{
    ContentItem, LocalShellAction, LocalShellExecAction, LocalShellStatus, ResponseItem,
};
pub use savfox_protocol::protocol;
pub use tools::spec::parse_tool_input_schema;
pub mod compact;
pub mod otel_init;
