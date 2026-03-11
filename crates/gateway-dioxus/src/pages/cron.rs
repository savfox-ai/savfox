use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::{
    AgentsResponse, CronJob, CronListResponse, CronRunEntry, CronRunsResponse, CronStatusResponse,
};
use crate::api::ws::WsRpc;
use crate::components::empty_state::EmptyState;
use crate::components::skeleton::*;
use crate::components::toast::Toaster;
use crate::utils::deep_link::replace_url;

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

fn build_schedule_value(
    schedule_type: ScheduleType,
    every_value: &str,
    every_unit: &str,
    at_datetime: &str,
    cron_expr: &str,
    cron_tz: &str,
) -> Result<serde_json::Value, String> {
    match schedule_type {
        ScheduleType::Every => Ok(json!({
            "kind": "every",
            "interval_secs": every_interval_secs(every_value, every_unit)?,
        })),
        ScheduleType::At => {
            let at_datetime =
                trimmed_or_none(at_datetime).ok_or_else(|| "Run time is required".to_string())?;
            let at_ms =
                js_sys::Date::new(&wasm_bindgen::JsValue::from_str(&at_datetime)).get_time();
            if !at_ms.is_finite() {
                return Err("Invalid run time".to_string());
            }
            Ok(json!({
                "kind": "at",
                "at_ms": at_ms.max(0.0) as u64,
            }))
        }
        ScheduleType::Cron => {
            let expression = trimmed_or_none(cron_expr)
                .ok_or_else(|| "Cron expression is required".to_string())?;
            Ok(json!({
                "kind": "cron",
                "expression": expression,
                "timezone": trimmed_or_none(cron_tz),
            }))
        }
    }
}

fn build_payload_value(
    payload_type: &str,
    payload_text: &str,
    payload_timeout: &str,
) -> Result<serde_json::Value, String> {
    match payload_type {
        "system_event" => {
            let text = trimmed_or_none(payload_text)
                .ok_or_else(|| "Event message is required".to_string())?;
            Ok(json!({
                "type": "system_event",
                "text": text,
            }))
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
            Ok(json!({
                "type": "agent_turn",
                "message": message,
                "timeout_secs": timeout_secs,
            }))
        }
        _ => Err("Unsupported payload type".to_string()),
    }
}

enum CronDeepLink {
    None,
    New,
    Detail(String),
}

#[component]
pub fn Cron() -> Element {
    cron_inner(CronDeepLink::None)
}

#[component]
pub fn CronNew() -> Element {
    cron_inner(CronDeepLink::New)
}

#[component]
pub fn CronDetail(job_id: String) -> Element {
    cron_inner(CronDeepLink::Detail(job_id))
}

