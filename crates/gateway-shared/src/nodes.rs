use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
pub struct NodeEntry {
    pub id: String,
    pub name: Option<String>,
    pub status: Option<String>,
    pub platform: Option<String>,
    pub last_seen: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NodesResponse {
    pub nodes: Vec<NodeEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DevicePairEntry {
    pub id: String,
    pub name: Option<String>,
    pub status: Option<String>,
    pub requested_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeDetail {
    pub id: String,
    pub name: Option<String>,
    pub status: Option<String>,
    pub platform: Option<String>,
    pub last_seen: Option<String>,
    pub capabilities: Option<Vec<String>>,
    pub commands: Option<Vec<String>>,
    pub tokens: Option<Vec<NodeToken>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeToken {
    pub role: Option<String>,
    pub token: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub status: Option<String>,
}
