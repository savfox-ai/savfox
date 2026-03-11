use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct LogEntry {
    pub timestamp: Option<String>,
    pub level: Option<String>,
    pub message: String,
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LogsResponse {
    pub entries: Vec<LogEntry>,
}
