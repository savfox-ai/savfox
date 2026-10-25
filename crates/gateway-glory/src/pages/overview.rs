use glory::prelude::*;
use serde_json::json;

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let gateway = query(ctx, "status", None);
    let channels = query(ctx, "channels.status", Some(json!({"probe":false})));
    let usage = query(ctx, "usage.status", None);
    let models = query(ctx, "models.list", None);
    let agents = query(ctx, "agents.list", None);
    let sessions = query(ctx, "sessions.list", None);
    let report = query(ctx, "security.audit", None);
    let mut children = vec![
        status(ctx, &gateway),
        refresh(ctx, &gateway),
        card(
            "Gateway",
            vec![Box::new(text().content(gateway.value.map(|v| {
                [
                    "version",
                    "uptime_ms",
                    "connected_clients",
                    "sessions_count",
                    "cron_enabled",
                ]
                .iter()
                .filter_map(|key| {
                    v.get(*key)
                        .map(|value| format!("{}: {}", key.replace('_', " "), value))
                })
                .collect::<Vec<_>>()
                .join("  ·  ")
            })))],
        ),
    ];
    for (title, data, key, path) in [
        ("Agents", agents, "agents", "/agents"),
        ("Sessions", sessions, "entries", "/sessions"),
        ("Models", models, "models", "/models"),
    ] {
        children.push(card(
            title,
            vec![
                Box::new(text().content(data.value.map(move |v| {
                    format!("{} available", v[key].as_array().map(Vec::len).unwrap_or(0))
                }))),
                nav(ctx, "Open", path),
            ],
        ));
    }
    children.push(card(
        "Channels",
        vec![
            status(ctx, &channels),
            Box::new(text().content(channels.value.map(|v| {
                v.get("instances")
                    .or_else(|| v.get("channels"))
                    .and_then(|v| v.as_object())
                    .map(|items| {
                        items
                            .iter()
                            .map(|(id, item)| {
                                format!(
                                    "{}: {}",
                                    string(item, &["channel_name", "platform", "name"]),
                                    item["health_state"]
                                        .as_str()
                                        .or_else(|| item["status"].as_str())
                                        .unwrap_or(id)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_else(|| "No configured channels".to_owned())
            }))),
            nav(ctx, "Manage channels", "/channels"),
        ],
    ));
    children.push(card(
        "Usage",
        vec![
            Box::new(text().content(usage.value.map(|v| {
                [
                    "total_tokens",
                    "total_cost",
                    "session_count",
                    "prompt_tokens",
                    "completion_tokens",
                ]
                .iter()
                .filter_map(|key| {
                    v.get(*key)
                        .map(|value| format!("{}: {value}", key.replace('_', " ")))
                })
                .collect::<Vec<_>>()
                .join("  ·  ")
            }))),
            nav(ctx, "View usage", "/usage"),
        ],
    ));
    children.push(card(
        "Security audit",
        vec![
            refresh(ctx, &report),
            status(ctx, &report),
            records(ctx, &report, &["findings"], |finding, _| {
                metadata(
                    &finding,
                    &[
                        ("severity", "Severity"),
                        ("title", "Finding"),
                        ("detail", "Details"),
                        ("remediation", "Remediation"),
                    ],
                )
            }),
        ],
    ));
    ui::localized_page(
        ctx,
        "nav.overview",
        "Gateway health, active agents, connections and usage",
        children,
    )
}
