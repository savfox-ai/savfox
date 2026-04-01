use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ConfigSchema {
    pub properties: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ConfigSnapshot {
    pub path: Option<String>,
    pub exists: Option<bool>,
    pub valid: Option<bool>,
    pub hash: Option<String>,
    pub config: Option<serde_json::Value>,
    pub issues: Option<Vec<ConfigSnapshotIssue>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ConfigSnapshotIssue {
    pub path: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ConfigPatchResult {
    pub ok: Option<bool>,
    pub path: Option<String>,
    pub key: Option<String>,
    pub entry: Option<serde_json::Value>,
}
