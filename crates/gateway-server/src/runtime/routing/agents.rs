use std::collections::HashMap;
use std::sync::OnceLock;

use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

static AGENTS_STORE: OnceLock<Mutex<HashMap<String, AgentConfig>>> = OnceLock::new();

fn agents_store() -> &'static Mutex<HashMap<String, AgentConfig>> {
    AGENTS_STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    pub created_at: u64,
    pub updated_at: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            name: "New Agent".to_owned(),
            description: None,
            model: None,
            system_prompt: None,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }
}

#[handler]
pub async fn agents_list_handler(res: &mut Response) {
    let store = agents_store().lock().await;
    let agents: Vec<_> = store.values().cloned().collect();
    res.render(Text::Json(
        json!({ "agents": agents, "count": agents.len() }).to_string(),
    ));
}

#[handler]
pub async fn agents_get_handler(req: &mut Request, res: &mut Response) {
    let agent_id = req.param::<String>("agent_id").unwrap_or_default();
    if agent_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing agent_id" }).to_string(),
        ));
        return;
    }

    let store = agents_store().lock().await;
    if let Some(agent) = store.get(&agent_id) {
        res.render(Text::Json(json!({ "agent": agent }).to_string()))
    } else {
        res.status_code(StatusCode::NOT_FOUND);
        res.render(Text::Json(
            json!({ "error": "agent not found" }).to_string(),
        ));
    }
}

#[handler]
pub async fn agents_create_handler(req: &mut Request, res: &mut Response) {
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

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let agent = AgentConfig {
        id: uuid::Uuid::now_v7().to_string(),
        name: body
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("New Agent")
            .to_owned(),
        description: body
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        model: body
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        system_prompt: body
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned()),
        enabled: body
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        created_at: now,
        updated_at: now,
    };

    let id = agent.id.clone();
    let mut store = agents_store().lock().await;
    store.insert(id.clone(), agent.clone());

    res.render(Text::Json(
        json!({ "agent": agent, "status": "created" }).to_string(),
    ));
}

#[handler]
pub async fn agents_update_handler(req: &mut Request, res: &mut Response) {
    let agent_id = req.param::<String>("agent_id").unwrap_or_default();
    if agent_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing agent_id" }).to_string(),
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

    let mut store = agents_store().lock().await;
    if let Some(agent) = store.get_mut(&agent_id) {
        if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
            agent.name = name.to_owned();
        }
        if let Some(desc) = body.get("description").and_then(|v| v.as_str()) {
            agent.description = Some(desc.to_owned());
        }
        if let Some(model) = body.get("model").and_then(|v| v.as_str()) {
            agent.model = Some(model.to_owned());
        }
        if let Some(prompt) = body.get("system_prompt").and_then(|v| v.as_str()) {
            agent.system_prompt = Some(prompt.to_owned());
        }
        if let Some(enabled) = body.get("enabled").and_then(|v| v.as_bool()) {
            agent.enabled = enabled;
        }
        agent.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        res.render(Text::Json(
            json!({ "agent": agent, "status": "updated" }).to_string(),
        ));
    } else {
        res.status_code(StatusCode::NOT_FOUND);
        res.render(Text::Json(
            json!({ "error": "agent not found" }).to_string(),
        ));
    }
}

#[handler]
pub async fn agents_delete_handler(req: &mut Request, res: &mut Response) {
    let agent_id = req.param::<String>("agent_id").unwrap_or_default();
    if agent_id.is_empty() {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Text::Json(
            json!({ "error": "missing agent_id" }).to_string(),
        ));
        return;
    }

    let mut store = agents_store().lock().await;
    if let Some(agent) = store.remove(&agent_id) {
        res.render(Text::Json(
            json!({ "agent": agent, "status": "deleted" }).to_string(),
        ))
    } else {
        res.status_code(StatusCode::NOT_FOUND);
        res.render(Text::Json(
            json!({ "error": "agent not found" }).to_string(),
        ));
    }
}
