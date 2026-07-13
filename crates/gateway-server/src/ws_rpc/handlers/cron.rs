use std::sync::Arc;

use savfox_core::cron::{CronDelivery, CronPayload, CronSchedule, CronSessionTarget};
use savfox_gateway_shared::{
    CronDelivery as CronDeliveryDto, CronJob as CronJobDto, CronPayload as CronPayloadDto,
    CronRunEntry as CronRunEntryDto, CronSessionTarget as CronSessionTargetDto, CronStatusResponse,
};
use serde_json::{Value, json};

use super::super::types::{INTERNAL_ERROR, INVALID_REQUEST, RpcResult};
use super::super::utils::{opt_u64, require_str};
use crate::channel::GatewayChannel;
use crate::cron_service::CronService;

pub(crate) fn cron_param_job_id(params: &Value) -> Option<&str> {
    params
        .get("id")
        .or_else(|| params.get("job_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn cron_format_timestamp(timestamp_ms: u64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms as i64)
        .map(|dt| dt.to_rfc3339())
}

fn cron_interval_label(interval_secs: u64) -> String {
    if interval_secs != 0 && interval_secs.is_multiple_of(86_400) {
        format!("every {}d", interval_secs / 86_400)
    } else if interval_secs != 0 && interval_secs.is_multiple_of(3_600) {
        format!("every {}h", interval_secs / 3_600)
    } else if interval_secs != 0 && interval_secs.is_multiple_of(60) {
        format!("every {}m", interval_secs / 60)
    } else {
        format!("every {interval_secs}s")
    }
}

fn cron_schedule_label(schedule: &CronSchedule) -> String {
    match schedule {
        CronSchedule::At { at_ms } => cron_format_timestamp(*at_ms)
            .map(|ts| format!("at {ts}"))
            .unwrap_or_else(|| format!("at {at_ms}")),
        CronSchedule::Every { interval_secs, .. } => cron_interval_label(*interval_secs),
        CronSchedule::Cron {
            expression,
            timezone,
        } => {
            let timezone = timezone
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if let Some(timezone) = timezone {
                format!("{expression} [{timezone}]")
            } else {
                expression.clone()
            }
        }
    }
}

fn cron_payload_dto(payload: &CronPayload) -> CronPayloadDto {
    match payload {
        CronPayload::SystemEvent { text } => CronPayloadDto::SystemEvent { text: text.clone() },
        CronPayload::AgentTurn {
            message,
            model,
            timeout_secs,
        } => CronPayloadDto::AgentTurn {
            message: message.clone(),
            model: model.clone(),
            timeout_secs: *timeout_secs,
        },
    }
}

fn cron_delivery_dto(delivery: &CronDelivery) -> CronDeliveryDto {
    CronDeliveryDto {
        mode: delivery.mode.clone(),
        channel: delivery.channel.clone(),
        recipient: delivery.recipient.clone(),
    }
}

fn cron_session_target_dto(session_target: &CronSessionTarget) -> CronSessionTargetDto {
    match session_target {
        CronSessionTarget::Main => CronSessionTargetDto::Main,
        CronSessionTarget::Isolated => CronSessionTargetDto::Isolated,
    }
}

pub(crate) fn cron_job_summary_value(job: &savfox_core::cron::CronJob) -> Value {
    json!(CronJobDto {
        id: job.id.clone(),
        name: Some(job.name.clone()),
        schedule: Some(cron_schedule_label(&job.schedule)),
        payload: Some(cron_payload_dto(&job.payload)),
        enabled: Some(job.state.enabled),
        next_run: job.state.next_run_at_ms.and_then(cron_format_timestamp),
        last_run: job.state.last_run_at_ms.and_then(cron_format_timestamp),
        agent_id: job.agent_id.clone(),
        session_target: Some(cron_session_target_dto(&job.session_target)),
        delivery: Some(cron_delivery_dto(&job.delivery)),
    })
}

pub(crate) fn cron_status_summary_value(status: &savfox_core::cron::CronServiceStatus) -> Value {
    json!(CronStatusResponse {
        running: Some(status.enabled),
        job_count: Some(status.total_jobs),
        enabled_jobs: Some(status.enabled_jobs),
        running_jobs: Some(status.running_jobs),
    })
}

pub(crate) fn cron_run_summary_value(run: &savfox_core::cron::CronRunEntry) -> Value {
    json!(CronRunEntryDto {
        job_id: run.job_id.clone(),
        started_at: cron_format_timestamp(run.started_at_ms),
        finished_at: cron_format_timestamp(run.finished_at_ms),
        status: Some(run.status.clone()),
        duration_ms: Some(run.finished_at_ms.saturating_sub(run.started_at_ms)),
        error: run.error.clone(),
        output: run.result_preview.clone(),
    })
}

fn parse_cron_field<T>(params: &Value, field: &str) -> Result<Option<T>, (i64, String)>
where
    T: serde::de::DeserializeOwned,
{
    params
        .get(field)
        .map(|value| {
            serde_json::from_value::<T>(value.clone())
                .map_err(|err| (INVALID_REQUEST, format!("invalid {field}: {err}")))
        })
        .transpose()
}

fn parse_optional_trimmed_string_field(
    params: &Value,
    field: &str,
) -> Result<Option<Option<String>>, (i64, String)> {
    match params.get(field) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(Some(None))
            } else {
                Ok(Some(Some(value.to_owned())))
            }
        }
        Some(_) => Err((
            INVALID_REQUEST,
            format!("invalid '{field}' parameter: expected string or null"),
        )),
    }
}

