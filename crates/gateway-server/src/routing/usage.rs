use std::collections::HashMap;
use std::sync::OnceLock;

use chrono::Utc;
use salvo::prelude::*;
use serde_json::json;
use tokio::sync::Mutex;

static USAGE_STORE: OnceLock<Mutex<UsageStore>> = OnceLock::new();

#[derive(Debug, Clone, Default)]
struct UsageStore {
    total_tokens: u64,
    total_cost_usd: f64,
    requests_count: u64,
    by_model: HashMap<String, ModelUsage>,
    by_day: HashMap<String, DailyUsage>,
}

#[derive(Debug, Clone)]
struct ModelUsage {
    tokens: u64,
    requests: u64,
    cost_usd: f64,
}

#[derive(Debug, Clone)]
struct DailyUsage {
    date: String,
    tokens: u64,
    requests: u64,
    cost_usd: f64,
}

fn usage_store() -> &'static Mutex<UsageStore> {
    USAGE_STORE.get_or_init(|| Mutex::new(UsageStore::default()))
}

pub async fn record_usage(model: &str, tokens: u64, cost_usd: f64) {
    let mut store = usage_store().lock().await;

    store.total_tokens += tokens;
    store.total_cost_usd += cost_usd;
    store.requests_count += 1;

    let model_usage = store
        .by_model
        .entry(model.to_owned())
        .or_insert(ModelUsage {
            tokens: 0,
            requests: 0,
            cost_usd: 0.0,
        });
    model_usage.tokens += tokens;
    model_usage.requests += 1;
    model_usage.cost_usd += cost_usd;

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let daily = store.by_day.entry(today.clone()).or_insert(DailyUsage {
        date: today,
        tokens: 0,
        requests: 0,
        cost_usd: 0.0,
    });
    daily.tokens += tokens;
    daily.requests += 1;
    daily.cost_usd += cost_usd;
}

#[handler]
pub async fn usage_stats_handler(res: &mut Response) {
    let store = usage_store().lock().await;

    let by_model: Vec<_> = store
        .by_model
        .iter()
        .map(|(k, v)| {
            json!({
                "model": k,
                "tokens": v.tokens,
                "requests": v.requests,
                "cost_usd": v.cost_usd,
            })
        })
        .collect();

    res.render(Text::Json(
        json!({
            "total_tokens": store.total_tokens,
            "total_cost_usd": store.total_cost_usd,
            "requests_count": store.requests_count,
            "by_model": by_model,
        })
        .to_string(),
    ));
}

#[handler]
pub async fn usage_cost_handler(res: &mut Response) {
    let store = usage_store().lock().await;

    res.render(Text::Json(
        json!({
            "total_cost_usd": store.total_cost_usd,
            "total_tokens": store.total_tokens,
            "estimated_monthly_usd": store.total_cost_usd * 30.0,
        })
        .to_string(),
    ));
}

#[handler]
pub async fn usage_timeseries_handler(req: &mut Request, res: &mut Response) {
    let days = req.query::<usize>("days").unwrap_or(7);

    let store = usage_store().lock().await;

    let mut timeseries: Vec<_> = store.by_day.values().cloned().collect();
    timeseries.sort_by(|a, b| a.date.cmp(&b.date));
    timeseries.reverse();
    timeseries.truncate(days);

    let data: Vec<_> = timeseries
        .into_iter()
        .map(|d| {
            json!({
                "date": d.date,
                "tokens": d.tokens,
                "requests": d.requests,
                "cost_usd": d.cost_usd,
            })
        })
        .collect();

    res.render(Text::Json(
        json!({
            "timeseries": data,
            "days": days,
        })
        .to_string(),
    ));
}

#[handler]
pub async fn usage_logs_handler(req: &mut Request, res: &mut Response) {
    let limit = req.query::<usize>("limit").unwrap_or(50);

    res.render(Text::Json(
        json!({
            "logs": [],
            "limit": limit,
            "message": "usage logs placeholder",
        })
        .to_string(),
    ));
}
