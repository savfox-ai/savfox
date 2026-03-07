use dioxus::prelude::*;
use serde_json::json;

use crate::api::types::{
    AgentsResponse, CronJob, CronListResponse, CronRunEntry, CronRunsResponse, CronStatusResponse,
};
use crate::api::ws::WsRpc;
use crate::components::toast::Toaster;

#[derive(Clone, Copy, PartialEq)]
enum ScheduleType {
    Every,
    At,
    Cron,
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

#[component]
pub fn Cron() -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut toaster = use_context::<Toaster>();
    let mut refresh_tick = use_signal(|| 0u32);

    let mut selected_job = use_signal(|| Option::<String>::None);
    let mut show_create = use_signal(|| false);

    // Create form state
    let new_name = use_signal(String::new);
    let schedule_type = use_signal(|| ScheduleType::Every);
    let new_every_value = use_signal(|| "1".to_string());
    let new_every_unit = use_signal(|| "hours".to_string());
    let new_at_datetime = use_signal(String::new);
    let new_cron_expr = use_signal(String::new);
    let new_cron_tz = use_signal(|| "UTC".to_string());
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

    rsx! {
        div { style: "display:flex;height:100%;",
            // Left: job list
            div { style: "width:360px;min-width:360px;border-right:1px solid var(--border);display:flex;flex-direction:column;",
                div { style: "padding:12px 16px;border-bottom:1px solid var(--border);",
                    div { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;",
                        h2 { style: "font-size:16px;font-weight:600;", "Cron Jobs" }
                        div { style: "display:flex;gap:6px;",
                            button {
                                onclick: move |_| refresh_tick += 1,
                                style: "{TOOL_BTN}",
                                "Refresh"
                            }
                            button {
                                onclick: move |_| {
                                    show_create.set(true);
                                    selected_job.set(None);
                                },
                                style: "{TOOL_BTN}background:var(--accent);color:#fff;border:none;",
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
                        p { style: "padding:16px;color:var(--text-muted);font-size:14px;", "Loading..." }
                    } else if jobs.is_empty() {
                        p { style: "padding:16px;color:var(--text-muted);font-size:14px;", "No cron jobs" }
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
                                        onclick: move |_| {
                                            selected_job.set(Some(j.id.clone()));
                                            show_create.set(false);
                                        },
                                        style: "padding:10px 16px;border-bottom:1px solid var(--border);cursor:pointer;background:{bg};",
                                        div { style: "display:flex;justify-content:space-between;align-items:center;",
                                            span { style: "font-weight:500;font-size:14px;", "{job.name.as_deref().unwrap_or(&job.id)}" }
                                            span { style: "width:8px;height:8px;border-radius:50%;display:inline-block;background:{ec};" }
                                        }
                                        if let Some(ref sched) = job.schedule {
                                            div { style: "font-size:12px;color:var(--text-muted);margin-top:2px;font-family:monospace;", "{sched}" }
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
            div { style: "flex:1;display:flex;flex-direction:column;overflow:auto;",
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
                    label { style: "{LABEL}", "Name" }
                    input {
                        value: "{new_name}",
                        oninput: move |e| new_name.set(e.value()),
                        placeholder: "daily-cleanup",
                        style: "{INPUT}",
                    }
                }

                // Agent
                div {
                    label { style: "{LABEL}", "Agent" }
                    select {
                        value: "{new_agent_id}",
                        onchange: move |e| new_agent_id.set(e.value()),
                        style: "{INPUT}",
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
                    label { style: "{LABEL}", "Schedule" }
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

                    match schedule_type() {
                        ScheduleType::Every => rsx! {
                            div { style: "display:flex;gap:8px;",
                                input {
                                    r#type: "number",
                                    value: "{new_every_value}",
                                    oninput: move |e| new_every_value.set(e.value()),
                                    style: "{INPUT}width:100px;",
                                    min: "1",
                                }
                                select {
                                    value: "{new_every_unit}",
                                    onchange: move |e| new_every_unit.set(e.value()),
                                    style: "{INPUT}width:150px;",
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
                                style: "{INPUT}",
                            }
                        },
                        ScheduleType::Cron => rsx! {
                            div { style: "display:flex;gap:8px;",
                                input {
                                    value: "{new_cron_expr}",
                                    oninput: move |e| new_cron_expr.set(e.value()),
                                    placeholder: "0 0 * * *",
                                    style: "{INPUT}flex:1;font-family:monospace;",
                                }
                                input {
                                    value: "{new_cron_tz}",
                                    oninput: move |e| new_cron_tz.set(e.value()),
                                    placeholder: "UTC",
                                    style: "{INPUT}width:120px;",
                                }
                            }
                        },
                    }
                }

                div {
                    label { style: "{LABEL}", "Session Target" }
                    select {
                        value: "{new_session_target}",
                        onchange: move |e| new_session_target.set(e.value()),
                        style: "{INPUT}",
                        option { value: "main", "Main Session" }
                        option { value: "isolated", "Isolated Run" }
                    }
                    p { style: "margin-top:4px;color:var(--text-muted);font-size:12px;",
                        "Main keeps the same cron conversation context between runs. Isolated starts fresh each time."
                    }
                }

                // Payload type
                div {
                    label { style: "{LABEL}", "Payload Type" }
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
                                style: "{INPUT}",
                            }
                        } else {
                            textarea {
                                value: "{new_payload_text}",
                                oninput: move |e| new_payload_text.set(e.value()),
                                placeholder: "Agent turn message...",
                                rows: 4,
                                style: "{INPUT}resize:vertical;font-size:13px;",
                            }
                            div { style: "margin-top:8px;",
                                label { style: "{LABEL}", "Timeout (seconds)" }
                                input {
                                    r#type: "number",
                                    value: "{new_payload_timeout}",
                                    oninput: move |e| new_payload_timeout.set(e.value()),
                                    style: "{INPUT}width:120px;",
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
                                        new_cron_tz.set("UTC".to_string());
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
                        style: "{TOOL_BTN}background:var(--accent);color:#fff;border:none;padding:8px 20px;",
                        "Create"
                    }
                    button {
                        onclick: move |_| show_create.set(false),
                        style: "{TOOL_BTN}padding:8px 20px;",
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
                        style: "{TOOL_BTN}",
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
                        style: "{TOOL_BTN}background:var(--accent);color:#fff;border:none;",
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
                        style: "{TOOL_BTN}color:var(--danger);border-color:var(--danger);",
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
                    p { style: "color:var(--text-muted);font-size:14px;", "No runs yet" }
                } else {
                    div { style: "border:1px solid var(--border);border-radius:var(--radius);overflow:hidden;",
                        table { style: "width:100%;border-collapse:collapse;",
                            thead {
                                tr { style: "background:var(--bg-tertiary);",
                                    th { style: "{TH}", "Started" }
                                    th { style: "{TH}", "Status" }
                                    th { style: "{TH}", "Duration" }
                                }
                            }
                            tbody {
                                for (i, run) in runs.iter().enumerate() {
                                    {
                                        let status = run.status.as_deref().unwrap_or("-");
                                        let status_class = match status {
                                            "success" | "ok" => "chip chip--success",
                                            "error" | "failed" => "chip chip--danger",
                                            "running" => "chip chip--info",
                                            _ => "chip chip--muted",
                                        };
                                        let dur = run.duration_ms.map(|d| format!("{d}ms")).unwrap_or_else(|| "-".into());
                                        let started = run.started_at.as_deref().unwrap_or("-").to_string();
                                        rsx! {
                                            tr { key: "{i}", style: "border-top:1px solid var(--border);",
                                                td { style: "{TD}", "{started}" }
                                                td { style: "{TD}", span { class: "{status_class}", "{status}" } }
                                                td { style: "{TD}font-family:monospace;", "{dur}" }
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

const TH: &str = "text-align:left;padding:10px 14px;font-size:12px;font-weight:600;color:var(--text-secondary);text-transform:uppercase;letter-spacing:0.05em;";
const TD: &str = "padding:10px 14px;font-size:14px;";
const TOOL_BTN: &str = "padding:4px 12px;background:transparent;color:var(--text-secondary);border:1px solid var(--border);border-radius:var(--radius);font-size:12px;";
const INPUT: &str = "width:100%;padding:8px 12px;background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--radius);color:var(--text-primary);outline:none;font-size:14px;";
const LABEL: &str =
    "display:block;font-size:12px;font-weight:600;color:var(--text-secondary);margin-bottom:4px;";
