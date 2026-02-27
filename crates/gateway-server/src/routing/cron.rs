use salvo::prelude::*;
use serde_json::json;
use std::sync::Arc;

use crate::cron_service::{CronDelivery, CronPayload, CronSchedule, CronService};

#[handler]
pub async fn cron_list_handler(depot: &mut Depot, res: &mut Response) {
    let cron_service = match depot.obtain::<Arc<CronService>>() {
        Ok(service) => service.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
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

    let cron_service = match depot.obtain::<Arc<CronService>>() {
        Ok(service) => service.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    match cron_service.get_job(&job_id).await {
        Some(job) => res.render(Text::Json(json!({ "job": job }).to_string())),
        None => {
            res.status_code(StatusCode::NOT_FOUND);
            res.render(Text::Json(json!({ "error": "job not found" }).to_string()));
        }
    }
}

#[handler]
pub async fn cron_status_handler(depot: &mut Depot, res: &mut Response) {
    let cron_service = match depot.obtain::<Arc<CronService>>() {
        Ok(service) => service.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
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

    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let schedule_str = body.get("schedule").and_then(|v| v.as_str()).unwrap_or("");
    let command = body.get("command").and_then(|v| v.as_str()).unwrap_or("");

    if name.is_empty() || schedule_str.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "name and schedule are required" }).to_string(),
        ));
        return;
    }

    let schedule = CronSchedule::Cron {
        expression: schedule_str.to_string(),
        timezone: None,
    };
    let payload = CronPayload::SystemEvent {
        text: command.to_string(),
    };
    let delivery = CronDelivery::default();

    let cron_service = match depot.obtain::<Arc<CronService>>() {
        Ok(service) => service.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let job_id = cron_service
        .add_job(
            name.to_string(),
            schedule,
            payload,
            delivery,
            Default::default(),
        )
        .await;

    res.render(Text::Json(
        json!({
            "status": "ok",
            "job_id": job_id,
            "name": name,
            "schedule": schedule_str,
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

    let cron_service = match depot.obtain::<Arc<CronService>>() {
        Ok(service) => service.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let enabled = body.get("enabled").and_then(|v| v.as_bool());

    let schedule = body
        .get("schedule")
        .and_then(|v| v.as_str())
        .map(|s| CronSchedule::Cron {
            expression: s.to_string(),
            timezone: None,
        });

    let command = body
        .get("command")
        .and_then(|v| v.as_str())
        .map(|s| CronPayload::SystemEvent {
            text: s.to_string(),
        });

    match cron_service
        .update_job(&job_id, name, schedule, command, None, enabled)
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

    let cron_service = match depot.obtain::<Arc<CronService>>() {
        Ok(service) => service.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
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

    res.render(Text::Json(
        json!({
            "status": "ok",
            "job_id": job_id,
            "run_id": uuid::Uuid::now_v7().to_string(),
            "message": "cron job triggered (placeholder - requires bridge)",
        })
        .to_string(),
    ));
}

#[handler]
pub async fn cron_runs_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let job_id = req.param::<String>("job_id").unwrap_or_default();
    let limit = req.query::<usize>("limit").unwrap_or(20);

    let cron_service = match depot.obtain::<Arc<CronService>>() {
        Ok(service) => service.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
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
