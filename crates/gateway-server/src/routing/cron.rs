use std::sync::Arc;

use salvo::prelude::*;
use savfox_core::cron::{CronDelivery, CronPayload, CronSchedule, CronSessionTarget};
use serde_json::json;

use crate::channel::GatewayChannel;
use crate::cron_service::CronService;

fn parse_cron_field<T>(body: &serde_json::Value, field: &str) -> Result<Option<T>, String>
where
    T: serde::de::DeserializeOwned,
{
    body.get(field)
        .map(|value| {
            serde_json::from_value::<T>(value.clone())
                .map_err(|err| format!("invalid {field}: {err}"))
        })
        .transpose()
}

fn parse_optional_trimmed_string_field(
    body: &serde_json::Value,
    field: &str,
) -> Result<Option<Option<String>>, String> {
    match body.get(field) {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(Some(None)),
        Some(serde_json::Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(Some(None))
            } else {
                Ok(Some(Some(value.to_owned())))
            }
        }
        Some(_) => Err(format!("invalid '{field}': expected string or null")),
    }
}

#[handler]
pub async fn cron_list_handler(depot: &mut Depot, res: &mut Response) {
    let cron_service = if let Ok(service) = depot.obtain::<Arc<CronService>>() { service.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let jobs = cron_service.list_jobs().await;

    res.render(Text::Json(
        json!({
            "jobs": jobs,
            "count": jobs.len(),
        })
        .to_string(),
    ));
}

#[handler]
pub async fn cron_get_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let job_id = req.param::<String>("job_id").unwrap_or_default();
    if job_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(json!({ "error": "missing job_id" }).to_string()));
        return;
    }

    let cron_service = if let Ok(service) = depot.obtain::<Arc<CronService>>() { service.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    if let Some(job) = cron_service.get_job(&job_id).await { res.render(Text::Json(json!({ "job": job }).to_string())) } else {
        res.status_code(StatusCode::NOT_FOUND);
        res.render(Text::Json(json!({ "error": "job not found" }).to_string()));
    }
}

#[handler]
pub async fn cron_status_handler(depot: &mut Depot, res: &mut Response) {
    let cron_service = if let Ok(service) = depot.obtain::<Arc<CronService>>() { service.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let status = cron_service.status().await;

    res.render(Text::Json(json!(status).to_string()));
}

#[handler]
pub async fn cron_add_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let body = match req.parse_json::<serde_json::Value>().await {
        Ok(v) => v,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({ "error": format!("invalid JSON: {err}") }).to_string(),
            ));
            return;
        }
    };

    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");

    if name.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "name is required" }).to_string(),
        ));
        return;
    }

    let schedule = match parse_cron_field::<CronSchedule>(&body, "schedule") {
        Ok(Some(schedule)) => schedule,
        Ok(None) => {
            if let Some(schedule_str) = body
                .get("schedule")
                .and_then(|v| v.as_str())
                .or_else(|| body.get("expression").and_then(|v| v.as_str()))
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                CronSchedule::Cron {
                    expression: schedule_str.to_owned(),
                    timezone: None,
                }
            } else {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Text::Json(
                    json!({ "error": "schedule is required" }).to_string(),
                ));
                return;
            }
        }
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(json!({ "error": err }).to_string()));
            return;
        }
    };

    let payload = match parse_cron_field::<CronPayload>(&body, "payload") {
        Ok(Some(payload)) => payload,
        Ok(None) => {
            if let Some(command) = body
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                CronPayload::SystemEvent {
                    text: command.to_owned(),
                }
            } else {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Text::Json(
                    json!({ "error": "payload or command is required" }).to_string(),
                ));
                return;
            }
        }
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(json!({ "error": err }).to_string()));
            return;
        }
    };
    let delivery = match parse_cron_field::<CronDelivery>(&body, "delivery") {
        Ok(delivery) => delivery.unwrap_or_default(),
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(json!({ "error": err }).to_string()));
            return;
        }
    };
    let session_target = match parse_cron_field::<CronSessionTarget>(&body, "session_target") {
        Ok(session_target) => session_target.unwrap_or_default(),
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(json!({ "error": err }).to_string()));
            return;
        }
    };
    let agent_id = match parse_optional_trimmed_string_field(&body, "agent_id") {
        Ok(agent_id) => agent_id.flatten(),
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(json!({ "error": err }).to_string()));
            return;
        }
    };

    let cron_service = if let Ok(service) = depot.obtain::<Arc<CronService>>() { service.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let job_id = cron_service
        .add_job(
            name.to_owned(),
            schedule,
            payload,
            delivery,
            session_target,
            agent_id,
        )
        .await;

    res.render(Text::Json(
        json!({
            "status": "ok",
            "job_id": job_id,
            "name": name,
            "message": "cron job added",
        })
        .to_string(),
    ));
}

