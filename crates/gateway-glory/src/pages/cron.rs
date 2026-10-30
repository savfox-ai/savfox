#![allow(dead_code)]
use std::result::Result;

use glory::prelude::*;
use serde_json::{Value, json};

use super::common::*;
use crate::api::types::{CronPayload, CronSchedule, CronSessionTarget};
use crate::ui::{self, AppContext};
#[derive(Clone, Copy, PartialEq)]
enum ScheduleType {
    Every,
    At,
    Cron,
}

fn describe_cron(expr: &str) -> String {
    let expr = expr.trim();
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return expr.to_string();
    }
    let (min, hour, dom, mon, dow) = (parts[0], parts[1], parts[2], parts[3], parts[4]);

    // Every minute
    if min == "*" && hour == "*" && dom == "*" && mon == "*" && dow == "*" {
        return "Every minute".to_string();
    }
    // Every N minutes: */N * * * *
    if min.starts_with("*/") && hour == "*" && dom == "*" && mon == "*" && dow == "*" {
        let n = &min[2..];
        return format!("Every {n} minutes");
    }
    // Every N hours: 0 */N * * *
    if min == "0" && hour.starts_with("*/") && dom == "*" && mon == "*" && dow == "*" {
        let n = &hour[2..];
        return format!("Every {n} hours");
    }
    // Every hour: 0 * * * *
    if min == "0" && hour == "*" && dom == "*" && mon == "*" && dow == "*" {
        return "Every hour".to_string();
    }
    // Every day at midnight: 0 0 * * *
    if min == "0" && hour == "0" && dom == "*" && mon == "*" && dow == "*" {
        return "Every day at midnight".to_string();
    }
    // Every Sunday at midnight: 0 0 * * 0
    if min == "0" && hour == "0" && dom == "*" && mon == "*" && dow == "0" {
        return "Every Sunday at midnight".to_string();
    }

    expr.to_string()
}

