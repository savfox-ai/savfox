use salvo::prelude::*;
use serde_json::json;
use std::sync::Arc;

use crate::session::GatewaySessionManager;
use crate::session::SessionStore;
use savfox_protocol::SessionId;

#[handler]
pub async fn sessions_patch_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let session_id = req.param::<String>("session_id").unwrap_or_default();
    if session_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing session_id" }).to_string(),
        ));
        return;
    }

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

    let session_store = match depot.obtain::<Arc<SessionStore>>() {
        Ok(store) => store.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    if let Some(label) = body.get("label").and_then(|v| v.as_str()) {
        session_store
            .set_label(&session_id, label.to_string())
            .await;
    }
    if let Some(pinned) = body.get("pinned").and_then(|v| v.as_bool()) {
        session_store.set_pinned(&session_id, pinned).await;
    }
    if let Some(archived) = body.get("archived").and_then(|v| v.as_bool()) {
        session_store.set_archived(&session_id, archived).await;
    }

    res.render(Text::Json(
        json!({
            "status": "ok",
            "session_id": session_id,
            "message": "session patched",
        })
        .to_string(),
    ));
}

#[handler]
pub async fn sessions_reset_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let session_id = req.param::<SessionId>("session_id").unwrap_or_default();
    if session_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing session_id" }).to_string(),
        ));
        return;
    }

    let session_mgr = match depot.obtain::<Arc<GatewaySessionManager>>() {
        Ok(mgr) => mgr.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    session_mgr.remove_session(&session_id).await;

    res.render(Text::Json(
        json!({
            "status": "ok",
            "session_id": session_id,
            "message": "session reset",
        })
        .to_string(),
    ));
}

#[handler]
pub async fn sessions_delete_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let session_id = req.param::<SessionId>("session_id").unwrap_or_default();
    if session_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing session_id" }).to_string(),
        ));
        return;
    }

    let session_mgr = match depot.obtain::<Arc<GatewaySessionManager>>() {
        Ok(mgr) => mgr.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let session_store = match depot.obtain::<Arc<SessionStore>>() {
        Ok(store) => store.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    session_mgr.remove_session(&session_id).await;
    session_store.delete(&session_id).await;

    res.render(Text::Json(
        json!({
            "status": "ok",
            "session_id": session_id,
            "message": "session deleted",
        })
        .to_string(),
    ));
}

#[handler]
pub async fn sessions_compact_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let session_id = req.param::<String>("session_id").unwrap_or_default();
    if session_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing session_id" }).to_string(),
        ));
        return;
    }

    let _body = req
        .parse_json::<serde_json::Value>()
        .await
        .unwrap_or(json!({}));

    res.render(Text::Json(
        json!({
            "status": "ok",
            "session_id": session_id,
            "message": "session compacted (placeholder)",
        })
        .to_string(),
    ));
}

#[handler]
pub async fn sessions_preview_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let limit = req.query::<usize>("limit").unwrap_or(10);

    let session_store = match depot.obtain::<Arc<SessionStore>>() {
        Ok(store) => store.clone(),
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let sessions = session_store.list_recent(limit).await;

    res.render(Text::Json(
        json!({
            "sessions": sessions,
            "count": sessions.len(),
        })
        .to_string(),
    ));
}
