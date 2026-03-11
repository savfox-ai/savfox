use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub version: Option<String>,
    pub installed: Option<bool>,
    pub category: Option<String>,
    pub eligible: Option<bool>,
    pub missing_deps: Option<Vec<String>>,
    #[serde(alias = "requires_key_env")]
    pub primary_env: Option<String>,
    pub env_set: Option<bool>,
    pub enabled: Option<bool>,
    pub description: Option<String>,
    pub disabled_reason: Option<String>,
    pub allowlist_blocked: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct SkillsStatusResponse {
    pub installed_count: Option<u32>,
    pub available_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct SkillsBinsResponse {
    pub bins: Vec<SkillEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SkillDetail {
    pub name: String,
    pub version: Option<String>,
    pub installed: Option<bool>,
    pub category: Option<String>,
    pub eligible: Option<bool>,
    pub missing_deps: Option<Vec<String>>,
    #[serde(alias = "requires_key_env")]
    pub primary_env: Option<String>,
    pub env_set: Option<bool>,
    pub enabled: Option<bool>,
    pub description: Option<String>,
    pub disabled_reason: Option<String>,
    pub allowlist_blocked: Option<bool>,
}
