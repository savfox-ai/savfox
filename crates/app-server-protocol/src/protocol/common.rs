use std::path::{Path, PathBuf};

use savfox_protocol::SessionId;
use savfox_protocol::config_types::{ForcedLoginMethod, ReasoningSummary, SandboxMode, Verbosity};
use savfox_protocol::openai_models::ReasoningEffort;
use savfox_protocol::protocol::{AskForApproval, SessionSource};
use savfox_utils::absolute_path::AbsolutePathBuf;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum_macros::Display;
use ts_rs::TS;

use crate::export::{GeneratedSchema, write_json_schema};
use crate::protocol::v1;
use crate::{JSONRPCNotification, JSONRPCRequest, RequestId};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
#[ts(type = "string")]
pub struct GitSha(pub String);

impl GitSha {
    pub fn new(sha: &str) -> Self {
        Self(sha.to_string())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<InitializeCapabilities>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub title: Option<String>,
    pub version: String,
}

/// Client-declared capabilities negotiated during initialize.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeCapabilities {
    /// Opt into receiving experimental API methods and fields.
    #[serde(default)]
    pub experimental_api: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub user_agent: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    pub conversation_id: SessionId,
    pub path: PathBuf,
    pub preview: String,
    pub timestamp: Option<String>,
    pub updated_at: Option<String>,
    pub model_provider: String,
    pub cwd: PathBuf,
    pub cli_version: String,
    pub source: SessionSource,
    pub git_info: Option<ConversationGitInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct ConversationGitInfo {
    pub sha: Option<String>,
    pub branch: Option<String>,
    pub origin_url: Option<String>,
}

/// Authentication mode for OpenAI-backed providers.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Display, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// OpenAI API key provided by the caller and stored by Savfox.
    ApiKey,
    /// ChatGPT OAuth managed by Savfox (tokens persisted and refreshed by Savfox).
    Chatgpt,
    /// [UNSTABLE] FOR OPENAI INTERNAL USE ONLY - DO NOT USE.
    ///
    /// ChatGPT auth tokens are supplied by an external host app and are only
    /// stored in memory. Token refresh must be handled by the external host app.
    #[serde(rename = "chatgptAuthTokens")]
    #[ts(rename = "chatgptAuthTokens")]
    #[strum(serialize = "chatgptAuthTokens")]
    ChatgptAuthTokens,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Serialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct UserSavedConfig {
    pub approval_policy: Option<AskForApproval>,
    pub sandbox_mode: Option<SandboxMode>,
    pub sandbox_settings: Option<SandboxSettings>,
    pub forced_chatgpt_workspace_id: Option<String>,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub model: Option<String>,
    pub model_reasoning_effort: Option<ReasoningEffort>,
    pub model_reasoning_summary: Option<ReasoningSummary>,
    pub model_verbosity: Option<Verbosity>,
    pub tools: Option<Tools>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Serialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Tools {
    pub web_search: Option<bool>,
    pub view_image: Option<bool>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Serialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SandboxSettings {
    #[serde(default)]
    pub writable_roots: Vec<AbsolutePathBuf>,
    pub network_access: Option<bool>,
    pub exclude_tmpdir_env_var: Option<bool>,
    pub exclude_slash_tmp: Option<bool>,
}

macro_rules! experimental_reason_expr {
    // If a request variant is explicitly marked experimental, that reason wins.
    (#[experimental($reason:expr)] $params:ident $(, $inspect_params:tt)?) => {
        Some($reason)
    };
    // `inspect_params: true` is used when a method is mostly stable but needs
    // field-level gating from its params type (for example, SessionStart).
    ($params:ident,true) => {
        crate::experimental_api::ExperimentalApi::experimental_reason($params)
    };
    ($params:ident $(, $inspect_params:tt)?) => {
        None
    };
}

macro_rules! experimental_method_entry {
    (#[experimental($reason:expr)] => $wire:literal) => {
        $wire
    };
    ($($tt:tt)*) => {
        ""
    };
}

macro_rules! experimental_type_entry {
    (#[experimental($reason:expr)] $ty:ty) => {
        stringify!($ty)
    };
    ($ty:ty) => {
        ""
    };
}

/// Generates an `enum ClientRequest` where each variant is a request that the
/// client can send to the server. Each variant has associated `params` and
/// `response` types. Also generates a `export_client_responses()` function to
/// export all response types to TypeScript.
macro_rules! client_request_definitions {
    (
        $(
            $(#[experimental($reason:expr)])?
            $(#[doc = $variant_doc:literal])*
            $variant:ident $(=> $wire:literal)? {
                params: $(#[$params_meta:meta])* $params:ty,
                $(inspect_params: $inspect_params:tt,)?
                response: $response:ty,
            }
        ),* $(,)?
    ) => {
        /// Request from the client to the server.
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
        #[serde(tag = "method", rename_all = "camelCase")]
        pub enum ClientRequest {
            $(
                $(#[doc = $variant_doc])*
                $(#[serde(rename = $wire)] #[ts(rename = $wire)])?
                $variant {
                    #[serde(rename = "id")]
                    request_id: RequestId,
                    $(#[$params_meta])*
                    params: $params,
                },
            )*
        }

        impl crate::experimental_api::ExperimentalApi for ClientRequest {
            fn experimental_reason(&self) -> Option<&'static str> {
                match self {
                    $(
                        Self::$variant { params: _params, .. } => {
                            experimental_reason_expr!(
                                $(#[experimental($reason)])?
                                _params
                                $(, $inspect_params)?
                            )
                        }
                    )*
                }
            }
        }

        pub(crate) const EXPERIMENTAL_CLIENT_METHODS: &[&str] = &[
            $(
                experimental_method_entry!($(#[experimental($reason)])? $(=> $wire)?),
            )*
        ];
        pub(crate) const EXPERIMENTAL_CLIENT_METHOD_PARAM_TYPES: &[&str] = &[
            $(
                experimental_type_entry!($(#[experimental($reason)])? $params),
            )*
        ];
        pub(crate) const EXPERIMENTAL_CLIENT_METHOD_RESPONSE_TYPES: &[&str] = &[
            $(
                experimental_type_entry!($(#[experimental($reason)])? $response),
            )*
        ];

        pub fn export_client_responses(
            out_dir: &::std::path::Path,
        ) -> ::std::result::Result<(), ::ts_rs::ExportError> {
            let cfg = ::ts_rs::Config::new().with_out_dir(out_dir);
            $(
                <$response as ::ts_rs::TS>::export_all(&cfg)?;
            )*
            Ok(())
        }

        #[allow(clippy::vec_init_then_push)]
        pub fn export_client_response_schemas(
            out_dir: &::std::path::Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let mut schemas = Vec::new();
            $(
                schemas.push(write_json_schema::<$response>(out_dir, stringify!($response))?);
            )*
            Ok(schemas)
        }

        #[allow(clippy::vec_init_then_push)]
        pub fn export_client_param_schemas(
            out_dir: &::std::path::Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let mut schemas = Vec::new();
            $(
                schemas.push(write_json_schema::<$params>(out_dir, stringify!($params))?);
            )*
            Ok(schemas)
        }
    };
}

client_request_definitions! {
    Initialize {
        params: InitializeParams,
        response: InitializeResponse,
    },

    /// NEW APIs
    // Session lifecycle
    // Uses `inspect_params` because only some fields are experimental.
    SessionStart => "session/start" {
        params: v1::SessionStartParams,
        inspect_params: true,
        response: v1::SessionStartResponse,
    },
    SessionResume => "session/resume" {
        params: v1::SessionResumeParams,
        response: v1::SessionResumeResponse,
    },
    SessionFork => "session/fork" {
        params: v1::SessionForkParams,
        response: v1::SessionForkResponse,
    },
    SessionArchive => "session/archive" {
        params: v1::SessionArchiveParams,
        response: v1::SessionArchiveResponse,
    },
    SessionSetName => "session/name/set" {
        params: v1::SessionSetNameParams,
        response: v1::SessionSetNameResponse,
    },
    SessionUnarchive => "session/unarchive" {
        params: v1::SessionUnarchiveParams,
        response: v1::SessionUnarchiveResponse,
    },
    SessionRollback => "session/rollback" {
        params: v1::SessionRollbackParams,
        response: v1::SessionRollbackResponse,
    },
    SessionList => "session/list" {
        params: v1::SessionListParams,
        response: v1::SessionListResponse,
    },
    SessionLoadedList => "session/loaded/list" {
        params: v1::SessionLoadedListParams,
        response: v1::SessionLoadedListResponse,
    },
    SessionRead => "session/read" {
        params: v1::SessionReadParams,
        response: v1::SessionReadResponse,
    },
    SkillsList => "skills/list" {
        params: v1::SkillsListParams,
        response: v1::SkillsListResponse,
    },
    AppsList => "app/list" {
        params: v1::AppsListParams,
        response: v1::AppsListResponse,
    },
    SkillsConfigWrite => "skills/config/write" {
        params: v1::SkillsConfigWriteParams,
        response: v1::SkillsConfigWriteResponse,
    },
    TurnStart => "turn/start" {
        params: v1::TurnStartParams,
        response: v1::TurnStartResponse,
    },
    TurnInterrupt => "turn/interrupt" {
        params: v1::TurnInterruptParams,
        response: v1::TurnInterruptResponse,
    },
    ReviewStart => "review/start" {
        params: v1::ReviewStartParams,
        response: v1::ReviewStartResponse,
    },

    ModelList => "model/list" {
        params: v1::ModelListParams,
        response: v1::ModelListResponse,
    },
    #[experimental("collaborationMode/list")]
    /// Lists collaboration mode presets.
    CollaborationModeList => "collaborationMode/list" {
        params: v1::CollaborationModeListParams,
        response: v1::CollaborationModeListResponse,
    },
    #[experimental("mock/experimentalMethod")]
    /// Test-only method used to validate experimental gating.
    MockExperimentalMethod => "mock/experimentalMethod" {
        params: v1::MockExperimentalMethodParams,
        response: v1::MockExperimentalMethodResponse,
    },

    McpServerOauthLogin => "mcpServer/oauth/login" {
        params: v1::McpServerOauthLoginParams,
        response: v1::McpServerOauthLoginResponse,
    },

    McpServerRefresh => "config/mcpServer/reload" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        response: v1::McpServerRefreshResponse,
    },

    McpServerStatusList => "mcpServerStatus/list" {
        params: v1::ListMcpServerStatusParams,
        response: v1::ListMcpServerStatusResponse,
    },

    LoginAccount => "account/login/start" {
        params: v1::LoginAccountParams,
        response: v1::LoginAccountResponse,
    },

    CancelLoginAccount => "account/login/cancel" {
        params: v1::CancelLoginAccountParams,
        response: v1::CancelLoginAccountResponse,
    },

    LogoutAccount => "account/logout" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        response: v1::LogoutAccountResponse,
    },

    GetAccountRateLimits => "account/rateLimits/read" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        response: v1::GetAccountRateLimitsResponse,
    },

    FeedbackUpload => "feedback/upload" {
        params: v1::FeedbackUploadParams,
        response: v1::FeedbackUploadResponse,
    },

    /// Execute a command (argv vector) under the server's sandbox.
    OneOffCommandExec => "command/exec" {
        params: v1::CommandExecParams,
        response: v1::CommandExecResponse,
    },

    ConfigRead => "config/read" {
        params: v1::ConfigReadParams,
        response: v1::ConfigReadResponse,
    },
    ConfigValueWrite => "config/value/write" {
        params: v1::ConfigValueWriteParams,
        response: v1::ConfigWriteResponse,
    },
    ConfigBatchWrite => "config/batchWrite" {
        params: v1::ConfigBatchWriteParams,
        response: v1::ConfigWriteResponse,
    },

    ConfigRequirementsRead => "configRequirements/read" {
        params: #[ts(type = "undefined")] #[serde(skip_serializing_if = "Option::is_none")] Option<()>,
        response: v1::ConfigRequirementsReadResponse,
    },

    GetAccount => "account/read" {
        params: v1::GetAccountParams,
        response: v1::GetAccountResponse,
    },

    FuzzyFileSearch {
        params: FuzzyFileSearchParams,
        response: FuzzyFileSearchResponse,
    },
}

/// Generates an `enum ServerRequest` where each variant is a request that the
/// server can send to the client along with the corresponding params and
/// response types. It also generates helper types used by the app/server
/// infrastructure (payload enum, request constructor, and export helpers).
macro_rules! server_request_definitions {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident $(=> $wire:literal)? {
                params: $params:ty,
                response: $response:ty,
            }
        ),* $(,)?
    ) => {
        /// Request initiated from the server and sent to the client.
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
        #[serde(tag = "method", rename_all = "camelCase")]
        pub enum ServerRequest {
            $(
                $(#[$variant_meta])*
                $(#[serde(rename = $wire)] #[ts(rename = $wire)])?
                $variant {
                    #[serde(rename = "id")]
                    request_id: RequestId,
                    params: $params,
                },
            )*
        }

        #[derive(Debug, Clone, PartialEq, JsonSchema)]
        pub enum ServerRequestPayload {
            $( $variant($params), )*
        }

        impl ServerRequestPayload {
            pub fn request_with_id(self, request_id: RequestId) -> ServerRequest {
                match self {
                    $(Self::$variant(params) => ServerRequest::$variant { request_id, params },)*
                }
            }
        }

        pub fn export_server_responses(
            out_dir: &::std::path::Path,
        ) -> ::std::result::Result<(), ::ts_rs::ExportError> {
            let cfg = ::ts_rs::Config::new().with_out_dir(out_dir);
            $(
                <$response as ::ts_rs::TS>::export_all(&cfg)?;
            )*
            Ok(())
        }

        #[allow(clippy::vec_init_then_push)]
        pub fn export_server_response_schemas(
            out_dir: &Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let mut schemas = Vec::new();
            $(
                schemas.push(crate::export::write_json_schema::<$response>(
                    out_dir,
                    concat!(stringify!($variant), "Response"),
                )?);
            )*
            Ok(schemas)
        }

        #[allow(clippy::vec_init_then_push)]
        pub fn export_server_param_schemas(
            out_dir: &Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let mut schemas = Vec::new();
            $(
                schemas.push(crate::export::write_json_schema::<$params>(
                    out_dir,
                    concat!(stringify!($variant), "Params"),
                )?);
            )*
            Ok(schemas)
        }
    };
}

/// Generates `ServerNotification` enum and helpers, including a JSON Schema
/// exporter for each notification.
macro_rules! server_notification_definitions {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident $(=> $wire:literal)? ( $payload:ty )
        ),* $(,)?
    ) => {
        /// Notification sent from the server to the client.
        #[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, TS, Display)]
        #[serde(tag = "method", content = "params", rename_all = "camelCase")]
        #[strum(serialize_all = "camelCase")]
        pub enum ServerNotification {
            $(
                $(#[$variant_meta])*
                $(#[serde(rename = $wire)] #[ts(rename = $wire)] #[strum(serialize = $wire)])?
                $variant($payload),
            )*
        }

        impl ServerNotification {
            pub fn to_params(self) -> Result<serde_json::Value, serde_json::Error> {
                match self {
                    $(Self::$variant(params) => serde_json::to_value(params),)*
                }
            }
        }

        impl TryFrom<JSONRPCNotification> for ServerNotification {
            type Error = serde_json::Error;

            fn try_from(value: JSONRPCNotification) -> Result<Self, serde_json::Error> {
                serde_json::from_value(serde_json::to_value(value)?)
            }
        }

        #[allow(clippy::vec_init_then_push)]
        pub fn export_server_notification_schemas(
            out_dir: &::std::path::Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let mut schemas = Vec::new();
            $(schemas.push(crate::export::write_json_schema::<$payload>(out_dir, stringify!($payload))?);)*
            Ok(schemas)
        }
    };
}
/// Notifications sent from the client to the server.
macro_rules! client_notification_definitions {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident $( ( $payload:ty ) )?
        ),* $(,)?
    ) => {
        #[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, TS, Display)]
        #[serde(tag = "method", content = "params", rename_all = "camelCase")]
        #[strum(serialize_all = "camelCase")]
        pub enum ClientNotification {
            $(
                $(#[$variant_meta])*
                $variant $( ( $payload ) )?,
            )*
        }

        pub fn export_client_notification_schemas(
            _out_dir: &::std::path::Path,
        ) -> ::anyhow::Result<Vec<GeneratedSchema>> {
            let schemas = Vec::new();
            $( $(schemas.push(crate::export::write_json_schema::<$payload>(_out_dir, stringify!($payload))?);)? )*
            Ok(schemas)
        }
    };
}

impl TryFrom<JSONRPCRequest> for ServerRequest {
    type Error = serde_json::Error;

    fn try_from(value: JSONRPCRequest) -> Result<Self, Self::Error> {
        serde_json::from_value(serde_json::to_value(value)?)
    }
}

server_request_definitions! {
    /// NEW APIs
    /// Sent when approval is requested for a specific command execution.
    /// This request is used for Turns started via turn/start.
    CommandExecutionRequestApproval => "item/commandExecution/requestApproval" {
        params: v1::CommandExecutionRequestApprovalParams,
        response: v1::CommandExecutionRequestApprovalResponse,
    },

    /// Sent when approval is requested for a specific file change.
    /// This request is used for Turns started via turn/start.
    FileChangeRequestApproval => "item/fileChange/requestApproval" {
        params: v1::FileChangeRequestApprovalParams,
        response: v1::FileChangeRequestApprovalResponse,
    },

    /// EXPERIMENTAL - Request input from the user for a tool call.
    ToolRequestUserInput => "item/tool/requestUserInput" {
        params: v1::ToolRequestUserInputParams,
        response: v1::ToolRequestUserInputResponse,
    },

    /// Execute a dynamic tool call on the client.
    DynamicToolCall => "item/tool/call" {
        params: v1::DynamicToolCallParams,
        response: v1::DynamicToolCallResponse,
    },

    ChatgptAuthTokensRefresh => "account/chatgptAuthTokens/refresh" {
        params: v1::ChatgptAuthTokensRefreshParams,
        response: v1::ChatgptAuthTokensRefreshResponse,
    },

}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FuzzyFileSearchParams {
    pub query: String,
    pub roots: Vec<String>,
    // if provided, will cancel any previous request that used the same value
    pub cancellation_token: Option<String>,
}

/// Superset of [`savfox_file_search::FileMatch`]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct FuzzyFileSearchResult {
    pub root: String,
    pub path: String,
    pub file_name: String,
    pub score: u32,
    pub indices: Option<Vec<u32>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct FuzzyFileSearchResponse {
    pub files: Vec<FuzzyFileSearchResult>,
}

server_notification_definitions! {
    /// NEW NOTIFICATIONS
    Error => "error" (v1::ErrorNotification),
    SessionStarted => "session/started" (v1::SessionStartedNotification),
    SessionNameUpdated => "session/name/updated" (v1::SessionNameUpdatedNotification),
    SessionTokenUsageUpdated => "session/tokenUsage/updated" (v1::SessionTokenUsageUpdatedNotification),
    TurnStarted => "turn/started" (v1::TurnStartedNotification),
    TurnCompleted => "turn/completed" (v1::TurnCompletedNotification),
    TurnDiffUpdated => "turn/diff/updated" (v1::TurnDiffUpdatedNotification),
    TurnPlanUpdated => "turn/plan/updated" (v1::TurnPlanUpdatedNotification),
    ItemStarted => "item/started" (v1::ItemStartedNotification),
    ItemCompleted => "item/completed" (v1::ItemCompletedNotification),
    /// This event is internal-only. Used by Savfox Cloud.
    RawResponseItemCompleted => "rawResponseItem/completed" (v1::RawResponseItemCompletedNotification),
    AgentMessageDelta => "item/agentMessage/delta" (v1::AgentMessageDeltaNotification),
    /// EXPERIMENTAL - proposed plan streaming deltas for plan items.
    PlanDelta => "item/plan/delta" (v1::PlanDeltaNotification),
    CommandExecutionOutputDelta => "item/commandExecution/outputDelta" (v1::CommandExecutionOutputDeltaNotification),
    TerminalInteraction => "item/commandExecution/terminalInteraction" (v1::TerminalInteractionNotification),
    FileChangeOutputDelta => "item/fileChange/outputDelta" (v1::FileChangeOutputDeltaNotification),
    McpToolCallProgress => "item/mcpToolCall/progress" (v1::McpToolCallProgressNotification),
    McpServerOauthLoginCompleted => "mcpServer/oauthLogin/completed" (v1::McpServerOauthLoginCompletedNotification),
    AccountUpdated => "account/updated" (v1::AccountUpdatedNotification),
    AccountRateLimitsUpdated => "account/rateLimits/updated" (v1::AccountRateLimitsUpdatedNotification),
    ReasoningSummaryTextDelta => "item/reasoning/summaryTextDelta" (v1::ReasoningSummaryTextDeltaNotification),
    ReasoningSummaryPartAdded => "item/reasoning/summaryPartAdded" (v1::ReasoningSummaryPartAddedNotification),
    ReasoningTextDelta => "item/reasoning/textDelta" (v1::ReasoningTextDeltaNotification),
    DeprecationNotice => "deprecationNotice" (v1::DeprecationNoticeNotification),
    ConfigWarning => "configWarning" (v1::ConfigWarningNotification),

    /// Notifies the user of world-writable directories on Windows, which cannot be protected by the sandbox.
    WindowsWorldWritableWarning => "windows/worldWritableWarning" (v1::WindowsWorldWritableWarningNotification),

    #[serde(rename = "account/login/completed")]
    #[ts(rename = "account/login/completed")]
    #[strum(serialize = "account/login/completed")]
    AccountLoginCompleted(v1::AccountLoginCompletedNotification),

}

client_notification_definitions! {
    Initialized,
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use savfox_protocol::SessionId;
    use savfox_protocol::account::PlanType;
    use serde_json::json;

    use super::*;

    #[test]
    fn conversation_id_serializes_as_plain_string() -> Result<()> {
        let id = SessionId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c8")?;

        assert_eq!(
            json!("67e55044-10b1-426f-9247-bb680e5fe0c8"),
            serde_json::to_value(id)?
        );
        Ok(())
    }

    #[test]
    fn conversation_id_deserializes_from_plain_string() -> Result<()> {
        let id: SessionId = serde_json::from_value(json!("67e55044-10b1-426f-9247-bb680e5fe0c8"))?;

        assert_eq!(
            SessionId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c8")?,
            id,
        );
        Ok(())
    }

    #[test]
    fn serialize_client_notification() -> Result<()> {
        let notification = ClientNotification::Initialized;
        // Note there is no "params" field for this notification.
        assert_eq!(
            json!({
                "method": "initialized",
            }),
            serde_json::to_value(&notification)?,
        );
        Ok(())
    }

    #[test]
    fn serialize_chatgpt_auth_tokens_refresh_request() -> Result<()> {
        let request = ServerRequest::ChatgptAuthTokensRefresh {
            request_id: RequestId::Integer(8),
            params: v1::ChatgptAuthTokensRefreshParams {
                reason: v1::ChatgptAuthTokensRefreshReason::Unauthorized,
                previous_account_id: Some("org-123".to_string()),
            },
        };
        assert_eq!(
            json!({
                "method": "account/chatgptAuthTokens/refresh",
                "id": 8,
                "params": {
                    "reason": "unauthorized",
                    "previousAccountId": "org-123"
                }
            }),
            serde_json::to_value(&request)?,
        );
        Ok(())
    }

    #[test]
    fn serialize_get_account_rate_limits() -> Result<()> {
        let request = ClientRequest::GetAccountRateLimits {
            request_id: RequestId::Integer(1),
            params: None,
        };
        assert_eq!(
            json!({
                "method": "account/rateLimits/read",
                "id": 1,
            }),
            serde_json::to_value(&request)?,
        );
        Ok(())
    }

    #[test]
    fn serialize_config_requirements_read() -> Result<()> {
        let request = ClientRequest::ConfigRequirementsRead {
            request_id: RequestId::Integer(1),
            params: None,
        };
        assert_eq!(
            json!({
                "method": "configRequirements/read",
                "id": 1,
            }),
            serde_json::to_value(&request)?,
        );
        Ok(())
    }

    #[test]
    fn serialize_account_login_api_key() -> Result<()> {
        let request = ClientRequest::LoginAccount {
            request_id: RequestId::Integer(2),
            params: v1::LoginAccountParams::ApiKey {
                api_key: "secret".to_string(),
            },
        };
        assert_eq!(
            json!({
                "method": "account/login/start",
                "id": 2,
                "params": {
                    "type": "apiKey",
                    "apiKey": "secret"
                }
            }),
            serde_json::to_value(&request)?,
        );
        Ok(())
    }

    #[test]
    fn serialize_account_login_chatgpt() -> Result<()> {
        let request = ClientRequest::LoginAccount {
            request_id: RequestId::Integer(3),
            params: v1::LoginAccountParams::Chatgpt,
        };
        assert_eq!(
            json!({
                "method": "account/login/start",
                "id": 3,
                "params": {
                    "type": "chatgpt"
                }
            }),
            serde_json::to_value(&request)?,
        );
        Ok(())
    }

    #[test]
    fn serialize_account_logout() -> Result<()> {
        let request = ClientRequest::LogoutAccount {
            request_id: RequestId::Integer(4),
            params: None,
        };
        assert_eq!(
            json!({
                "method": "account/logout",
                "id": 4,
            }),
            serde_json::to_value(&request)?,
        );
        Ok(())
    }

    #[test]
    fn serialize_account_login_chatgpt_auth_tokens() -> Result<()> {
        let request = ClientRequest::LoginAccount {
            request_id: RequestId::Integer(5),
            params: v1::LoginAccountParams::ChatgptAuthTokens {
                access_token: "access-token".to_string(),
                id_token: "id-token".to_string(),
            },
        };
        assert_eq!(
            json!({
                "method": "account/login/start",
                "id": 5,
                "params": {
                    "type": "chatgptAuthTokens",
                    "accessToken": "access-token",
                    "idToken": "id-token"
                }
            }),
            serde_json::to_value(&request)?,
        );
        Ok(())
    }

    #[test]
    fn serialize_get_account() -> Result<()> {
        let request = ClientRequest::GetAccount {
            request_id: RequestId::Integer(6),
            params: v1::GetAccountParams {
                refresh_token: false,
            },
        };
        assert_eq!(
            json!({
                "method": "account/read",
                "id": 6,
                "params": {
                    "refreshToken": false
                }
            }),
            serde_json::to_value(&request)?,
        );
        Ok(())
    }

    #[test]
    fn account_serializes_fields_in_camel_case() -> Result<()> {
        let api_key = v1::Account::ApiKey {};
        assert_eq!(
            json!({
                "type": "apiKey",
            }),
            serde_json::to_value(&api_key)?,
        );

        let chatgpt = v1::Account::Chatgpt {
            email: "user@example.com".to_string(),
            plan_type: PlanType::Plus,
        };
        assert_eq!(
            json!({
                "type": "chatgpt",
                "email": "user@example.com",
                "planType": "plus",
            }),
            serde_json::to_value(&chatgpt)?,
        );

        Ok(())
    }

    #[test]
    fn serialize_list_models() -> Result<()> {
        let request = ClientRequest::ModelList {
            request_id: RequestId::Integer(6),
            params: v1::ModelListParams::default(),
        };
        assert_eq!(
            json!({
                "method": "model/list",
                "id": 6,
                "params": {
                    "limit": null,
                    "cursor": null
                }
            }),
            serde_json::to_value(&request)?,
        );
        Ok(())
    }

    #[test]
    fn serialize_list_collaboration_modes() -> Result<()> {
        let request = ClientRequest::CollaborationModeList {
            request_id: RequestId::Integer(7),
            params: v1::CollaborationModeListParams::default(),
        };
        assert_eq!(
            json!({
                "method": "collaborationMode/list",
                "id": 7,
                "params": {}
            }),
            serde_json::to_value(&request)?,
        );
        Ok(())
    }

    #[test]
    fn mock_experimental_method_is_marked_experimental() {
        let request = ClientRequest::MockExperimentalMethod {
            request_id: RequestId::Integer(1),
            params: v1::MockExperimentalMethodParams::default(),
        };
        let reason = crate::experimental_api::ExperimentalApi::experimental_reason(&request);
        assert_eq!(reason, Some("mock/experimentalMethod"));
    }

    #[test]
    fn session_start_mock_field_is_marked_experimental() {
        let request = ClientRequest::SessionStart {
            request_id: RequestId::Integer(1),
            params: v1::SessionStartParams {
                mock_experimental_field: Some("mock".to_string()),
                ..Default::default()
            },
        };
        let reason = crate::experimental_api::ExperimentalApi::experimental_reason(&request);
        assert_eq!(reason, Some("session/start.mockExperimentalField"));
    }
}
