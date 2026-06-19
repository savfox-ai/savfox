use std::collections::HashMap;

use dioxus::prelude::*;
use serde_json::json;
use wasm_bindgen::JsCast;

use crate::api::types::{UsageCostEntry, UsageCostResponse, UsageDetail};
use crate::api::ws::WsRpc;

// ── Date range enum ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum DateRange {
    Today,
    Week,
    Month,
    All,
}

impl DateRange {
    fn label(&self) -> &'static str {
        match self {
            DateRange::Today => "Today",
            DateRange::Week => "7 Days",
            DateRange::Month => "30 Days",
            DateRange::All => "All Time",
        }
    }

    fn rpc_period(&self) -> &'static str {
        match self {
            DateRange::Today => "today",
            DateRange::Week => "week",
            DateRange::Month => "month",
            DateRange::All => "all",
        }
    }
}

// ── Model breakdown aggregation ─────────────────────────────────────────────

#[derive(Clone, PartialEq)]
struct ModelBreakdown {
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    total_cost: f64,
    estimated_cost: f64,
    session_count: u32,
}

fn compute_model_breakdown(entries: &[UsageCostEntry]) -> Vec<ModelBreakdown> {
    let mut map: HashMap<String, (u64, u64, u64, f64, u32)> = HashMap::new();
    for e in entries {
        let model = e.model.as_deref().unwrap_or("unknown").to_string();
        let entry = map.entry(model).or_insert((0, 0, 0, 0.0, 0));
        entry.0 += e.input_tokens.unwrap_or(0);
        entry.1 += e.output_tokens.unwrap_or(0);
        entry.2 += e.tokens.unwrap_or(0);
        entry.3 += e.cost.unwrap_or(0.0);
        entry.4 += 1;
    }
    let mut result: Vec<ModelBreakdown> = map
        .into_iter()
        .map(|(model, (inp, out, total, cost, count))| {
            let est = total as f64 / 1000.0 * estimate_cost_per_1k(&model);
            ModelBreakdown {
                model,
                input_tokens: inp,
                output_tokens: out,
                total_tokens: total,
                total_cost: cost,
                estimated_cost: est,
                session_count: count,
            }
        })
        .collect();
    // Sort by estimated cost descending (requirement: sort by cost descending).
    result.sort_by(|a, b| {
        let cost_a = if a.total_cost > 0.0 {
            a.total_cost
        } else {
            a.estimated_cost
        };
        let cost_b = if b.total_cost > 0.0 {
            b.total_cost
        } else {
            b.estimated_cost
        };
        cost_b
            .partial_cmp(&cost_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    result
}

// ── Cost estimation per 1K tokens ───────────────────────────────────────────

/// Pricing estimates per 1K tokens (blended input/output average).
fn estimate_cost_per_1k(model: &str) -> f64 {
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") {
        0.0003
    } else if m.contains("gpt-4o") {
        0.005
    } else if m.contains("gpt-4-turbo") || m.contains("gpt-4-1") {
        0.02
    } else if m.contains("gpt-4") {
        0.03
    } else if m.contains("gpt-3.5") || m.contains("gpt-35") {
        0.0015
    } else if m.contains("o1-mini") || m.contains("o3-mini") || m.contains("o4-mini") {
        0.003
    } else if m.contains("o1")
        || m.contains("o3")
        || m.contains("claude-3-opus")
        || m.contains("claude-opus")
        || m.contains("opus")
    {
        0.015
    } else if m.contains("claude-3-sonnet")
        || m.contains("claude-3.5-sonnet")
        || m.contains("claude-sonnet")
        || m.contains("sonnet")
    {
        0.003
    } else if m.contains("claude-3-haiku")
        || m.contains("claude-3.5-haiku")
        || m.contains("claude-haiku")
        || m.contains("haiku")
    {
        0.0008
    } else if m.contains("claude") {
        0.008
    } else if m.contains("gemini-2") || m.contains("gemini-1.5-pro") {
        0.003
    } else if m.contains("gemini") {
        0.001
    } else if m.contains("llama") || m.contains("mistral") || m.contains("deepseek") {
        0.0005
    } else if m.contains("qwen") || m.contains("phi") {
        0.0003
    } else {
        0.002
    }
}

/// Estimate input cost per 1K tokens.
fn estimate_input_cost_per_1k(model: &str) -> f64 {
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") {
        0.00015
    } else if m.contains("gpt-4o") {
        0.0025
    } else if m.contains("gpt-4") {
        0.03
    } else if m.contains("claude-3-opus") || m.contains("opus") {
        0.015
    } else if m.contains("sonnet") {
        0.003
    } else if m.contains("haiku") {
        0.00025
    } else if m.contains("claude") {
        0.003
    } else {
        0.001
    }
}

/// Estimate output cost per 1K tokens.
fn estimate_output_cost_per_1k(model: &str) -> f64 {
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") {
        0.0006
    } else if m.contains("gpt-4o") {
        0.01
    } else if m.contains("gpt-4") {
        0.06
    } else if m.contains("claude-3-opus") || m.contains("opus") {
        0.075
    } else if m.contains("sonnet") {
        0.015
    } else if m.contains("haiku") {
        0.00125
    } else if m.contains("claude") {
        0.015
    } else {
        0.003
    }
}

