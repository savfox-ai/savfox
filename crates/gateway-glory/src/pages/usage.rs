use glory::prelude::*;
use serde_json::{Value, json};

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
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
    let mut children = vec![
        status(ctx, &state),
        refresh(ctx, &state),
        choice(
            "Period",
            period,
            &[
                ("today", "Today"),
                ("week", "7 days"),
                ("month", "30 days"),
                ("all", "All time"),
            ],
        ),
        Box::new(text().content(value.map(|value| {
            [
                "total_tokens",
                "total_cost",
                "session_count",
                "total_messages",
                "tool_calls",
                "errors",
                "cache_hits",
                "cache_misses",
            ]
            .iter()
            .filter_map(|key| {
                value
                    .get(*key)
                    .map(|v| format!("{}: {v}", key.replace('_', " ")))
            })
            .collect::<Vec<_>>()
            .join("\n")
        }))),
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
        "By model",
        vec![Box::new(each(
            breakdown,
            |(model, counts)| format!("{model}-{}-{}-{}", counts.0, counts.1, counts.2),
            move |item, _| {
                let (model, (input, output, cost)) = item.get_untracked();
                paragraph(format!(
                    "{model} · {input} input / {output} output tokens · ${cost:.4}"
                ))
            },
        ))],
    ));
    children.push(card(
        "Daily usage",
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
    children.push(records(ctx, &rows, &["entries"], |entry, _| {
        metadata(
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
    }));
    children.push(ui::action("Export JSON", move || {
        crate::utils::download::trigger_download(
            "savfox-usage.json",
            &serde_json::to_string_pretty(&costs.get()).unwrap_or_default(),
            "application/json",
        )
    }));
    children.push(ui::action("Export CSV", move || {
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
    ui::localized_page(
        ctx,
        "nav.usage",
        "Recorded token usage and provider costs",
        children,
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