fn cron_inner(deep_link: CronDeepLink) -> Element {
    let is_routed = !matches!(deep_link, CronDeepLink::None);
    let nav = use_navigator();

    let initial_selected = match &deep_link {
        CronDeepLink::Detail(id) => Some(id.clone()),
        _ => Option::None,
    };
    let initial_create = matches!(&deep_link, CronDeepLink::New);
    let initial_show_detail = initial_selected.is_some() || initial_create;

    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut toaster = use_context::<Toaster>();
    let mut refresh_tick = use_signal(|| 0u32);

    let mut selected_job = use_signal(move || initial_selected);
    let mut show_create = use_signal(move || initial_create);
    let mut show_detail = use_signal(move || initial_show_detail);

    // Sync URL with current view state for deep linking
    use_effect(move || {
        let selected = selected_job();
        let creating = show_create();

        if let Some(ref id) = selected {
            replace_url(&format!("/cron/{id}"));
        } else if creating {
            replace_url("/cron/new");
        } else if is_routed {
            nav.replace(crate::route::Route::Cron {});
        } else {
            replace_url("/cron");
        }
    });

    // Create form state
    let new_name = use_signal(String::new);
    let schedule_type = use_signal(|| ScheduleType::Every);
    let new_every_value = use_signal(|| "1".to_string());
    let new_every_unit = use_signal(|| "hours".to_string());
    let new_at_datetime = use_signal(String::new);
    let new_cron_expr = use_signal(String::new);
    let new_cron_tz = use_signal(|| {
        js_sys::eval("Intl.DateTimeFormat().resolvedOptions().timeZone")
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "UTC".to_string())
    });
    let new_agent_id = use_signal(String::new);
    let new_session_target = use_signal(|| "main".to_string());
    let new_payload_type = use_signal(|| "system_event".to_string());
    let new_payload_text = use_signal(String::new);
    let new_payload_timeout = use_signal(|| "300".to_string());

    let ws_list = ws.clone();
    let jobs_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_list.clone();
        async move {
            ws.call::<CronListResponse>("cron.list", None)
                .await
                .map(|r| r.jobs)
                .unwrap_or_default()
        }
    });

    let ws_status = ws.clone();
    let cron_status = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_status.clone();
        async move {
            ws.call::<CronStatusResponse>("cron.status", None)
                .await
                .ok()
        }
    });

    let ws_agents = ws.clone();
    let agents_data = use_resource(move || {
        let _c = ws_connected();
        let ws = ws_agents.clone();
        async move {
            ws.call::<AgentsResponse>("agents.list", None)
                .await
                .map(|r| r.agents)
                .unwrap_or_default()
        }
    });

    let ws_runs = ws.clone();
    let sel = selected_job();
    let runs_data = use_resource(move || {
        let _c = ws_connected();
        let _t = refresh_tick();
        let ws = ws_runs.clone();
        let job_id = sel.clone();
        async move {
            if let Some(id) = job_id {
                ws.call::<CronRunsResponse>("cron.runs", Some(json!({ "id": id })))
                    .await
                    .map(|r| r.runs)
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        }
    });

    let jobs: Vec<CronJob> = jobs_data.read().as_ref().cloned().unwrap_or_default();
    let runs: Vec<CronRunEntry> = runs_data.read().as_ref().cloned().unwrap_or_default();
    let agents = agents_data.read().as_ref().cloned().unwrap_or_default();
    let is_loading = jobs_data.read().is_none();

    let status_read = cron_status.read();
    let running = status_read
        .as_ref()
        .and_then(|s| s.as_ref())
        .and_then(|s| s.running)
        .unwrap_or(false);
    let running_label = if running { "Running" } else { "Stopped" };
    let running_color = if running {
        "var(--success)"
    } else {
        "var(--text-muted)"
    };

    let selected_entry: Option<CronJob> =
        selected_job().and_then(|id| jobs.iter().find(|j| j.id == id).cloned());

    let detail_class = if show_detail() {
        "split-view--detail-active"
    } else {
        ""
    };

    rsx! {
        div { class: "split-view {detail_class}",
            // Left: job list
            div { class: "split-view__list",
                div { style: "padding:12px 16px;border-bottom:1px solid var(--border);",
                    div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;",
                        h2 { style: "font-size:16px;font-weight:600;", "Cron Jobs" }
                        div { style: "display:flex;gap:6px;",
                            button {
                                onclick: move |_| refresh_tick += 1,
                                class: "{TOOL_BTN}",
                                "Refresh"
                            }
                            button {
                                onclick: move |_| {
                                    show_create.set(true);
                                    show_detail.set(true);
                                    selected_job.set(None);
                                },
                                class: "{TOOL_BTN} tool-btn--primary",
                                "+ New"
                            }
                        }
                    }
                    div { style: "display:flex;align-items:center;gap:6px;font-size:12px;",
                        span { style: "width:8px;height:8px;border-radius:50%;display:inline-block;background:{running_color};" }
                        span { style: "color:var(--text-muted);", "{running_label}" }
                        span { style: "color:var(--text-muted);margin-left:8px;", "({jobs.len()} jobs)" }
                    }
                }

                div { style: "flex:1;overflow:auto;",
                    if is_loading {
                        div { style: "padding:16px;",
                            SkeletonLines { count: 3 }
                        }
                    } else if jobs.is_empty() {
                        EmptyState {
                            icon: "\u{23F1}".to_string(),
                            message: "No cron jobs configured".to_string(),
                        }
                    } else {
                        for job in jobs.iter() {
                            {
                                let is_sel = selected_job() == Some(job.id.clone());
                                let bg = if is_sel { "var(--bg-hover)" } else { "transparent" };
                                let enabled = job.enabled.unwrap_or(true);
                                let ec = if enabled { "var(--success)" } else { "var(--text-muted)" };
                                let j = job.clone();
                                rsx! {
                                    div {
                                        key: "{job.id}",
                                        role: "button",
                                        tabindex: "0",
                                        onclick: move |_| {
                                            selected_job.set(Some(j.id.clone()));
                                            show_create.set(false);
                                            show_detail.set(true);
                                        },
                                        style: "padding:10px 16px;border-bottom:1px solid var(--border);cursor:pointer;background:{bg};",
                                        div { style: "display:flex;justify-content:space-between;align-items:center;",
                                            span { style: "font-weight:500;font-size:14px;", "{job.name.as_deref().unwrap_or(&job.id)}" }
                                            span { style: "width:8px;height:8px;border-radius:50%;display:inline-block;background:{ec};" }
                                        }
                                        if let Some(ref sched) = job.schedule {
                                            div { style: "font-size:12px;color:var(--text-muted);margin-top:2px;font-family:var(--font-mono);", "{sched}" }
                                        }
                                        if let Some(ref next) = job.next_run {
                                            div { style: "font-size:11px;color:var(--text-muted);margin-top:1px;", "Next: {next}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Right: detail / create
            div { class: "split-view__detail",
                button {
                    class: "split-view__back",
                    onclick: move |_| show_detail.set(false),
                    "\u{2190} Back"
                }
                if show_create() {
                    { render_create_form(
                        ws.clone(),
                        refresh_tick,
                        show_create,
                        new_name,
                        schedule_type,
                        new_every_value,
                        new_every_unit,
                        new_at_datetime,
                        new_cron_expr,
                        new_cron_tz,
                        new_agent_id,
                        new_session_target,
                        new_payload_type,
                        new_payload_text,
                        new_payload_timeout,
                        toaster,
                        &agents,
                    ) }
                } else if let Some(ref entry) = selected_entry {
                    { render_job_detail(ws.clone(), refresh_tick, entry, selected_job, toaster, &runs) }
                } else {
                    div { style: "display:flex;align-items:center;justify-content:center;height:100%;color:var(--text-muted);font-size:14px;",
                        "Select a job or create a new one"
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_create_form(
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    mut show_create: Signal<bool>,
    mut new_name: Signal<String>,
    mut schedule_type: Signal<ScheduleType>,
    mut new_every_value: Signal<String>,
    mut new_every_unit: Signal<String>,
    mut new_at_datetime: Signal<String>,
    mut new_cron_expr: Signal<String>,
    mut new_cron_tz: Signal<String>,
    mut new_agent_id: Signal<String>,
    mut new_session_target: Signal<String>,
    mut new_payload_type: Signal<String>,
    mut new_payload_text: Signal<String>,
    mut new_payload_timeout: Signal<String>,
    mut toaster: Toaster,
    agents: &[crate::api::types::AgentEntry],
) -> Element {
    let sched_types = [
        (ScheduleType::Every, "Every"),
        (ScheduleType::At, "At"),
        (ScheduleType::Cron, "Cron"),
    ];

    rsx! {
        div { style: "padding:24px;max-width:700px;",
            h3 { style: "font-size:18px;margin-bottom:16px;", "New Cron Job" }
            div { style: "display:flex;flex-direction:column;gap:16px;",
                // Name
                div {
                    label { class: "{LABEL}", "Name" }
                    input {
                        value: "{new_name}",
                        oninput: move |e| new_name.set(e.value()),
                        placeholder: "daily-cleanup",
                        class: "{INPUT}",
                    }
                }

                // Agent
                div {
                    label { class: "{LABEL}", "Agent" }
                    select {
                        value: "{new_agent_id}",
                        onchange: move |e| new_agent_id.set(e.value()),
                        class: "{INPUT}",
                        option { value: "", "-- Optional agent --" }
                        for agent in agents.iter() {
                            option {
                                key: "{agent.id.as_deref().unwrap_or(&agent.name)}",
                                value: "{agent.id.as_deref().unwrap_or(&agent.name)}",
                                "{agent.name}"
                            }
                        }
                    }
                    p { style: "margin-top:4px;color:var(--text-muted);font-size:12px;",
                        "Associate this job with an agent for filtering and management."
                    }
                }

                // Schedule type selector
                div {
                    label { class: "{LABEL}", "Schedule" }
                    div { class: "schedule-selector",
                        for (st, label) in sched_types.iter() {
                            {
                                let st = *st;
                                let is_active = schedule_type() == st;
                                let class = if is_active { "schedule-selector__btn active" } else { "schedule-selector__btn" };
                                rsx! {
                                    button {
                                        key: "{label}",
                                        class: "{class}",
                                        onclick: move |_| schedule_type.set(st),
                                        "{label}"
                                    }
                                }
                            }
                        }
                    }

                    div { style: "min-height:120px;",
                        match schedule_type() {
                            ScheduleType::Every => rsx! {
                                div { style: "display:flex;gap:8px;",
                                    input {
                                        r#type: "number",
                                        value: "{new_every_value}",
                                        oninput: move |e| new_every_value.set(e.value()),
                                        class: "{INPUT}", style: "width:100px;",
                                        min: "1",
                                    }
                                    select {
                                        value: "{new_every_unit}",
                                        onchange: move |e| new_every_unit.set(e.value()),
                                        class: "{INPUT}", style: "width:150px;",
                                        option { value: "seconds", "seconds" }
                                        option { value: "minutes", "minutes" }
                                        option { value: "hours", "hours" }
                                        option { value: "days", "days" }
                                    }
                                }
                            },
                            ScheduleType::At => rsx! {
                                input {
                                    r#type: "datetime-local",
                                    value: "{new_at_datetime}",
                                    oninput: move |e| new_at_datetime.set(e.value()),
                                    class: "{INPUT}",
                                }
                            },
                            ScheduleType::Cron => rsx! {
                                div { style: "display:flex;gap:8px;",
                                    input {
                                        value: "{new_cron_expr}",
                                        oninput: move |e| new_cron_expr.set(e.value()),
                                        placeholder: "0 0 * * *",
                                        class: "{INPUT}", style: "flex:1;font-family:var(--font-mono);",
                                    }
                                    select {
                                        value: "{new_cron_tz}",
                                        onchange: move |e| new_cron_tz.set(e.value()),
                                        class: "{INPUT}", style: "width:200px;",
                                        option { value: "UTC", "UTC" }
                                        optgroup { label: "Americas",
                                            option { value: "America/New_York", "America/New_York" }
                                            option { value: "America/Chicago", "America/Chicago" }
                                            option { value: "America/Denver", "America/Denver" }
                                            option { value: "America/Los_Angeles", "America/Los_Angeles" }
                                        }
                                        optgroup { label: "Europe",
                                            option { value: "Europe/London", "Europe/London" }
                                            option { value: "Europe/Paris", "Europe/Paris" }
                                            option { value: "Europe/Berlin", "Europe/Berlin" }
                                        }
                                        optgroup { label: "Asia",
                                            option { value: "Asia/Tokyo", "Asia/Tokyo" }
                                            option { value: "Asia/Shanghai", "Asia/Shanghai" }
                                            option { value: "Asia/Kolkata", "Asia/Kolkata" }
                                            option { value: "Asia/Singapore", "Asia/Singapore" }
                                        }
                                        optgroup { label: "Oceania",
                                            option { value: "Australia/Sydney", "Australia/Sydney" }
                                            option { value: "Pacific/Auckland", "Pacific/Auckland" }
                                        }
                                    }
                                }
                                {
                                    let cron_val = new_cron_expr();
                                    let desc = describe_cron(&cron_val);
                                    if desc != cron_val.trim() && !cron_val.trim().is_empty() {
                                        rsx! {
                                            span { style: "font-size:12px;color:var(--text-muted);margin-top:4px;display:block;", "{desc}" }
                                        }
                                    } else {
                                        rsx! {}
                                    }
                                }
                            },
                        }
                    }
                }

                div {
                    label { class: "{LABEL}", "Session Target" }
                    select {
                        value: "{new_session_target}",
                        onchange: move |e| new_session_target.set(e.value()),
                        class: "{INPUT}",
                        option { value: "main", "Main Session" }
                        option { value: "isolated", "Isolated Run" }
                    }
                    p { style: "margin-top:4px;color:var(--text-muted);font-size:12px;",
                        "Main keeps the same cron conversation context between runs. Isolated starts fresh each time."
                    }
                }

                // Payload type
                div {
                    label { class: "{LABEL}", "Payload Type" }
                    div { class: "cfg-segmented", style: "max-width:300px;",
                        {
                            let types = [("system_event", "System Event"), ("agent_turn", "Agent Turn")];
                            rsx! {
                                for (pt, pt_label) in types.iter() {
                                    {
                                        let pt = pt.to_string();
                                        let is_active = new_payload_type() == pt;
                                        let class = if is_active { "cfg-segmented__btn active" } else { "cfg-segmented__btn" };
                                        rsx! {
                                            button {
                                                key: "{pt_label}",
                                                class: "{class}",
                                                onclick: move |_| new_payload_type.set(pt.clone()),
                                                "{pt_label}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { style: "margin-top:8px;",
                        if new_payload_type() == "system_event" {
                            input {
                                value: "{new_payload_text}",
                                oninput: move |e| new_payload_text.set(e.value()),
                                placeholder: "Event message text",
                                class: "{INPUT}",
                            }
                        } else {
                            textarea {
                                value: "{new_payload_text}",
                                oninput: move |e| new_payload_text.set(e.value()),
                                placeholder: "Agent turn message...",
                                rows: 4,
                                class: "{INPUT}", style: "resize:vertical;font-size:13px;",
                            }
                            div { style: "margin-top:8px;",
                                label { class: "{LABEL}", "Timeout (seconds)" }
                                input {
                                    r#type: "number",
                                    value: "{new_payload_timeout}",
                                    oninput: move |e| new_payload_timeout.set(e.value()),
                                    class: "{INPUT}", style: "width:120px;",
                                }
                            }
                        }
                    }
                }

                // Actions
                div { style: "display:flex;gap:8px;margin-top:8px;",
                    button {
                        onclick: move |_| {
                            let name = new_name().trim().to_string();
                            if name.is_empty() {
                                toaster.error("Cron job name is required");
                                return;
                            }

                            let schedule = match build_schedule_value(
                                schedule_type(),
                                &new_every_value(),
                                &new_every_unit(),
                                &new_at_datetime(),
                                &new_cron_expr(),
                                &new_cron_tz(),
                            ) {
                                Ok(schedule) => schedule,
                                Err(err) => {
                                    toaster.error(err);
                                    return;
                                }
                            };
                            let payload = match build_payload_value(
                                &new_payload_type(),
                                &new_payload_text(),
                                &new_payload_timeout(),
                            ) {
                                Ok(payload) => payload,
                                Err(err) => {
                                    toaster.error(err);
                                    return;
                                }
                            };
                            let session_target = match new_session_target().as_str() {
                                "main" | "isolated" => new_session_target(),
                                _ => {
                                    toaster.error("Invalid session target");
                                    return;
                                }
                            };
                            let ws = ws.clone();
                            let agent = trimmed_or_none(&new_agent_id());
                            let mut toaster = toaster;
                            spawn(async move {
                                let mut params = json!({
                                    "name": name,
                                    "schedule": schedule,
                                    "payload": payload,
                                    "session_target": session_target,
                                });
                                if let Some(agent) = agent {
                                    params["agent_id"] = json!(agent);
                                }

                                match ws.call::<serde_json::Value>("cron.add", Some(params)).await {
                                    Ok(_) => {
                                        toaster.success("Cron job created");
                                        show_create.set(false);
                                        new_name.set(String::new());
                                        schedule_type.set(ScheduleType::Every);
                                        new_every_value.set("1".to_string());
                                        new_every_unit.set("hours".to_string());
                                        new_at_datetime.set(String::new());
                                        new_cron_expr.set(String::new());
                                        new_cron_tz.set(
                                            js_sys::eval("Intl.DateTimeFormat().resolvedOptions().timeZone")
                                                .ok()
                                                .and_then(|v| v.as_string())
                                                .unwrap_or_else(|| "UTC".to_string())
                                        );
                                        new_agent_id.set(String::new());
                                        new_session_target.set("main".to_string());
                                        new_payload_type.set("system_event".to_string());
                                        new_payload_text.set(String::new());
                                        new_payload_timeout.set("300".to_string());
                                        refresh_tick += 1;
                                    }
                                    Err(err) => toaster.error(format!("Create failed: {err}")),
                                }
                            });
                        },
                        class: "{TOOL_BTN} tool-btn--primary tool-btn--lg",
                        "Create"
                    }
                    button {
                        onclick: move |_| show_create.set(false),
                        class: "{TOOL_BTN} tool-btn--lg",
                        "Cancel"
                    }
                }
            }
        }
    }
}

fn render_job_detail(
    ws: WsRpc,
    mut refresh_tick: Signal<u32>,
    job: &CronJob,
    mut selected_job: Signal<Option<String>>,
    mut toaster: Toaster,
    runs: &[CronRunEntry],
) -> Element {
    let id_run = job.id.clone();
    let id_del = job.id.clone();
    let id_toggle = job.id.clone();
    let ws_run = ws.clone();
    let ws_del = ws.clone();
    let ws_toggle = ws;
    let enabled = job.enabled.unwrap_or(true);

    let enabled_class = if enabled {
        "chip chip--success"
    } else {
        "chip chip--muted"
    };
    let enabled_label = if enabled { "Enabled" } else { "Disabled" };

    // Parse schedule for display
    let schedule_display = job.schedule.as_deref().unwrap_or("-").to_string();
    let payload_type = job
        .payload
        .as_ref()
        .and_then(|payload| payload.get("type"))
        .and_then(|value| value.as_str())
        .unwrap_or("-");

    rsx! {
        div { style: "display:flex;flex-direction:column;height:100%;",
            // Header
            div { style: "padding:12px 16px;border-bottom:1px solid var(--border);display:flex;justify-content:space-between;align-items:center;",
                div { style: "display:flex;align-items:center;gap:8px;",
                    span { style: "font-weight:600;font-size:16px;", "{job.name.as_deref().unwrap_or(&job.id)}" }
                    span { class: "{enabled_class}", "{enabled_label}" }
                }
                div { style: "display:flex;gap:6px;",
                    button {
                        onclick: move |_| {
                            let id = id_toggle.clone();
                            let ws = ws_toggle.clone();
                            let next = !enabled;
                            let mut toaster = toaster;
                            spawn(async move {
                                match ws.call::<serde_json::Value>(
                                    "cron.update",
                                    Some(json!({ "id": id, "enabled": next })),
                                ).await {
                                    Ok(_) => {
                                        toaster.success(if next {
                                            "Cron job enabled"
                                        } else {
                                            "Cron job disabled"
                                        });
                                        refresh_tick += 1;
                                    }
                                    Err(err) => toaster.error(format!("Update failed: {err}")),
                                }
                            });
                        },
                        class: "{TOOL_BTN}",
                        if enabled { "Disable" } else { "Enable" }
                    }
                    button {
                        onclick: move |_| {
                            let id = id_run.clone();
                            let ws = ws_run.clone();
                            let mut toaster = toaster;
                            spawn(async move {
                                match ws.call::<serde_json::Value>("cron.run", Some(json!({ "id": id }))).await {
                                    Ok(_) => {
                                        toaster.success("Cron job triggered");
                                        refresh_tick += 1;
                                    }
                                    Err(err) => toaster.error(format!("Run failed: {err}")),
                                }
                            });
                        },
                        class: "{TOOL_BTN} tool-btn--primary",
                        "Run Now"
                    }
                    button {
                        onclick: move |_| {
                            let id = id_del.clone();
                            let ws = ws_del.clone();
                            let mut toaster = toaster;
                            spawn(async move {
                                match ws.call::<serde_json::Value>("cron.remove", Some(json!({ "id": id }))).await {
                                    Ok(_) => {
                                        toaster.success("Cron job deleted");
                                        selected_job.set(None);
                                        refresh_tick += 1;
                                    }
                                    Err(err) => toaster.error(format!("Delete failed: {err}")),
                                }
                            });
                        },
                        class: "{TOOL_BTN} tool-btn--danger",
                        "Delete"
                    }
                }
            }

            // Job info
            div { style: "padding:16px;border-bottom:1px solid var(--border);display:flex;flex-wrap:wrap;gap:16px;",
                div {
                    span { style: "font-size:12px;color:var(--text-muted);display:block;", "Schedule" }
                    code { style: "font-size:13px;", "{schedule_display}" }
                }
                div {
                    span { style: "font-size:12px;color:var(--text-muted);display:block;", "Payload" }
                    span { style: "font-size:13px;", "{payload_type}" }
                }
                if let Some(ref agent_id) = job.agent_id {
                    div {
                        span { style: "font-size:12px;color:var(--text-muted);display:block;", "Agent" }
                        span { style: "font-size:13px;", "{agent_id}" }
                    }
                }
                if let Some(ref next) = job.next_run {
                    div {
                        span { style: "font-size:12px;color:var(--text-muted);display:block;", "Next Run" }
                        span { style: "font-size:13px;", "{next}" }
                    }
                }
                if let Some(ref last) = job.last_run {
                    div {
                        span { style: "font-size:12px;color:var(--text-muted);display:block;", "Last Run" }
                        span { style: "font-size:13px;", "{last}" }
                    }
                }
            }

            // Run history
            div { style: "flex:1;padding:16px;overflow:auto;",
                h4 { style: "font-size:14px;font-weight:600;color:var(--text-secondary);margin-bottom:12px;text-transform:uppercase;letter-spacing:0.05em;", "Run History" }
                if runs.is_empty() {
                    div { style: "display:flex;flex-direction:column;align-items:center;justify-content:center;padding:40px 16px;color:var(--text-muted);",
                        div { style: "font-size:32px;margin-bottom:8px;opacity:0.5;", "---" }
                        p { style: "font-size:14px;margin:0;", "No run history" }
                        p { style: "font-size:12px;margin:4px 0 0;", "Runs will appear here once this job has been executed." }
                    }
                } else {
                    div { style: "border:1px solid var(--border);border-radius:var(--radius);overflow:hidden;",
                        table { style: "width:100%;border-collapse:collapse;",
                            thead {
                                tr { style: "background:var(--bg-tertiary);",
                                    th { class: "{TH}", "Started" }
                                    th { class: "{TH}", "Status" }
                                    th { class: "{TH}", "Duration" }
                                    th { class: "{TH}", "Output" }
                                }
                            }
                            tbody {
                                for (i, run) in runs.iter().enumerate() {
                                    {
                                        let status = run.status.as_deref().unwrap_or("-");
                                        let status_color = match status {
                                            "success" | "ok" => "var(--success, #22c55e)",
                                            "error" | "failed" => "var(--danger, #ef4444)",
                                            "running" => "var(--accent, #3b82f6)",
                                            _ => "var(--text-muted)",
                                        };
                                        let status_class = match status {
                                            "success" | "ok" => "chip chip--success",
                                            "error" | "failed" => "chip chip--danger",
                                            "running" => "chip chip--info",
                                            _ => "chip chip--muted",
                                        };
                                        let dur = format_duration(run.duration_ms);
                                        let started = run.started_at.as_deref().unwrap_or("-").to_string();
                                        let output_text = run.error.as_deref()
                                            .or(run.output.as_deref())
                                            .unwrap_or("")
                                            .to_string();
                                        let has_output = !output_text.is_empty();
                                        let truncated: String = if output_text.chars().count() > 80 {
                                            let t: String = output_text.chars().take(80).collect();
                                            format!("{t}...")
                                        } else {
                                            output_text.clone()
                                        };
                                        let is_truncated = output_text.chars().count() > 80;
                                        rsx! {
                                            tr { key: "{i}", style: "border-top:1px solid var(--border);",
                                                td { class: "{TD}", "{started}" }
                                                td { class: "{TD}",
                                                    div { style: "display:flex;align-items:center;gap:6px;",
                                                        span { style: "width:8px;height:8px;border-radius:50%;display:inline-block;background:{status_color};flex-shrink:0;" }
                                                        span { class: "{status_class}", "{status}" }
                                                    }
                                                }
                                                td { class: "{TD}", style: "font-family:var(--font-mono);", "{dur}" }
                                                td { class: "{TD}", style: "max-width:300px;",
                                                    if has_output {
                                                        if is_truncated {
                                                            details { style: "cursor:pointer;",
                                                                summary { style: "font-size:12px;color:var(--text-muted);font-family:var(--font-mono);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:280px;",
                                                                    "{truncated}"
                                                                }
                                                                pre { style: "font-size:12px;color:var(--text-secondary);font-family:var(--font-mono);white-space:pre-wrap;word-break:break-all;margin:4px 0 0;padding:8px;background:var(--bg-tertiary);border-radius:var(--radius);max-height:200px;overflow:auto;",
                                                                    "{output_text}"
                                                                }
                                                            }
                                                        } else {
                                                            span { style: "font-size:12px;color:var(--text-muted);font-family:var(--font-mono);", "{output_text}" }
                                                        }
                                                    } else {
                                                        span { style: "color:var(--text-muted);font-size:12px;", "-" }
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
        }
    }
}

fn format_duration(ms: Option<u64>) -> String {
    match ms {
        None => "-".to_string(),
        Some(d) if d < 1000 => format!("{d}ms"),
        Some(d) if d < 60_000 => format!("{:.1}s", d as f64 / 1000.0),
        Some(d) if d < 3_600_000 => {
            let mins = d / 60_000;
            let secs = (d % 60_000) / 1000;
            format!("{mins}m {secs}s")
        }
        Some(d) => {
            let hrs = d / 3_600_000;
            let mins = (d % 3_600_000) / 60_000;
            format!("{hrs}h {mins}m")
        }
    }
}

const TH: &str = "th-cell--cron";
const TD: &str = "td-cell";
const TOOL_BTN: &str = "tool-btn";
const INPUT: &str = "form-input";
const LABEL: &str = "form-label";
