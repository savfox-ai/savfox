use glory::prelude::*;
use serde_json::{Value, json};

use super::common::*;
use crate::ui::{self, AppContext};

pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let gateway = query(ctx, "status", None);
    let channels = query(ctx, "channels.status", Some(json!({"probe":false})));
    let usage = query(ctx, "usage.status", None);
    let models = query(ctx, "models.list", None);
    let sessions = query(ctx, "sessions.list", None);
    let report = query(ctx, "security.audit", None);
    let cron = query(ctx, "cron.status", None);
    let connected = ctx.connected;
    let model_data = models.value;
    let session_data = sessions.value;
    let usage_data = usage.value;
    let gateway_data = gateway.value;
    let cron_data = cron.value;
    let connection = ctx.scope.bond(move || {
        ui::translate(
            locale.get(),
            if connected.get() {
                "Connected"
            } else {
                "Disconnected"
            },
        )
    });
    let hero = col().class("ov-hero").fill((
        col().class("ov-hero__copy").fill((
            text()
                .class("ov-hero__eyebrow")
                .content(ui::label_for(locale, "Foxfire Control Room")),
            text()
                .class("page-title ov-hero__title")
                .kind(TextKind::Heading(1))
                .content(
                    ctx.scope
                        .bond(move || crate::i18n::t(locale.get(), "nav.overview")),
                ),
            text().class("ov-hero__desc").content(ui::label_for(
                locale,
                "A sharper command deck for your agents, channels, and sessions. Savfox keeps the backstage hot, watchful, and ready to work.",
            )),
            row().class("ov-hero__chips").fill((
                text()
                    .class("ov-hero__chip ov-hero__chip--live")
                    .content(connection),
                text()
                    .class("ov-hero__chip")
                    .content(ctx.scope.bond(move || {
                        format!(
                            "{} {}",
                            array(&model_data.get(), &["models"]).len(),
                            ui::translate(locale.get(), "Models")
                        )
                    })),
                text()
                    .class("ov-hero__chip")
                    .content(ctx.scope.bond(move || {
                        format!(
                            "{} {}",
                            array(&session_data.get(), &["entries"]).len(),
                            ui::translate(locale.get(), "Sessions")
                        )
                    })),
            )),
        )),
        col().class("ov-hero__aside").fill((
            hero_stat(
                locale,
                "Build",
                ctx.scope.bond(move || {
                    gateway_data.get()["version"]
                        .as_str()
                        .unwrap_or(env!("CARGO_PKG_VERSION"))
                        .to_owned()
                }),
            ),
            hero_stat(
                locale,
                "Security",
                ctx.scope.bond(move || {
                    let value = report.value.get();
                    value
                        .pointer("/summary/critical")
                        .and_then(Value::as_u64)
                        .map(|n| {
                            if n == 0 {
                                ui::translate(locale.get(), "No critical issues")
                            } else {
                                format!("{n} {}", ui::translate(locale.get(), "Critical"))
                            }
                        })
                        .unwrap_or_else(|| "—".into())
                }),
            ),
            hero_stat(
                locale,
                "Next Cron",
                ctx.scope.bond(move || {
                    let value = cron_data.get();
                    value
                        .get("next_run")
                        .or_else(|| value.get("next_run_at"))
                        .and_then(Value::as_str)
                        .unwrap_or("—")
                        .to_owned()
                }),
            ),
        )),
    ));
    let stats = row().class("stats-grid").fill((
        stat(
            locale,
            "Version",
            gateway_data.map(|v| {
                v["version"]
                    .as_str()
                    .unwrap_or(env!("CARGO_PKG_VERSION"))
                    .to_owned()
            }),
        ),
        stat(
            locale,
            "Connected clients",
            gateway_data.map(|v| v["connected_clients"].as_u64().unwrap_or(0).to_string()),
        ),
        stat(
            locale,
            "Sessions",
            session_data.map(|v| array(v, &["entries"]).len().to_string()),
        ),
        stat(
            locale,
            "Models",
            model_data.map(|v| array(v, &["models"]).len().to_string()),
        ),
        stat(
            locale,
            "Total tokens",
            usage_data.map(|v| v["total_tokens"].as_u64().unwrap_or(0).to_string()),
        ),
        stat(
            locale,
            "Total cost",
            usage_data.map(|v| {
                v["total_cost"]
                    .as_f64()
                    .map(|cost| format!("${cost:.4}"))
                    .unwrap_or_else(|| "—".into())
            }),
        ),
    ));
    let security_summary = report.value;
    let connection_card = card(
        ui::label_for(locale, "Connection"),
        vec![Box::new(
            row().class("ov-connection-row").fill((
                text().class("status-ok").content(connection),
                text()
                    .class("ov-security-summary")
                    .content(ctx.scope.bond(move || {
                        let value = security_summary.get();
                        if value.is_null() {
                            String::new()
                        } else if value
                            .pointer("/summary/critical")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                            == 0
                        {
                            ui::translate(locale.get(), "No critical issues")
                        } else {
                            ui::translate(locale.get(), "Review security findings")
                        }
                    })),
            )),
        )],
    );
    let system_stats = row().class("ov-system-grid").fill((
        stat(
            locale,
            "Uptime",
            gateway_data.map(|v| {
                v["uptime_ms"]
                    .as_u64()
                    .map(|ms| format!("{}m", ms / 60000))
                    .unwrap_or_else(|| "—".into())
            }),
        ),
        stat(
            locale,
            "Tick interval",
            gateway_data.map(|v| {
                v["tick_interval_ms"]
                    .as_u64()
                    .map(|ms| format!("{ms}ms"))
                    .unwrap_or_else(|| "—".into())
            }),
        ),
        stat(
            locale,
            "Cron jobs",
            ctx.scope.bond(move || {
                ui::translate(
                    locale.get(),
                    if gateway_data.get()["cron_enabled"]
                        .as_bool()
                        .or_else(|| cron_data.get()["running"].as_bool())
                        .unwrap_or(false)
                    {
                        "Enabled"
                    } else {
                        "Disabled"
                    },
                )
            }),
        ),
    ));
    let channel_data = channels.value;
    let channel_rows = ctx.scope.bond(move || {
        let value = channel_data.get();
        value
            .get("instances")
            .or_else(|| value.get("channels"))
            .and_then(Value::as_object)
            .map(|items| {
                items
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    let channel_cards = each(channel_rows)
        .key(|(id, value)| format!("{id}-{}", record_key(value)))
        .children(move |each_row| {
            let entry = each_row.value;
            let (id, value) = entry.get_untracked();
            let label = string(&value, &["channel_name", "name", "platform"]);
            let name = if label.is_empty() { id } else { label };
            let healthy = value["connected"].as_bool().unwrap_or(false);
            row().class("ov-channel-card").fill((
                col().class("ov-channel-info").fill((
                    text().class("ov-channel-name").content(name),
                    text()
                        .class(if healthy {
                            "ov-channel-status status-ok"
                        } else {
                            "ov-channel-status"
                        })
                        .content(ui::label_for(
                            locale,
                            if healthy { "Connected" } else { "Disconnected" },
                        )),
                )),
                text()
                    .class(if healthy {
                        "status-dot status-dot--success"
                    } else {
                        "status-dot"
                    })
                    .content(""),
            ))
        });
    let listed_models = model_data.map(|value| {
        array(value, &["models"])
            .into_iter()
            .take(6)
            .collect::<Vec<_>>()
    });
    let model_rows = each(listed_models)
        .key(record_key)
        .children(move |each_row| {
            let model = each_row.value;
            let model = model.get_untracked();
            row().class("ov-model-row").fill((
                col().class("ov-model-name").fill((
                    text()
                        .class("ov-model-id")
                        .content(string(&model, &["name", "model_slug", "id"])),
                    text()
                        .class("ov-model-provider-badge")
                        .content(string(&model, &["provider_name", "provider"])),
                )),
                text()
                    .class("status-ok")
                    .content(ui::label_for(locale, "Available")),
            ))
        });
    let refresh_resources = [
        gateway.clone(),
        channels.clone(),
        usage.clone(),
        models.clone(),
        sessions.clone(),
        report.clone(),
        cron.clone(),
    ];
    let refresh_all = ui::action(ui::label_for(locale, "Refresh All"), move || {
        for resource in &refresh_resources {
            resource.refresh();
        }
    });
    let trio = row().class("ov-trio-grid").fill((
        card(
            ui::label_for(locale, "Channel Health"),
            vec![
                status(ctx, &channels),
                Box::new(col().class("ov-channel-grid").fill(channel_cards)),
                nav(ctx, "Manage channels", "/channels"),
            ],
        ),
        card(
            ui::label_for(locale, "Model Availability"),
            vec![
                status(ctx, &models),
                Box::new(col().class("ov-model-list").fill(model_rows)),
                nav(ctx, "Models", "/models"),
            ],
        ),
        card(
            ui::label_for(locale, "Security audit"),
            vec![
                status(ctx, &report),
                refresh(ctx, &report),
                records(ctx, &report, &["findings"], move |finding, _| {
                    metadata_for(
                        locale,
                        &finding,
                        &[
                            ("severity", "Severity"),
                            ("title", "Finding"),
                            ("detail", "Details"),
                        ],
                    )
                }),
            ],
        ),
    ));
    Box::new(
        col()
            .class("gateway-page management-page overview-page")
            .fill((
                hero,
                row().class("ov-actions").fill((
                    refresh_all,
                    nav(ctx, "View Logs", "/logs"),
                    nav(ctx, "Connect provider", "/models/connect-provider"),
                )),
                status(ctx, &gateway),
                connection_card,
                stats,
                system_stats,
                trio,
            )),
    )
}
fn hero_stat(
    locale: Cage<crate::i18n::Locale>,
    label: &str,
    value: impl Into<Vary<String>>,
) -> Box<dyn Widget> {
    Box::new(
        col().class("ov-hero__stat").fill((
            text()
                .class("ov-hero__stat-label")
                .content(ui::label_for(locale, label)),
            text().class("ov-hero__stat-value").content(value),
        )),
    )
}
fn stat(
    locale: Cage<crate::i18n::Locale>,
    label: &str,
    value: impl Into<Vary<String>>,
) -> Box<dyn Widget> {
    Box::new(
        col().class("stat-card").fill((
            text()
                .class("stat-label")
                .content(ui::label_for(locale, label)),
            text().class("stat-value").content(value),
        )),
    )
}