fn trimmed_or_none(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn every_interval_secs(value: &str, unit: &str) -> Result<u64, String> {
    let quantity = value
        .trim()
        .parse::<u64>()
        .map_err(|_| "Interval must be a positive integer".to_string())?;
    if quantity == 0 {
        return Err("Interval must be greater than 0".to_string());
    }

    let multiplier = match unit {
        "seconds" => 1,
        "minutes" => 60,
        "hours" => 3_600,
        "days" => 86_400,
        _ => return Err("Unsupported interval unit".to_string()),
    };

    quantity
        .checked_mul(multiplier)
        .ok_or_else(|| "Interval is too large".to_string())
}

fn build_schedule(
    schedule_type: ScheduleType,
    every_value: &str,
    every_unit: &str,
    at_datetime: &str,
    cron_expr: &str,
    cron_tz: &str,
) -> Result<CronSchedule, String> {
    match schedule_type {
        ScheduleType::Every => Ok(CronSchedule::Every {
            interval_secs: every_interval_secs(every_value, every_unit)?,
            anchor_ms: 0,
        }),
        ScheduleType::At => {
            let at_datetime =
                trimmed_or_none(at_datetime).ok_or_else(|| "Run time is required".to_string())?;
            let at_ms = chrono::DateTime::parse_from_rfc3339(&at_datetime)
                .map(|date| date.timestamp_millis() as f64)
                .unwrap_or(f64::NAN);
            if !at_ms.is_finite() {
                return Err("Invalid run time".to_string());
            }
            Ok(CronSchedule::At {
                at_ms: at_ms.max(0.0) as u64,
            })
        }
        ScheduleType::Cron => {
            let expression = trimmed_or_none(cron_expr)
                .ok_or_else(|| "Cron expression is required".to_string())?;
            Ok(CronSchedule::Cron {
                expression,
                timezone: trimmed_or_none(cron_tz),
            })
        }
    }
}

fn build_payload(
    payload_type: &str,
    payload_text: &str,
    payload_timeout: &str,
) -> Result<CronPayload, String> {
    match payload_type {
        "system_event" => {
            let text = trimmed_or_none(payload_text)
                .ok_or_else(|| "Event message is required".to_string())?;
            Ok(CronPayload::SystemEvent { text })
        }
        "agent_turn" => {
            let message = trimmed_or_none(payload_text)
                .ok_or_else(|| "Agent message is required".to_string())?;
            let timeout_secs = match trimmed_or_none(payload_timeout) {
                Some(timeout) => {
                    let timeout_secs = timeout
                        .parse::<u64>()
                        .map_err(|_| "Timeout must be a positive integer".to_string())?;
                    if timeout_secs == 0 {
                        return Err("Timeout must be greater than 0".to_string());
                    }
                    Some(timeout_secs)
                }
                None => None,
            };
            Ok(CronPayload::AgentTurn {
                message,
                model: None,
                timeout_secs,
            })
        }
        _ => Err("Unsupported payload type".to_string()),
    }
}

pub fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    let locale = ctx.locale;
    if path == "/cron/new" {
        return create(ctx);
    }
    let state = query(ctx, "cron.list", None);
    let clock = query(ctx, "cron.status", None);
    let row_state = state.clone();
    let selected = path.strip_prefix("/cron/").map(str::to_owned);
    let runs = selected
        .as_ref()
        .map(|id| query(ctx, "cron.runs", Some(json!({"id":id}))));
    let selected_for_list = selected.clone();
    let has_selection = selected.is_some();
    let list_context = ctx.clone();
    let job_rows = state.value.map(|v| array(v, &["jobs"]));
    let refresh_jobs = state.clone();
    let create_context = ctx.clone();
    let list = col().class("split-view__list agent-list-pane").fill((
        row().class("panel-header").fill((
            text()
                .class("panel-header__title page-title")
                .kind(TextKind::Heading(1))
                .content(ui::label_for(locale, "Cron Jobs")),
            row().class("toolbar-actions").fill((
                sapid::button()
                    .unstyled()
                    .class("btn btn-secondary cron-toolbar-button")
                    .name(ui::label_for(locale, "Refresh"))
                    .enabled(ctx.connected)
                    .fill(
                        text()
                            .class("shell-icon shell-icon--refresh-cw")
                            .content(""),
                    )
                    .on_press(move || refresh_jobs.refresh()),
                sapid::button()
                    .unstyled()
                    .class("btn btn-secondary cron-toolbar-button")
                    .name(ui::label_for(locale, "Create scheduled job"))
                    .fill(text().class("shell-icon shell-icon--plus").content(""))
                    .on_press(move || create_context.navigate("/cron/new")),
            )),
        )),
        text()
            .class("cron-scheduler-status")
            .content(ctx.scope.bond(move || {
                ui::translate(
                    locale.get(),
                    if clock.value.get()["running"].as_bool().unwrap_or(false) {
                        "Running"
                    } else {
                        "Stopped"
                    },
                )
            })),
        status(ctx, &state),
        col()
            .class("agent-list-scroll")
            .fill(each(job_rows).key(record_key).children(move |each_row| {
                let item = each_row.value;
                let job = item.get_untracked();
                let id = string(&job, &["id"]);
                let target = id.clone();
                let context = list_context.clone();
                col()
                    .class(if selected_for_list.as_deref() == Some(id.as_str()) {
                        "list-item agent-list-item list-item--active"
                    } else {
                        "list-item agent-list-item"
                    })
                    .fill((
                        sapid::button()
                            .unstyled()
                            .class("btn-plain agent-select")
                            .label(string(&job, &["name", "id"]))
                            .on_press(move || context.navigate(&format!("/cron/{target}"))),
                        text()
                            .class("list-item__subtitle")
                            .content(string(&job, &["schedule"])),
                        text().class("list-item__subtitle").content(format!(
                            "{}: {}",
                            ui::translate(locale.get(), "Next run"),
                            string(&job, &["next_run"])
                        )),
                    ))
            })),
    ));
    let mut children = Vec::new();
    children.push(records(ctx, &state, &["jobs"], move |job, context| {
        let id = string(&job, &["id"]);
        if selected.as_ref().is_some_and(|selected| *selected != id) {
            return Box::new(group()) as Box<dyn Widget>;
        }
        let enabled = job["enabled"].as_bool().unwrap_or(true);
        let mut actions = vec![nav(&context, "Run history", format!("/cron/{id}"))];
        for (label, method, params) in [
            (
                if enabled { "Disable" } else { "Enable" },
                "cron.update",
                json!({"id":id,"enabled":!enabled}),
            ),
            ("Run now", "cron.run", json!({"id":id})),
            ("Remove", "cron.remove", json!({"id":id})),
        ] {
            let context = context.clone();
            let state = row_state.clone();
            actions.push(ui::action(ui::label_for(locale, label), move || {
                if method == "cron.remove" && !confirmed("Remove this scheduled job?") {
                    return;
                }
                mutate(&context, &state, method, params.clone());
            }));
        }
        card(
            string(&job, &["name", "id"]),
            vec![
                metadata_for(
                    locale,
                    &job,
                    &[
                        ("schedule", "Schedule"),
                        ("agent_id", "Agent"),
                        ("session_target", "Session"),
                        ("enabled", "Enabled"),
                        ("last_run", "Last run"),
                        ("next_run", "Next run"),
                        ("payload", "Payload"),
                    ],
                ),
                Box::new(row().class("actions").fill(actions)),
            ],
        )
    }));
    if let Some(runs) = runs {
        children.push(card(
            ui::label_for(locale, "Run history"),
            vec![
                status(ctx, &runs),
                refresh(ctx, &runs),
                records(ctx, &runs, &["runs"], move |run, _| {
                    metadata_for(
                        locale,
                        &run,
                        &[
                            ("started_at", "Started"),
                            ("status", "Status"),
                            ("duration_ms", "Duration (ms)"),
                            ("output", "Output"),
                            ("error", "Error"),
                        ],
                    )
                }),
            ],
        ));
    }
    let detail: Box<dyn Widget> = if has_selection {
        Box::new(col().class("cron-detail-content").fill(children))
    } else {
        Box::new(
            col().class("agent-empty-detail").fill(
                text()
                    .class("empty-state__message")
                    .content(ui::label_for(locale, "Select a job or create a new one")),
            ),
        )
    };
    Box::new(row().class(if has_selection {"gateway-page management-page agent-workspace cron-workspace split-view split-view--detail"}else{"gateway-page management-page agent-workspace cron-workspace split-view"}).fill((list,col().class("split-view__detail").fill(detail))))
}
fn create(ctx: &AppContext) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let name = ctx.scope.cage(String::new());
    let schedule = ctx.scope.cage("every".to_owned());
    let interval = ctx.scope.cage("1".to_owned());
    let unit = ctx.scope.cage("hours".to_owned());
    let at = ctx.scope.cage(String::new());
    let expression = ctx.scope.cage("0 * * * *".to_owned());
    let timezone = ctx.scope.cage("UTC".to_owned());
    let agent = ctx.scope.cage(String::new());
    let target = ctx.scope.cage("main".to_owned());
    let payload_type = ctx.scope.cage("system_event".to_owned());
    let text = ctx.scope.cage(String::new());
    let timeout = ctx.scope.cage("300".to_owned());
    let state = query(ctx, "agents.list", None);
    let context = ctx.clone();
    let save_state = state.clone();
    ui::page(ui::label_for(locale, "New scheduled job"), ui::label_for(locale, "Intervals, RFC3339 timestamps and cron expressions are supported"),vec![nav(ctx,"All scheduled jobs","/cron"),status(ctx,&state),ui::field(ui::label_for(locale, "Job name"),name,false),choice_for(locale, "Schedule",schedule,&[("every","Interval"),("at","Once"),("cron","Cron expression")]),ui::field(ui::label_for(locale, "Interval"),interval,false),choice_for(locale, "Interval unit",unit,&[("seconds","Seconds"),("minutes","Minutes"),("hours","Hours"),("days","Days")]),ui::field(ui::label_for(locale, "Run at (RFC3339 with timezone)"),at,false),ui::field(ui::label_for(locale, "Cron expression"),expression,false),ui::field(ui::label_for(locale, "Timezone"),timezone,false),ui::field(ui::label_for(locale, "Agent ID (optional)"),agent,false),choice_for(locale, "Session target",target,&[("main","Main"),("isolated","Isolated")]),choice_for(locale, "Payload",payload_type,&[("system_event","System event"),("agent_turn","Agent turn")]),ui::field(ui::label_for(locale, "Message"),text,true),ui::field(ui::label_for(locale, "Timeout seconds"),timeout,false),Box::new(sapid::button().label(ui::label_for(locale, "Create job")).loading(state.busy).on_press(move||{
        if name.get().trim().is_empty(){ui::set(save_state.error,"Job name is required".to_owned());return;}
        let schedule_type=match schedule.get().as_str(){"at"=>ScheduleType::At,"cron"=>ScheduleType::Cron,_=>ScheduleType::Every};
        let schedule=match build_schedule(schedule_type,&interval.get(),&unit.get(),&at.get(),&expression.get(),&timezone.get()){Ok(value)=>value,Err(error)=>{ui::set(save_state.error,error);return;}};
        let payload=match build_payload(&payload_type.get(),&text.get(),&timeout.get()){Ok(value)=>value,Err(error)=>{ui::set(save_state.error,error);return;}};
        let mut params=json!({"name":name.get(),"schedule":schedule,"payload":payload,"session_target":if target.get()=="isolated"{CronSessionTarget::Isolated}else{CronSessionTarget::Main}});if !agent.get().trim().is_empty(){params["agent_id"]=json!(agent.get());}
        let context_async=context.clone();let state=save_state.clone();ui::set(state.busy,true);ui::set(state.error,String::new());context.spawn(async move{match context_async.rpc.call::<Value>("cron.add",Some(params)).await{Ok(_)=>context_async.navigate("/cron"),Err(error)=>ui::set(state.error,error)}ui::set(state.busy,false);});
    }))])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interval_rejects_zero_and_overflow() {
        assert!(every_interval_secs("0", "hours").is_err());
        assert!(every_interval_secs(&u64::MAX.to_string(), "days").is_err());
        assert_eq!(every_interval_secs("2", "hours"), Ok(7200));
    }
    #[test]
    fn one_time_requires_explicit_timezone() {
        assert!(build_schedule(ScheduleType::At, "", "", "2026-10-25T12:00", "", "").is_err());
        assert!(matches!(
            build_schedule(
                ScheduleType::At,
                "",
                "",
                "2026-10-25T12:00:00+08:00",
                "",
                ""
            ),
            Ok(CronSchedule::At { .. })
        ));
    }
    #[test]
    fn empty_messages_and_zero_timeouts_are_rejected() {
        assert!(build_payload("agent_turn", "", "300").is_err());
        assert!(build_payload("agent_turn", "hello", "0").is_err());
        assert!(build_payload("system_event", "hello", "").is_ok());
    }
}