/// Calculate detailed cost estimate given input/output token split.
fn estimate_session_cost(model: &str, input: u64, output: u64) -> f64 {
    let inp_cost = input as f64 / 1000.0 * estimate_input_cost_per_1k(model);
    let out_cost = output as f64 / 1000.0 * estimate_output_cost_per_1k(model);
    inp_cost + out_cost
}

// ── Main Component ──────────────────────────────────────────────────────────

#[component]
pub fn Usage() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut date_range = use_signal(|| DateRange::All);
    let mut show_session_table = use_signal(|| false);
    let session_sort_col = use_signal(|| SessionSortCol::Cost);
    let session_sort_asc = use_signal(|| false);

    // Fetch extended usage detail (for summary cards, hourly heatmap).
    let ws_detail = ws.clone();
    let detail_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_detail.clone();
        async move { ws.call::<UsageDetail>("usage.status", None).await.ok() }
    });

    // Fetch per-session cost entries with period filtering.
    let ws_cost = ws.clone();
    let cost_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let _dr = date_range();
        let ws = ws_cost.clone();
        let period = date_range().rpc_period().to_string();
        async move {
            ws.call::<UsageCostResponse>("usage.cost", Some(json!({ "period": period })))
                .await
                .map(|r| r.entries)
                .unwrap_or_default()
        }
    });

    let detail_read = detail_data.read();
    let detail = detail_read.as_ref().and_then(|d| d.as_ref());
    let cost_entries: Vec<UsageCostEntry> = cost_data.read().as_ref().cloned().unwrap_or_default();

    // ── Summary values ──────────────────────────────────────────────

    let total_tokens = detail
        .and_then(|d| d.total_tokens)
        .map(format_number)
        .unwrap_or_else(|| "-".into());
    let total_cost = detail
        .and_then(|d| d.total_cost)
        .map(|c| format!("${c:.4}"))
        .unwrap_or_else(|| "-".into());
    let session_count = detail
        .and_then(|d| d.session_count)
        .map(|c| c.to_string())
        .unwrap_or_else(|| "-".into());
    let total_messages = detail
        .and_then(|d| d.total_messages)
        .map(format_number)
        .unwrap_or_else(|| "-".into());
    let tool_calls = detail
        .and_then(|d| d.tool_calls)
        .map(format_number)
        .unwrap_or_else(|| "-".into());
    let errors = detail
        .and_then(|d| d.errors)
        .map(format_number)
        .unwrap_or_else(|| "-".into());

    // Derived stats
    let avg_tokens_per_msg = detail
        .and_then(|d| {
            d.total_tokens?
                .checked_div(d.total_messages?)
                .map(format_number)
        })
        .unwrap_or_else(|| "-".into());

    let error_rate = detail
        .and_then(|d| {
            let msgs = d.total_messages?;
            let errs = d.errors?;
            if msgs > 0 {
                Some(format!("{:.1}%", errs as f64 / msgs as f64 * 100.0))
            } else {
                None
            }
        })
        .unwrap_or_else(|| "-".into());

    let cache_hit_rate = detail
        .and_then(|d| {
            let hits = d.cache_hits?;
            let misses = d.cache_misses?;
            let total = hits + misses;
            if total > 0 {
                Some(format!("{:.1}%", hits as f64 / total as f64 * 100.0))
            } else {
                None
            }
        })
        .unwrap_or_else(|| "-".into());

    // ── Model breakdown ─────────────────────────────────────────────
    // Cached aggregations driven by the `cost_data` resource so they only
    // recompute when the underlying cost entries change.

    let model_breakdown_memo = use_memo(move || {
        compute_model_breakdown(&cost_data.read().as_ref().cloned().unwrap_or_default())
    });
    let model_breakdown = model_breakdown_memo.read();

    // ── Cost calculations ───────────────────────────────────────────

    // (estimated_cost, actual_cost, grand_total_cost, total_input, total_output)
    let cost_totals = use_memo(move || {
        let entries = cost_data.read().as_ref().cloned().unwrap_or_default();
        // Estimated cost using input/output split pricing when available.
        let estimated_cost: f64 = entries
            .iter()
            .filter(|e| e.cost.is_none())
            .map(|e| {
                let model = e.model.as_deref().unwrap_or("");
                let inp = e.input_tokens.unwrap_or(0);
                let out = e.output_tokens.unwrap_or(0);
                if inp > 0 || out > 0 {
                    estimate_session_cost(model, inp, out)
                } else {
                    let total = e.tokens.unwrap_or(0) as f64;
                    total / 1000.0 * estimate_cost_per_1k(model)
                }
            })
            .sum();
        let actual_cost: f64 = entries.iter().filter_map(|e| e.cost).sum();
        let grand_total_cost = actual_cost + estimated_cost;
        let total_input_tokens: u64 = entries.iter().filter_map(|e| e.input_tokens).sum();
        let total_output_tokens: u64 = entries.iter().filter_map(|e| e.output_tokens).sum();
        (
            estimated_cost,
            actual_cost,
            grand_total_cost,
            total_input_tokens,
            total_output_tokens,
        )
    });
    let (_estimated_cost, actual_cost, grand_total_cost, total_input_tokens, total_output_tokens) =
        cost_totals();

    // Hourly distribution
    let hourly = detail
        .and_then(|d| d.hourly_distribution.as_ref())
        .cloned()
        .unwrap_or_default();

    // Daily distribution (last 14 days)
    let daily: Vec<(String, u64, f64)> = detail
        .and_then(|d| d.daily_distribution.as_ref())
        .map(|days| {
            days.iter()
                .map(|d| {
                    let date = d
                        .get("date")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let tokens = d.get("tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cost = d.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    (date, tokens, cost)
                })
                .collect()
        })
        .unwrap_or_default();

    // Per-session sorted order (indices into the entry list). Cached and only
    // computed when the session table is actually shown; returns an empty order
    // otherwise. `UsageCostEntry` is not `PartialEq`, so we memoize the cheap,
    // comparable index permutation instead of cloned entries.
    let sorted_order = use_memo(move || {
        if !show_session_table() {
            return Vec::<usize>::new();
        }
        let entries = cost_data.read().as_ref().cloned().unwrap_or_default();
        let mut order: Vec<usize> = (0..entries.len()).collect();
        let col = session_sort_col();
        let asc = session_sort_asc();
        order.sort_by(|&ia, &ib| {
            let a = &entries[ia];
            let b = &entries[ib];
            let cmp = match col {
                SessionSortCol::Session => a.session_id.cmp(&b.session_id),
                SessionSortCol::Model => {
                    let am = a.model.as_deref().unwrap_or("");
                    let bm = b.model.as_deref().unwrap_or("");
                    am.cmp(bm)
                }
                SessionSortCol::Input => {
                    let ai = a.input_tokens.unwrap_or(0);
                    let bi = b.input_tokens.unwrap_or(0);
                    ai.cmp(&bi)
                }
                SessionSortCol::Output => {
                    let ao = a.output_tokens.unwrap_or(0);
                    let bo = b.output_tokens.unwrap_or(0);
                    ao.cmp(&bo)
                }
                SessionSortCol::Total => {
                    let at = a.tokens.unwrap_or(0);
                    let bt = b.tokens.unwrap_or(0);
                    at.cmp(&bt)
                }
                SessionSortCol::Cost => {
                    let ac = session_entry_cost(a);
                    let bc = session_entry_cost(b);
                    ac.partial_cmp(&bc).unwrap_or(std::cmp::Ordering::Equal)
                }
            };
            if asc { cmp } else { cmp.reverse() }
        });
        order
    });

    let ranges = [
        DateRange::Today,
        DateRange::Week,
        DateRange::Month,
        DateRange::All,
    ];

    rsx! {
        div { class: "page-content usage-page",
            // ── Header toolbar ──────────────────────────────────────
            div { class: "usage-header",
                h2 { class: "usage-title", "Usage Analytics" }
                div { class: "usage-header__actions",
                    // Time aggregation toggle
                    div { class: "usage-range-toggle",
                        for range in ranges.iter() {
                            {
                                let r = *range;
                                let is_active = date_range() == r;
                                let cls = if is_active { "usage-range-btn usage-range-btn--active" } else { "usage-range-btn" };
                                rsx! {
                                    button {
                                        key: "{r.label()}",
                                        class: "{cls}",
                                        onclick: move |_| date_range.set(r),
                                        "{r.label()}"
                                    }
                                }
                            }
                        }
                    }
                    button {
                        class: "usage-action-btn",
                        onclick: {
                            let cost_entries = cost_entries.clone();
                            move |_| export_csv(&cost_entries)
                        },
                        "Export CSV"
                    }
                    button {
                        class: "usage-action-btn",
                        onclick: {
                            let cost_entries = cost_entries.clone();
                            move |_| export_json(&cost_entries)
                        },
                        "Export JSON"
                    }
                    button {
                        class: "usage-action-btn",
                        onclick: move |_| refresh_tick += 1,
                        "Refresh"
                    }
                }
            }

            // ── Prominent total cost hero card ──────────────────────
            div { class: "usage-cost-hero",
                div { class: "usage-cost-hero__main",
                    div { class: "usage-cost-hero__label", "Estimated Total Cost" }
                    div { class: "usage-cost-hero__value", "${grand_total_cost:.4}" }
                    div { class: "usage-cost-hero__period", "{date_range().label()}" }
                }
                div { class: "usage-cost-hero__breakdown",
                    div { class: "usage-cost-hero__item",
                        span { class: "usage-cost-hero__item-label", "Input Tokens" }
                        span { class: "usage-cost-hero__item-value", "{format_number(total_input_tokens)}" }
                    }
                    div { class: "usage-cost-hero__item",
                        span { class: "usage-cost-hero__item-label", "Output Tokens" }
                        span { class: "usage-cost-hero__item-value", "{format_number(total_output_tokens)}" }
                    }
                    div { class: "usage-cost-hero__item",
                        span { class: "usage-cost-hero__item-label", "Reported Cost" }
                        span { class: "usage-cost-hero__item-value", "{total_cost}" }
                    }
                    if actual_cost > 0.0 {
                        div { class: "usage-cost-hero__item",
                            span { class: "usage-cost-hero__item-label", "Actual" }
                            span { class: "usage-cost-hero__item-value", "${actual_cost:.4}" }
                        }
                    }
                }
            }

            // ── Summary stat cards ──────────────────────────────────
            div { class: "usage-stat-grid",
                { stat_card("Total Tokens", &total_tokens) }
                { stat_card("Sessions", &session_count) }
                { stat_card("Total Messages", &total_messages) }
                { stat_card("Tool Calls", &tool_calls) }
                { stat_card("Errors", &errors) }
                { stat_card("Avg Tok/Msg", &avg_tokens_per_msg) }
                { stat_card("Error Rate", &error_rate) }
                { stat_card("Cache Hit Rate", &cache_hit_rate) }
            }

            // ── Token usage bar chart (per model) ───────────────────
            if !model_breakdown.is_empty() {
                div { class: "usage-card",
                    h3 { class: "usage-card__title", "Token Usage by Model" }
                    { render_token_bars(&model_breakdown) }
                }
            }

            // ── Hourly heatmap ──────────────────────────────────────
            if !hourly.is_empty() {
                div { class: "usage-card",
                    h3 { class: "usage-card__title", "Hourly Distribution" }
                    div { class: "heatmap-grid",
                        for (hour, count) in hourly.iter().enumerate() {
                            { render_heatmap_cell(hour, *count, hourly.iter().copied().max().unwrap_or(1)) }
                        }
                    }
                    div { class: "heatmap-labels",
                        span { "0:00" }
                        span { "6:00" }
                        span { "12:00" }
                        span { "18:00" }
                        span { "23:00" }
                    }
                }
            }

            // ── Daily time series chart ───────────────────────────────
            if !daily.is_empty() {
                div { class: "usage-card",
                    h3 { class: "usage-card__title", "Daily Token Usage" }
                    { render_daily_chart(&daily) }
                }
            }

            // ── Per-model token breakdown table ─────────────────────
            if !model_breakdown.is_empty() {
                div { class: "usage-card",
                    h3 { class: "usage-card__title", "Per-Model Token Breakdown" }
                    div { class: "usage-table-wrap",
                        table { class: "usage-table",
                            thead {
                                tr {
                                    th { class: "usage-th", "Model" }
                                    th { class: "usage-th usage-th--right", "Input Tokens" }
                                    th { class: "usage-th usage-th--right", "Output Tokens" }
                                    th { class: "usage-th usage-th--right", "Total Tokens" }
                                    th { class: "usage-th usage-th--right", "Sessions" }
                                    th { class: "usage-th usage-th--right", "Est. Cost" }
                                    th { class: "usage-th usage-th--right", "$/1K" }
                                }
                            }
                            tbody {
                                for (i, mb) in model_breakdown.iter().enumerate() {
                                    {
                                        let inp_str = format_number(mb.input_tokens);
                                        let out_str = format_number(mb.output_tokens);
                                        let total_str = format_number(mb.total_tokens);
                                        let cost_str = if mb.total_cost > 0.0 {
                                            format!("${:.4}", mb.total_cost)
                                        } else {
                                            format!("~${:.4}", mb.estimated_cost)
                                        };
                                        let rate = estimate_cost_per_1k(&mb.model);
                                        rsx! {
                                            tr { key: "{i}", class: "usage-tr",
                                                td { class: "usage-td", code { class: "usage-model-code", "{mb.model}" } }
                                                td { class: "usage-td usage-td--mono usage-td--right", "{inp_str}" }
                                                td { class: "usage-td usage-td--mono usage-td--right", "{out_str}" }
                                                td { class: "usage-td usage-td--mono usage-td--right usage-td--bold", "{total_str}" }
                                                td { class: "usage-td usage-td--mono usage-td--center", "{mb.session_count}" }
                                                td { class: "usage-td usage-td--mono usage-td--right", "{cost_str}" }
                                                td { class: "usage-td usage-td--mono usage-td--right usage-td--muted", "${rate:.4}" }
                                            }
                                        }
                                    }
                                }
                            }
                            // Table footer with totals
                            tfoot {
                                tr { class: "usage-tr usage-tr--footer",
                                    td { class: "usage-td usage-td--bold", "Total" }
                                    td { class: "usage-td usage-td--mono usage-td--right usage-td--bold", "{format_number(total_input_tokens)}" }
                                    td { class: "usage-td usage-td--mono usage-td--right usage-td--bold", "{format_number(total_output_tokens)}" }
                                    td { class: "usage-td usage-td--mono usage-td--right usage-td--bold",
                                        "{format_number(total_input_tokens + total_output_tokens)}"
                                    }
                                    td { class: "usage-td usage-td--mono usage-td--center usage-td--bold", "{cost_entries.len()}" }
                                    td { class: "usage-td usage-td--mono usage-td--right usage-td--bold", "${grand_total_cost:.4}" }
                                    td { class: "usage-td" }
                                }
                            }
                        }
                    }
                }
            }

            // ── Per-session cost tracking table (collapsible) ───────
            div { class: "usage-card",
                div {
                    class: "usage-card__toggle",
                    role: "button",
                    tabindex: "0",
                    onclick: move |_| show_session_table.toggle(),
                    h3 { class: "usage-card__title usage-card__title--inline", "Per-Session Cost Tracking" }
                    span { class: "usage-card__toggle-hint",
                        if show_session_table() { "Hide" } else { "Show ({cost_entries.len()} sessions)" }
                    }
                }
                if show_session_table() {
                    if sorted_order.read().is_empty() {
                        p { class: "usage-empty", "No session usage data available for this period" }
                    } else {
                        div { class: "usage-table-wrap",
                            table { class: "usage-table",
                                thead {
                                    tr {
                                        { sortable_th("Session", SessionSortCol::Session, session_sort_col, session_sort_asc, session_sort_col) }
                                        { sortable_th("Model", SessionSortCol::Model, session_sort_col, session_sort_asc, session_sort_col) }
                                        { sortable_th("Input", SessionSortCol::Input, session_sort_col, session_sort_asc, session_sort_col) }
                                        { sortable_th("Output", SessionSortCol::Output, session_sort_col, session_sort_asc, session_sort_col) }
                                        { sortable_th("Total", SessionSortCol::Total, session_sort_col, session_sort_asc, session_sort_col) }
                                        { sortable_th("Est. Cost", SessionSortCol::Cost, session_sort_col, session_sort_asc, session_sort_col) }
                                    }
                                }
                                tbody {
                                    for idx in sorted_order.read().iter().copied() {
                                        {
                                            let entry = &cost_entries[idx];
                                            let short_id = truncate_session_id(&entry.session_id);
                                            let model = entry.model.as_deref().unwrap_or("-").to_string();
                                            let inp = entry.input_tokens.map(format_number).unwrap_or_else(|| "-".into());
                                            let out = entry.output_tokens.map(format_number).unwrap_or_else(|| "-".into());
                                            let total = entry.tokens.map(format_number).unwrap_or_else(|| "-".into());
                                            let cost = format_cost_entry(entry);
                                            rsx! {
                                                tr { key: "{entry.session_id}", class: "usage-tr",
                                                    td { class: "usage-td", title: "{entry.session_id}",
                                                        code { class: "usage-session-code", "{short_id}" }
                                                    }
                                                    td { class: "usage-td", "{model}" }
                                                    td { class: "usage-td usage-td--mono usage-td--right", "{inp}" }
                                                    td { class: "usage-td usage-td--mono usage-td--right", "{out}" }
                                                    td { class: "usage-td usage-td--mono usage-td--right usage-td--bold", "{total}" }
                                                    td { class: "usage-td usage-td--mono usage-td--right", "{cost}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        style { {USAGE_STYLES} }
    }
}

// ── Session sorting ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum SessionSortCol {
    Session,
    Model,
    Input,
    Output,
    Total,
    Cost,
}

fn session_entry_cost(e: &UsageCostEntry) -> f64 {
    if let Some(c) = e.cost {
        return c;
    }
    let model = e.model.as_deref().unwrap_or("");
    let inp = e.input_tokens.unwrap_or(0);
    let out = e.output_tokens.unwrap_or(0);
    if inp > 0 || out > 0 {
        estimate_session_cost(model, inp, out)
    } else {
        let total = e.tokens.unwrap_or(0) as f64;
        total / 1000.0 * estimate_cost_per_1k(model)
    }
}

fn format_cost_entry(e: &UsageCostEntry) -> String {
    if let Some(c) = e.cost {
        return format!("${c:.4}");
    }
    let est = session_entry_cost(e);
    if est > 0.0 {
        format!("~${est:.4}")
    } else {
        "-".into()
    }
}

fn truncate_session_id(id: &str) -> String {
    if id.chars().count() > 24 {
        let truncated: String = id.chars().take(24).collect();
        format!("{truncated}...")
    } else {
        id.to_string()
    }
}

fn sortable_th(
    label: &str,
    col: SessionSortCol,
    current: Signal<SessionSortCol>,
    mut sort_asc: Signal<bool>,
    mut sort_col: Signal<SessionSortCol>,
) -> Element {
    let is_active = current() == col;
    let asc = sort_asc();
    let indicator = if is_active {
        if asc { " ^" } else { " v" }
    } else {
        ""
    };
    let cls = if is_active {
        "usage-th usage-th--sortable usage-th--active"
    } else {
        "usage-th usage-th--sortable"
    };
    let is_numeric = matches!(
        col,
        SessionSortCol::Input
            | SessionSortCol::Output
            | SessionSortCol::Total
            | SessionSortCol::Cost
    );
    let cls = if is_numeric {
        format!("{cls} usage-th--right")
    } else {
        cls.to_string()
    };

    rsx! {
        th {
            class: "{cls}",
            onclick: move |_| {
                if current() == col {
                    sort_asc.toggle();
                } else {
                    sort_col.set(col);
                    sort_asc.set(false);
                }
            },
            "{label}{indicator}"
        }
    }
}

// ── Heatmap cell ────────────────────────────────────────────────────────────

fn render_heatmap_cell(hour: usize, count: u64, max: u64) -> Element {
    let intensity = if max > 0 {
        (count as f64 / max as f64 * 0.8 + 0.1).min(1.0)
    } else {
        0.1
    };
    let opacity = if count == 0 { 0.15 } else { intensity };
    let label = format!("{hour}:00 - {count}");

    rsx! {
        div {
            key: "{hour}",
            class: "heatmap-cell",
            title: "{label}",
            style: "background:rgba(99,102,241,{opacity});",
        }
    }
}

// ── Token usage bar chart (input/output split) ──────────────────────────────

fn render_token_bars(breakdown: &[ModelBreakdown]) -> Element {
    let max_tokens = breakdown.iter().map(|m| m.total_tokens).max().unwrap_or(1);

    rsx! {
        div { class: "bar-chart-container",
            for mb in breakdown.iter() {
                {
                    let total_pct = if max_tokens > 0 {
                        (mb.total_tokens as f64 / max_tokens as f64 * 100.0).max(2.0)
                    } else {
                        2.0
                    };
                    let input_pct = if mb.total_tokens > 0 {
                        mb.input_tokens as f64 / mb.total_tokens as f64 * 100.0
                    } else {
                        50.0
                    };
                    let tokens_str = format_number(mb.total_tokens);
                    let cost_str = if mb.total_cost > 0.0 {
                        format!("${:.4}", mb.total_cost)
                    } else {
                        format!("~${:.4}", mb.estimated_cost)
                    };
                    rsx! {
                        div { key: "{mb.model}", class: "bar-chart-row",
                            div { class: "bar-chart-label", "{mb.model}" }
                            div { class: "bar-chart-track",
                                div {
                                    class: "bar-chart-bar",
                                    style: "width:{total_pct:.1}%;",
                                    // Input portion
                                    div {
                                        class: "bar-chart-bar__input",
                                        style: "width:{input_pct:.1}%;",
                                    }
                                }
                            }
                            div { class: "bar-chart-value", "{tokens_str}" }
                            div { class: "bar-chart-cost", "{cost_str}" }
                        }
                    }
                }
            }
            // Legend
            div { class: "bar-chart-legend",
                div { class: "bar-chart-legend__item",
                    div { class: "bar-chart-legend__swatch bar-chart-legend__swatch--input" }
                    span { "Input" }
                }
                div { class: "bar-chart-legend__item",
                    div { class: "bar-chart-legend__swatch bar-chart-legend__swatch--output" }
                    span { "Output" }
                }
            }
        }
    }
}

// ── CSV Export ───────────────────────────────────────────────────────────────

/// Escape a value for CSV: wrap in quotes if it contains comma, quote, or newline.
fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn export_csv(entries: &[UsageCostEntry]) {
    let mut csv = String::from("Date,Model,Input Tokens,Output Tokens,Total Tokens,Cost\n");
    for e in entries {
        let session = csv_escape(&e.session_id);
        let model = csv_escape(e.model.as_deref().unwrap_or(""));
        let inp = e.input_tokens.map(|t| t.to_string()).unwrap_or_default();
        let out = e.output_tokens.map(|t| t.to_string()).unwrap_or_default();
        let total = e.tokens.map(|t| t.to_string()).unwrap_or_default();
        let cost = format!("{:.6}", session_entry_cost(e));
        csv.push_str(&format!(
            "{},{},{},{},{},{}\n",
            session, model, inp, out, total, cost,
        ));
    }

    if let Some(window) = web_sys::window() {
        let blob_opts = web_sys::BlobPropertyBag::new();
        blob_opts.set_type("text/csv");
        if let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(
            &js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(&csv)),
            &blob_opts,
        ) {
            if let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) {
                if let Some(doc) = window.document() {
                    if let Ok(a) = doc.create_element("a") {
                        let _ = a.set_attribute("href", &url);
                        let _ = a.set_attribute("download", "savfox-usage-export.csv");
                        let _ = a.set_attribute("style", "display:none");
                        if let Some(body) = doc.body() {
                            let _ = body.append_child(&a);
                            if let Some(el) = a.dyn_ref::<web_sys::HtmlElement>() {
                                el.click();
                            }
                            let _ = body.remove_child(&a);
                        }
                        let _ = web_sys::Url::revoke_object_url(&url);
                    }
                }
            }
        }
    }
}

// ── Daily Time Series Chart ───────────────────────────────────────────────────

fn render_daily_chart(daily: &[(String, u64, f64)]) -> Element {
    let max_tokens = daily.iter().map(|(_, t, _)| *t).max().unwrap_or(1);

    rsx! {
        div { class: "daily-chart-container",
            // Chart bars
            div { class: "daily-chart-bars",
                for (date, tokens, cost) in daily.iter() {
                    {
                        let height_pct = if max_tokens > 0 {
                            (*tokens as f64 / max_tokens as f64 * 100.0).max(2.0)
                        } else {
                            2.0
                        };
                        let tokens_str = format_number(*tokens);
                        let cost_str = if *cost > 0.0 { format!("${:.4}", cost) } else { "-".into() };
                        let short_date = if date.len() > 5 { &date[5..] } else { date };
                        rsx! {
                            div { class: "daily-chart-bar-wrapper",
                                div { class: "daily-chart-bar",
                                    style: "height:{height_pct:.1}%;",
                                    title: "{date}: {tokens_str} tokens, {cost_str}"
                                }
                                div { class: "daily-chart-label", "{short_date}" }
                            }
                        }
                    }
                }
            }
            // Legend
            div { class: "daily-chart-legend",
                span { "Daily token usage (last 14 days)" }
            }
        }
    }
}

// ── JSON Export ───────────────────────────────────────────────────────────────

fn export_json(entries: &[UsageCostEntry]) {
    let json_data: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "session_id": e.session_id,
                "model": e.model,
                "input_tokens": e.input_tokens,
                "output_tokens": e.output_tokens,
                "total_tokens": e.tokens,
                "cost": e.cost,
                "estimated_cost": session_entry_cost(e)
            })
        })
        .collect();

    let json_str = serde_json::to_string_pretty(&json_data).unwrap_or_else(|_| "[]".into());

    if let Some(window) = web_sys::window() {
        let blob_opts = web_sys::BlobPropertyBag::new();
        blob_opts.set_type("application/json");
        if let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(
            &js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(&json_str)),
            &blob_opts,
        ) {
            if let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) {
                if let Some(doc) = window.document() {
                    if let Ok(a) = doc.create_element("a") {
                        let _ = a.set_attribute("href", &url);
                        let _ = a.set_attribute("download", "savfox-usage-export.json");
                        let _ = a.set_attribute("style", "display:none");
                        if let Some(body) = doc.body() {
                            let _ = body.append_child(&a);
                            if let Some(el) = a.dyn_ref::<web_sys::HtmlElement>() {
                                el.click();
                            }
                            let _ = body.remove_child(&a);
                        }
                        let _ = web_sys::Url::revoke_object_url(&url);
                    }
                }
            }
        }
    }
}

// ── Stat card helper ────────────────────────────────────────────────────────

fn stat_card(label: &str, value: &str) -> Element {
    rsx! {
        div { class: "usage-stat-card",
            div { class: "usage-stat-card__label", "{label}" }
            div { class: "usage-stat-card__value", "{value}" }
        }
    }
}

// ── Number formatting ───────────────────────────────────────────────────────

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// ── Styles ──────────────────────────────────────────────────────────────────

const USAGE_STYLES: &str = r#"
    .usage-page {
        /* layout handled by .page-content */
    }

    /* ── Header ── */
    .usage-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 24px;
        flex-wrap: wrap;
        gap: 12px;
    }

    .usage-title {
        font-size: 20px;
        font-weight: 600;
    }

    .usage-header__actions {
        display: flex;
        gap: 8px;
        align-items: center;
        flex-wrap: wrap;
    }

    /* ── Time range toggle ── */
    .usage-range-toggle {
        display: flex;
        background: var(--bg-tertiary);
        border-radius: var(--radius);
        padding: 2px;
    }

    .usage-range-btn {
        padding: 5px 14px;
        background: transparent;
        color: var(--text-secondary);
        border: none;
        border-radius: 6px;
        font-size: 12px;
        font-weight: 500;
        cursor: pointer;
        transition: all 0.15s ease;
    }

    .usage-range-btn:hover {
        color: var(--text-primary);
        background: var(--bg-hover);
    }

    .usage-range-btn--active {
        background: var(--accent);
        color: #fff;
    }

    .usage-range-btn--active:hover {
        background: var(--accent);
        color: #fff;
    }

    .usage-action-btn {
        padding: 6px 14px;
        background: var(--bg-tertiary);
        color: var(--text-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        font-size: 13px;
        cursor: pointer;
        transition: all 0.15s ease;
    }

    .usage-action-btn:hover {
        background: var(--bg-hover);
        color: var(--text-primary);
    }

    /* ── Cost hero card ── */
    .usage-cost-hero {
        background: linear-gradient(135deg, var(--bg-secondary) 0%, var(--bg-tertiary) 100%);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        padding: 24px;
        margin-bottom: 20px;
        display: flex;
        gap: 32px;
        align-items: center;
        flex-wrap: wrap;
    }

    .usage-cost-hero__main {
        min-width: 200px;
    }

    .usage-cost-hero__label {
        font-size: 12px;
        color: var(--text-muted);
        margin-bottom: 4px;
    }

    .usage-cost-hero__value {
        font-size: 36px;
        font-weight: 700;
        font-family: var(--font-mono);
        color: var(--accent);
        line-height: 1.1;
    }

    .usage-cost-hero__period {
        font-size: 12px;
        color: var(--text-muted);
        margin-top: 4px;
    }

    .usage-cost-hero__breakdown {
        display: flex;
        gap: 24px;
        flex-wrap: wrap;
        flex: 1;
    }

    .usage-cost-hero__item {
        display: flex;
        flex-direction: column;
        gap: 2px;
    }

    .usage-cost-hero__item-label {
        font-size: 11px;
        color: var(--text-muted);
    }

    .usage-cost-hero__item-value {
        font-size: 18px;
        font-weight: 600;
        font-family: var(--font-mono);
    }

    /* ── Stat cards grid ── */
    .usage-stat-grid {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
        gap: 12px;
        margin-bottom: 20px;
    }

    .usage-stat-card {
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        padding: 14px 16px;
    }

    .usage-stat-card__label {
        font-size: 11px;
        color: var(--text-muted);
        margin-bottom: 4px;
    }

    .usage-stat-card__value {
        font-size: 20px;
        font-weight: 600;
        font-family: var(--font-mono);
    }

    /* ── Cards ── */
    .usage-card {
        background: var(--bg-secondary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        padding: 20px;
        margin-bottom: 16px;
    }

    .usage-card__title {
        font-size: 14px;
        font-weight: 600;
        margin-bottom: 14px;
        color: var(--text-secondary);
    }

    .usage-card__title--inline {
        margin-bottom: 0;
    }

    .usage-card__toggle {
        display: flex;
        justify-content: space-between;
        align-items: center;
        cursor: pointer;
        user-select: none;
    }

    .usage-card__toggle:hover .usage-card__toggle-hint {
        color: var(--text-secondary);
    }

    .usage-card__toggle-hint {
        font-size: 12px;
        color: var(--text-muted);
        transition: color 0.15s;
    }

    .usage-empty {
        color: var(--text-muted);
        font-size: 14px;
        margin-top: 12px;
    }

    /* ── Tables ── */
    .usage-table-wrap {
        border: 1px solid var(--border);
        border-radius: var(--radius);
        overflow-x: auto;
        margin-top: 4px;
    }

    .usage-table {
        width: 100%;
        border-collapse: collapse;
    }

    .usage-th {
        text-align: left;
        padding: 10px 14px;
        font-size: 11px;
        font-weight: 600;
        color: var(--text-secondary);
        background: var(--bg-tertiary);
        white-space: nowrap;
    }

    .usage-th--right {
        text-align: right;
    }

    .usage-th--sortable {
        cursor: pointer;
        user-select: none;
        transition: color 0.15s;
    }

    .usage-th--sortable:hover {
        color: var(--text-primary);
    }

    .usage-th--active {
        color: var(--accent);
    }

    .usage-tr {
        border-top: 1px solid var(--border);
        transition: background 0.1s;
    }

    .usage-tr:hover {
        background: var(--bg-hover);
    }

    .usage-tr--footer {
        background: var(--bg-tertiary);
        border-top: 2px solid var(--border);
    }

    .usage-td {
        padding: 10px 14px;
        font-size: 13px;
    }

    .usage-td--mono {
        font-family: var(--font-mono);
    }

    .usage-td--right {
        text-align: right;
    }

    .usage-td--center {
        text-align: center;
    }

    .usage-td--bold {
        font-weight: 600;
    }

    .usage-td--muted {
        color: var(--text-muted);
    }

    .usage-model-code {
        font-size: 13px;
        padding: 2px 6px;
        background: var(--bg-tertiary);
        border-radius: 4px;
    }

    .usage-session-code {
        font-size: 11px;
        padding: 2px 6px;
        background: var(--bg-tertiary);
        border-radius: 4px;
    }

    /* ── Bar chart ── */
    .bar-chart-container {
        display: flex;
        flex-direction: column;
        gap: 10px;
    }

    .bar-chart-row {
        display: flex;
        align-items: center;
        gap: 10px;
    }

    .bar-chart-label {
        width: 160px;
        font-size: 12px;
        text-align: right;
        color: var(--text-secondary);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        flex-shrink: 0;
    }

    .bar-chart-track {
        flex: 1;
        height: 22px;
        background: var(--bg-tertiary);
        border-radius: 4px;
        overflow: hidden;
        position: relative;
    }

    .bar-chart-bar {
        height: 100%;
        background: rgba(99, 102, 241, 0.35);
        border-radius: 4px;
        transition: width 0.3s ease;
        display: flex;
        overflow: hidden;
    }

    .bar-chart-bar__input {
        height: 100%;
        background: rgba(99, 102, 241, 0.8);
        border-radius: 4px 0 0 4px;
        transition: width 0.3s ease;
    }

    .bar-chart-value {
        width: 70px;
        font-size: 11px;
        font-family: var(--font-mono);
        color: var(--text-muted);
        text-align: right;
        flex-shrink: 0;
    }

    .bar-chart-cost {
        width: 80px;
        font-size: 11px;
        font-family: var(--font-mono);
        color: var(--text-muted);
        text-align: right;
        flex-shrink: 0;
    }

    .bar-chart-legend {
        display: flex;
        gap: 16px;
        margin-top: 4px;
        padding-left: 170px;
    }

    .bar-chart-legend__item {
        display: flex;
        align-items: center;
        gap: 6px;
        font-size: 11px;
        color: var(--text-muted);
    }

    .bar-chart-legend__swatch {
        width: 12px;
        height: 12px;
        border-radius: 3px;
    }

    .bar-chart-legend__swatch--input {
        background: rgba(99, 102, 241, 0.8);
    }

    .bar-chart-legend__swatch--output {
        background: rgba(99, 102, 241, 0.35);
    }

    /* ── Heatmap ── */
    .heatmap-grid {
        display: grid;
        grid-template-columns: repeat(24, 1fr);
        gap: 3px;
    }

    .heatmap-cell {
        aspect-ratio: 1;
        border-radius: 3px;
        min-height: 20px;
        transition: transform 0.1s;
    }

    .heatmap-cell:hover {
        transform: scale(1.2);
    }

    .heatmap-labels {
        display: flex;
        justify-content: space-between;
        font-size: 10px;
        color: var(--text-muted);
        margin-top: 4px;
        padding: 0 2px;
    }

    /* ── Daily Chart ── */
    .daily-chart-container {
        margin-top: 8px;
    }

    .daily-chart-bars {
        display: flex;
        align-items: flex-end;
        gap: 8px;
        height: 120px;
        padding: 0 4px;
    }

    .daily-chart-bar-wrapper {
        flex: 1;
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 4px;
        min-width: 0;
    }

    .daily-chart-bar {
        width: 100%;
        max-width: 40px;
        background: linear-gradient(to top, rgba(99, 102, 241, 0.7), rgba(99, 102, 241, 0.9));
        border-radius: 4px 4px 0 0;
        transition: height 0.3s ease, opacity 0.2s;
        cursor: pointer;
    }

    .daily-chart-bar:hover {
        opacity: 0.8;
    }

    .daily-chart-label {
        font-size: 9px;
        color: var(--text-muted);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        max-width: 50px;
    }

    .daily-chart-legend {
        display: flex;
        justify-content: center;
        margin-top: 8px;
        font-size: 11px;
        color: var(--text-muted);
    }

    /* ── Responsive ── */
    @media screen and (max-width: 768px) {
        .usage-cost-hero {
            flex-direction: column;
            gap: 16px;
        }

        .usage-cost-hero__breakdown {
            gap: 12px;
        }

        .usage-header {
            flex-direction: column;
            align-items: stretch;
        }

        .usage-header__actions {
            flex-direction: column;
        }

        .bar-chart-label {
            width: 100px;
            font-size: 11px;
        }

        .bar-chart-legend {
            padding-left: 110px;
        }

        .bar-chart-cost {
            display: none;
        }

        .usage-stat-grid {
            grid-template-columns: repeat(auto-fill, minmax(130px, 1fr));
        }
    }
"#;
