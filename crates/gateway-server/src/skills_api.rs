use std::sync::Arc;

use salvo::prelude::*;
use savfox_skill_registry::SkillRegistry;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct SkillsApiState {
    registry: Arc<SkillRegistry>,
}

impl SkillsApiState {
    #[must_use] 
    pub fn new(savfox_home: &std::path::Path) -> Self {
        Self {
            registry: Arc::new(SkillRegistry::new(savfox_home)),
        }
    }

    #[must_use] 
    pub fn registry(&self) -> &SkillRegistry {
        &self.registry
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillInstallRequest {
    pub name: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillUninstallRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillInstallResponse {
    pub success: bool,
    pub name: String,
    pub version: String,
    pub install_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[handler]
pub async fn skills_list_handler(depot: &mut Depot, res: &mut Response) {
    let state = if let Ok(s) = depot.obtain::<SkillsApiState>() { s.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Json(
            json!({"error": "skills api not initialized"}).to_string(),
        ));
        return;
    };

    match state.registry.list_available(false).await {
        Ok(packages) => {
            let items: Vec<serde_json::Value> = packages
                .iter()
                .map(|p| {
                    json!({
                        "name": p.manifest.name,
                        "version": p.manifest.version.to_string(),
                        "description": p.manifest.description,
                        "shortDescription": p.manifest.short_description,
                        "author": p.manifest.author,
                        "keywords": p.manifest.keywords,
                        "categories": p.manifest.categories,
                        "installed": p.installed,
                        "installedVersion": p.installed_version.as_ref().map(|v| v.to_string()),
                        "status": p.status_label(),
                    })
                })
                .collect();
            res.render(Text::Json(json!({ "skills": items }).to_string()));
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(json!({ "error": err.to_string() }).to_string()));
        }
    }
}

#[handler]
pub async fn skills_search_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let query = req.query::<String>("q").unwrap_or_default();

    if query.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "missing query parameter 'q'"}).to_string(),
        ));
        return;
    }

    let state = if let Ok(s) = depot.obtain::<SkillsApiState>() { s.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Json(
            json!({"error": "skills api not initialized"}).to_string(),
        ));
        return;
    };

    match state.registry.search(&query).await {
        Ok(packages) => {
            let items: Vec<serde_json::Value> = packages
                .iter()
                .map(|p| {
                    json!({
                        "name": p.manifest.name,
                        "version": p.manifest.version.to_string(),
                        "description": p.manifest.description,
                        "installed": p.installed,
                    })
                })
                .collect();
            res.render(Text::Json(
                json!({ "results": items, "query": query }).to_string(),
            ));
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(json!({ "error": err.to_string() }).to_string()));
        }
    }
}

#[handler]
pub async fn skills_installed_handler(depot: &mut Depot, res: &mut Response) {
    let state = if let Ok(s) = depot.obtain::<SkillsApiState>() { s.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Json(
            json!({"error": "skills api not initialized"}).to_string(),
        ));
        return;
    };

    match state.registry.list_installed().await {
        Ok(packages) => {
            let items: Vec<serde_json::Value> = packages
                .iter()
                .map(|p| {
                    json!({
                        "name": p.manifest.name,
                        "version": p.manifest.version.to_string(),
                        "description": p.manifest.description,
                        "installPath": p.install_path,
                    })
                })
                .collect();
            res.render(Text::Json(json!({ "installed": items }).to_string()));
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(json!({ "error": err.to_string() }).to_string()));
        }
    }
}

#[handler]
pub async fn skills_refresh_handler(depot: &mut Depot, res: &mut Response) {
    let state = if let Ok(s) = depot.obtain::<SkillsApiState>() { s.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Json(
            json!({"error": "skills api not initialized"}).to_string(),
        ));
        return;
    };

    match state.registry.sync_registry().await {
        Ok(()) => {
            res.render(Text::Json(
                json!({
                    "status": "refreshed",
                })
                .to_string(),
            ));
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(json!({ "error": err.to_string() }).to_string()));
        }
    }
}

#[handler]
pub async fn skills_install_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let body = match req.parse_json::<SkillInstallRequest>().await {
        Ok(b) => b,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": format!("invalid request: {err}")}).to_string(),
            ));
            return;
        }
    };

    if body.name.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "skill name is required"}).to_string(),
        ));
        return;
    }

    let state = if let Ok(s) = depot.obtain::<SkillsApiState>() { s.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Json(
            json!({"error": "skills api not initialized"}).to_string(),
        ));
        return;
    };

    let packages = match state.registry.list_available(false).await {
        Ok(p) => p,
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({"error": format!("failed to list packages: {err}")}).to_string(),
            ));
            return;
        }
    };

    let Some(package) = packages.iter().find(|p| p.manifest.name == body.name) else {
        res.status_code(StatusCode::NOT_FOUND);
        res.render(Text::Json(
            json!({"error": format!("skill '{}' not found", body.name)}).to_string(),
        ));
        return;
    };

    match state.registry.install(package, None).await {
        Ok(result) => {
            if result.success {
                res.render(Text::Json(
                    json!({
                        "success": true,
                        "name": result.name,
                        "version": result.version,
                        "installPath": result.install_path,
                    })
                    .to_string(),
                ));
            } else {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Text::Json(
                    json!({
                        "success": false,
                        "name": result.name,
                        "error": result.error,
                    })
                    .to_string(),
                ));
            }
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({"error": format!("installation failed: {err}")}).to_string(),
            ));
        }
    }
}

#[handler]
pub async fn skills_uninstall_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let body = match req.parse_json::<SkillUninstallRequest>().await {
        Ok(b) => b,
        Err(err) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Json(
                json!({"error": format!("invalid request: {err}")}).to_string(),
            ));
            return;
        }
    };

    if body.name.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({"error": "skill name is required"}).to_string(),
        ));
        return;
    }

    let state = if let Ok(s) = depot.obtain::<SkillsApiState>() { s.clone() } else {
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Json(
            json!({"error": "skills api not initialized"}).to_string(),
        ));
        return;
    };

    match state.registry.uninstall(&body.name).await {
        Ok(removed) => {
            if removed {
                res.render(Text::Json(
                    json!({
                        "success": true,
                        "name": body.name,
                        "message": "skill uninstalled",
                    })
                    .to_string(),
                ));
            } else {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Text::Json(
                    json!({
                        "success": false,
                        "name": body.name,
                        "error": "skill not installed",
                    })
                    .to_string(),
                ));
            }
        }
        Err(err) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Json(
                json!({"error": format!("uninstall failed: {err}")}).to_string(),
            ));
        }
    }
}
