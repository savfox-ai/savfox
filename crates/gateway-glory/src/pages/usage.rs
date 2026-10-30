use glory::prelude::*;
use serde_json::{Value, json};

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let state = query(ctx, "usage.status", None);
    let period = ctx.scope.cage("week".to_owned());
    let costs = ctx.scope.cage(Value::Null);
    let context = ctx.clone();
    let error = state.error;
    let revision = state.revision;
    ctx.scope.effect(move || {
        let connected = context.connected.get();
        let period = period.get();
        let _ = revision.get();
        if !connected {
            return;
        }
        let context_async = context.clone();
        context.spawn(async move {
            match context_async
                .rpc
                .call::<Value>("usage.cost", Some(json!({"period":period})))
                .await
            {
                Ok(value) => ui::set(costs, value),
                Err(message) => ui::set(error, message),
            }
        });
    });
    let value = state.value;
    let metric = |label: &'static str, key: &'static str| -> Box<dyn Widget> {
        Box::new(
            col().class("usage-stat-card").fill((
                text()
                    .class("usage-stat-label")
                    .content(ui::label_for(locale, label)),
                text()
                    .class("usage-stat-value")
                    .content(value.map(move |v| {
                        v[key]
                            .as_u64()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "—".into())
                    })),
            )),
        )
    };
    let mut children = vec![
        status(ctx, &state),
        Box::new(
            row().class("usage-cost-hero").fill((
                col().class("usage-cost-hero__main").fill((
                    text()
                        .class("usage-cost-hero__label")
                        .content(ui::label_for(locale, "Estimated total cost")),
                    text()
                        .class("usage-cost-hero__amount")
                        .content(ctx.scope.bond(move || {
                            let cost = value.get()["total_cost"].as_f64().unwrap_or_else(|| {
                                array(&costs.get(), &["entries"])
                                    .iter()
                                    .filter_map(|entry| entry["cost"].as_f64())
                                    .sum()
                            });
                            format!("${cost:.4}")
                        })),
                    text()
                        .class("usage-cost-hero__period")
                        .content(period.map(move |p| {
                            ui::translate(
                                locale.get(),
                                match p.as_str() {
                                    "today" => "Today",
                                    "week" => "7 days",
                                    "month" => "30 days",
                                    _ => "All time",
                                },
                            )
                        })),
                )),
                row().class("usage-cost-hero__breakdown").fill((
                    metric("Input tokens", "prompt_tokens"),
                    metric("Output tokens", "completion_tokens"),
                    col().class("usage-stat-card").fill((
                        text()
                            .class("usage-stat-label")
                            .content(ui::label_for(locale, "Reported cost")),
                        text().class("usage-stat-value").content(value.map(|v| {
                            v["total_cost"]
                                .as_f64()
                                .map(|v| format!("${v:.4}"))
                                .unwrap_or_else(|| "—".into())
                        })),
                    )),
                )),
            )),
        ),
        Box::new(row().class("usage-stat-grid").fill((
            metric("Total tokens", "total_tokens"),
            metric("Sessions", "session_count"),
            metric("Total messages", "total_messages"),
            metric("Tool calls", "tool_calls"),
            metric("Errors", "errors"),
            metric("Cache hits", "cache_hits"),
            metric("Cache misses", "cache_misses"),
        ))),
        card(
            ui::label_for(locale, "Hourly distribution"),
            vec![
                Box::new(
                    row().class("heatmap-grid").fill(
                        each(value.map(|v| {
                            let rows = v["hourly_distribution"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default();
                            let max = rows
                                .iter()
                                .filter_map(Value::as_u64)
                                .max()
                                .unwrap_or(0)
                                .max(1);
                            (0..24)
                                .map(|i| (i, rows.get(i).and_then(Value::as_u64).unwrap_or(0), max))
                                .collect::<Vec<_>>()
                        }))
                        .key(|(hour, count, _)| format!("{hour}-{count}"))
                        .children(move |each_row| {
                            let item = each_row.value;
                            let (_, count, max) = item.get_untracked();
                            text()
                                .class(format!(
                                    "heatmap-cell heatmap-cell--{}",
                                    if count == 0 { 0 } else { 1 + (count * 4 / max) }
                                ))
                                .content(count.to_string())
                        }),
                    ),
                ),
                Box::new(row().class("heatmap-labels").fill((
                    text().content("0:00"),
                    text().content("6:00"),
                    text().content("12:00"),
                    text().content("18:00"),
                    text().content("23:00"),
                ))),
            ],
        ),
    ];
    let breakdown = costs.map(|value| {
        let mut result = std::collections::BTreeMap::<String, (u64, u64, f64)>::new();
        for entry in array(value, &["entries"]) {
            let model = string(&entry, &["model"]);
            let row = result.entry(model).or_default();
            row.0 += entry["input_tokens"].as_u64().unwrap_or(0);
            row.1 += entry["output_tokens"].as_u64().unwrap_or(0);
            row.2 += entry["cost"].as_f64().unwrap_or(0.0);
        }
        result.into_iter().collect::<Vec<_>>()
    });
    children.push(card(
        ui::label_for(locale, "By model"),
        vec![Box::new(
            each(breakdown)
                .key(|(model, counts)| format!("{model}-{}-{}-{}", counts.0, counts.1, counts.2))
                .children(move |each_row| {
                    let item = each_row.value;
                    let (model, (input, output, cost)) = item.get_untracked();
                    paragraph(format!(
                        "{model} · {input} input / {output} output tokens · ${cost:.4}"
                    ))
                }),
        )],
    ));
    children.push(card(
        ui::label_for(locale, "Daily usage"),
        vec![Box::new(text().content(value.map(|value| {
            array(value, &["daily_distribution"])
                .iter()
                .map(|entry| {
                    format!(
                        "{} · {} tokens · ${:.4}",
                        string(entry, &["date"]),
                        entry["tokens"].as_u64().unwrap_or(0),
                        entry["cost"].as_f64().unwrap_or(0.0)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })))],
    ));
    let rows = Query {
        value: costs,
        ..state.clone()
    };
    children.push(ui::details(
        "usage-sessions",
        ui::label_for(locale, "Per-session cost tracking"),
        records(ctx, &rows, &["entries"], move |entry, _| {
            metadata_for(
                locale,
                &entry,
                &[
                    ("session_id", "Session"),
                    ("model", "Model"),
                    ("input_tokens", "Input tokens"),
                    ("output_tokens", "Output tokens"),
                    ("tokens", "Total tokens"),
                    ("cost", "Cost"),
                ],
            )
        }),
    ));
    let mut exports = Vec::new();
    exports.push(ui::action(
        ui::label_for(locale, "Export JSON"),
        move || {
            crate::utils::download::trigger_download(
                "savfox-usage.json",
                &serde_json::to_string_pretty(&costs.get()).unwrap_or_default(),
                "application/json",
            )
        },
    ));
    exports.push(ui::action(ui::label_for(locale, "Export CSV"), move || {
        let mut csv = "session_id,model,input_tokens,output_tokens,total_tokens,cost\n".to_owned();
        for entry in array(&costs.get(), &["entries"]) {
            csv.push_str(
                &[
                    "session_id",
                    "model",
                    "input_tokens",
                    "output_tokens",
                    "tokens",
                    "cost",
                ]
                .iter()
                .map(|key| {
                    csv_escape(&entry[*key].as_str().map(str::to_owned).unwrap_or_else(|| {
                        if entry[*key].is_null() {
                            String::new()
                        } else {
                            entry[*key].to_string()
                        }
                    }))
                })
                .collect::<Vec<_>>()
                .join(","),
            );
            csv.push('\n');
        }
        crate::utils::download::trigger_download("savfox-usage.csv", &csv, "text/csv");
    }));
    exports.push(refresh(ctx, &state));
    exports.insert(
        0,
        choice_for(
            locale,
            "Period",
            period,
            &[
                ("today", "Today"),
                ("week", "7 days"),
                ("month", "30 days"),
                ("all", "All time"),
            ],
        ),
    );
    Box::new(
        col()
            .class("gateway-page management-page usage-page")
            .fill((
                row().class("usage-header").fill((
                    text()
                        .class("page-title")
                        .kind(TextKind::Heading(1))
                        .content(ui::label_for(locale, "Usage Analytics")),
                    row().class("usage-header__actions").fill(exports),
                )),
                col().class("page-body").fill(children),
            )),
    )
}
fn csv_escape(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn csv_quotes_commas_newlines_and_quotes() {
        assert_eq!(csv_escape("a,\"b\"\nc"), "\"a,\"\"b\"\"\nc\"");
    }
}
