use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct ApprovalSettings {
    pub enabled: Option<bool>,
    pub auto_approve_safe: Option<bool>,
    pub timeout_secs: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub command: Option<String>,
    pub node: Option<String>,
    pub timestamp: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApprovalsResponse {
    pub pending: Vec<ApprovalRequest>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ExecApprovalFull {
    pub id: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub host: Option<String>,
    pub security: Option<String>,
    pub ask: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub expires_at_ms: Option<u64>,
    pub node: Option<String>,
    pub timestamp: Option<String>,
    pub status: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExecApprovalPolicy {
    pub mode: Option<String>,
    pub rules: Option<Vec<String>>,
}