pub(crate) async fn handle_cron_list(cron_service: &Arc<CronService>) -> RpcResult {
    let jobs = cron_service.list_jobs().await;
    let jobs = jobs.iter().map(cron_job_summary_value).collect::<Vec<_>>();
    Ok(json!({ "jobs": jobs }))
}

pub(crate) async fn handle_cron_status(cron_service: &Arc<CronService>) -> RpcResult {
    let status = cron_service.status().await;
    Ok(cron_status_summary_value(&status))
}

pub(crate) async fn handle_cron_add(params: &Value, cron_service: &Arc<CronService>) -> RpcResult {
    let name = require_str(params, "name")?.to_owned();

    // Parse schedule.
    let schedule = if let Some(schedule) = parse_cron_field::<CronSchedule>(params, "schedule")? {
        schedule
    } else if let Some(expression) = params.get("expression").and_then(|v| v.as_str()) {
        CronSchedule::Cron {
            expression: expression.to_owned(),
            timezone: None,
        }
    } else {
        return Err((
            INVALID_REQUEST,
            "missing 'schedule' or 'expression' parameter".to_owned(),
        ));
    };

    // Parse payload.
    let payload = if let Some(payload) = parse_cron_field::<CronPayload>(params, "payload")? {
        payload
    } else if let Some(command) = params.get("command").and_then(|v| v.as_str()) {
        CronPayload::SystemEvent {
            text: command.to_owned(),
        }
    } else {
        return Err((
            INVALID_REQUEST,
            "missing 'payload' or 'command' parameter".to_owned(),
        ));
    };

    // Parse delivery.
    let delivery = parse_cron_field::<CronDelivery>(params, "delivery")?.unwrap_or_default();

    // Parse session target (main or isolated).
    let session_target =
        parse_cron_field::<CronSessionTarget>(params, "session_target")?.unwrap_or_default();
    let agent_id = parse_optional_trimmed_string_field(params, "agent_id")?.flatten();

    let id = cron_service
        .add_job(
            name.clone(),
            schedule,
            payload,
            delivery,
            session_target,
            agent_id,
        )
        .await;
    Ok(json!({ "id": id, "name": name, "status": "added" }))
}

pub(crate) async fn handle_cron_update(
    params: &Value,
    cron_service: &Arc<CronService>,
) -> RpcResult {
    let id = cron_param_job_id(params).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_owned()));
    }

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());
    let schedule = parse_cron_field::<CronSchedule>(params, "schedule")?;
    let payload = parse_cron_field::<CronPayload>(params, "payload")?;
    let delivery = parse_cron_field::<CronDelivery>(params, "delivery")?;
    let session_target = parse_cron_field::<CronSessionTarget>(params, "session_target")?;
    let agent_id = parse_optional_trimmed_string_field(params, "agent_id")?;
    let enabled = params.get("enabled").and_then(|v| v.as_bool());

    match cron_service
        .update_job(
            id,
            name,
            schedule,
            payload,
            delivery,
            session_target,
            agent_id,
            enabled,
        )
        .await
    {
        Ok(job) => Ok(json!({
            "id": id,
            "status": "updated",
            "job": cron_job_summary_value(&job),
        })),
        Err(err) => Err((INVALID_REQUEST, err)),
    }
}

pub(crate) async fn handle_cron_remove(
    params: &Value,
    cron_service: &Arc<CronService>,
) -> RpcResult {
    let id = cron_param_job_id(params).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_owned()));
    }
    let removed = cron_service.remove_job(id).await;
    if removed {
        Ok(json!({ "id": id, "status": "removed" }))
    } else {
        Err((INVALID_REQUEST, format!("job '{id}' not found")))
    }
}

pub(crate) async fn handle_cron_run(
    params: &Value,
    cron_service: &Arc<CronService>,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let id = cron_param_job_id(params).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_owned()));
    }
    match cron_service.run_job(id, channel).await {
        Ok(()) => Ok(json!({ "id": id, "status": "triggered" })),
        Err(err) => Err((INTERNAL_ERROR, err)),
    }
}

pub(crate) async fn handle_cron_runs(params: &Value, cron_service: &Arc<CronService>) -> RpcResult {
    let id = cron_param_job_id(params).unwrap_or("");
    if id.is_empty() {
        return Err((INVALID_REQUEST, "missing 'id' parameter".to_owned()));
    }
    let limit = opt_u64(params, "limit", 20) as usize;

    let runs = cron_service.get_runs(id, limit).await;
    let runs = runs.iter().map(cron_run_summary_value).collect::<Vec<_>>();
    Ok(json!({ "id": id, "runs": runs }))
}