#[handler]
pub async fn cron_update_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let job_id = req.param::<String>("job_id").unwrap_or_default();
    if job_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(json!({ "error": "missing job_id" }).to_string()));
        return;
    }

    let body = req
        .parse_json::<serde_json::Value>()
        .await
        .unwrap_or(json!({}));

    let cron_service = if let Ok(service) = depot.obtain::<Arc<CronService>>() { service.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|s| s.to_owned());
    let enabled = body.get("enabled").and_then(|v| v.as_bool());

    let schedule = match parse_cron_field::<CronSchedule>(&body, "schedule") {
        Ok(schedule) => schedule.or_else(|| {
            body.get("schedule")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|schedule| CronSchedule::Cron {
                    expression: schedule.to_owned(),
                    timezone: None,
                })
        }),
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(json!({ "error": err }).to_string()));
            return;
        }
    };
    let payload = match parse_cron_field::<CronPayload>(&body, "payload") {
        Ok(payload) => payload.or_else(|| {
            body.get("command")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|command| CronPayload::SystemEvent {
                    text: command.to_owned(),
                })
        }),
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(json!({ "error": err }).to_string()));
            return;
        }
    };
    let delivery = match parse_cron_field::<CronDelivery>(&body, "delivery") {
        Ok(delivery) => delivery,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(json!({ "error": err }).to_string()));
            return;
        }
    };
    let session_target = match parse_cron_field::<CronSessionTarget>(&body, "session_target") {
        Ok(session_target) => session_target,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(json!({ "error": err }).to_string()));
            return;
        }
    };
    let agent_id = match parse_optional_trimmed_string_field(&body, "agent_id") {
        Ok(agent_id) => agent_id,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(json!({ "error": err }).to_string()));
            return;
        }
    };

    match cron_service
        .update_job(
            &job_id,
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
        Ok(job) => res.render(Text::Json(
            json!({ "status": "ok", "job": job }).to_string(),
        )),
        Err(e) => {
            res.status_code(StatusCode::NOT_FOUND);
            res.render(Text::Json(json!({ "error": e }).to_string()));
        }
    }
}

#[handler]
pub async fn cron_delete_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let job_id = req.param::<String>("job_id").unwrap_or_default();
    if job_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(json!({ "error": "missing job_id" }).to_string()));
        return;
    }

    let cron_service = if let Ok(service) = depot.obtain::<Arc<CronService>>() { service.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let removed = cron_service.remove_job(&job_id).await;

    res.render(Text::Json(
        json!({
            "status": if removed { "ok" } else { "not_found" },
            "job_id": job_id,
            "removed": removed,
        })
        .to_string(),
    ));
}

#[handler]
pub async fn cron_run_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let job_id = req.param::<String>("job_id").unwrap_or_default();
    if job_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(json!({ "error": "missing job_id" }).to_string()));
        return;
    }

    let cron_service = if let Ok(service) = depot.obtain::<Arc<CronService>>() { service.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };
    let channel = if let Ok(channel) = depot.obtain::<Arc<GatewayChannel>>() { channel.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    match cron_service.run_job(&job_id, &channel).await {
        Ok(()) => res.render(Text::Json(
            json!({
                "status": "ok",
                "job_id": job_id,
                "run_id": uuid::Uuid::now_v7().to_string(),
                "message": "cron job triggered",
            })
            .to_string(),
        )),
        Err(err) => {
            res.status_code(if err.contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            });
            res.render(Text::Json(json!({ "error": err }).to_string()));
        }
    }
}

#[handler]
pub async fn cron_runs_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let job_id = req.param::<String>("job_id").unwrap_or_default();
    let limit = req.query::<usize>("limit").unwrap_or(20);

    let cron_service = if let Ok(service) = depot.obtain::<Arc<CronService>>() { service.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let runs = cron_service.get_runs(&job_id, limit).await;

    res.render(Text::Json(
        json!({
            "job_id": job_id,
            "runs": runs,
            "count": runs.len(),
        })
        .to_string(),
    ));
}
