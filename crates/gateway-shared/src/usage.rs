use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct UsageStatus {
    pub total_tokens: Option<u64>,
    pub total_cost: Option<f64>,
    pub session_count: Option<u32>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsageCostEntry {
    pub session_id: String,
    pub tokens: Option<u64>,
    pub cost: Option<f64>,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct UsageCostResponse {
    pub entries: Vec<UsageCostEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsageDetail {
    pub total_tokens: Option<u64>,
    pub total_cost: Option<f64>,
    pub session_count: Option<u32>,
    pub total_messages: Option<u64>,
    pub tool_calls: Option<u64>,
    pub errors: Option<u64>,
    pub cache_hits: Option<u64>,
    pub cache_misses: Option<u64>,
    pub hourly_distribution: Option<Vec<u64>>,
    pub daily_distribution: Option<Vec<serde_json::Value>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionsUsageEntry {
    pub key: Option<String>,
    pub session_id: Option<String>,
    pub label: Option<String>,
    pub agent_id: Option<String>,
    pub channel: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost: Option<f64>,
    pub context_weight: Option<f64>,
    pub message_count: Option<u32>,
    pub first_activity: Option<i64>,
    pub last_activity: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionsUsageTotals {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cost: Option<f64>,
    pub session_count: Option<u32>,
    pub message_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionsUsageResult {
    pub updated_at: Option<i64>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub sessions: Option<Vec<SessionsUsageEntry>>,
    pub totals: Option<SessionsUsageTotals>,
    pub daily: Option<Vec<serde_json::Value>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CostUsageDailyEntry {
    pub date: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost: Option<f64>,
    pub session_count: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CostUsageSummary {
    pub updated_at: Option<i64>,
    pub days: Option<u32>,
    pub daily: Option<Vec<CostUsageDailyEntry>>,
    pub totals: Option<SessionsUsageTotals>,
}
