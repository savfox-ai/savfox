use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AgentModels {
    pub primary: Option<String>,
    pub fallbacks: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AgentEntry {
    pub id: Option<String>,
    pub name: String,
    pub model: Option<String>,
    pub models: Option<AgentModels>,
    pub system_prompt: Option<String>,
    pub thinking: Option<String>,
    pub verbose: Option<String>,
    pub status: Option<String>,
    pub created_at: Option<String>,
    pub is_default: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AgentsResponse {
    pub agents: Vec<AgentEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentFile {
    #[serde(alias = "path")]
    pub name: String,
    pub size: Option<u64>,
    pub content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentFilesResponse {
    pub files: Vec<AgentFile>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentDetail {
    pub name: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub status: Option<String>,
    pub created_at: Option<String>,
    pub thinking: Option<String>,
    pub verbose: Option<String>,
    pub emoji: Option<String>,
    pub theme_color: Option<String>,
    pub is_default: Option<bool>,
    pub fallback_models: Option<Vec<String>>,
    pub group_activation: Option<String>,
    pub group_keywords: Option<Vec<String>>,
    pub agent_aliases: Option<Vec<String>>,
    pub ingest_policy: Option<String>,
    pub external_bot_policy: Option<String>,
    /// Unified permission policy (sandbox + approval + tool access).
    pub permission_policy: Option<serde_json::Value>,
    /// List of Matrix appservice channel config IDs for which to auto-create
    /// a virtual user for this agent.
    pub matrix_auto_user_channels: Option<Vec<String>>,
}
