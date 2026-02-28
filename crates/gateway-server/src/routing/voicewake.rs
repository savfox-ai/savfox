use std::sync::{Arc, OnceLock};

use salvo::prelude::*;
use serde_json::json;
use tokio::sync::RwLock;

static VOICEWAKE_STATE: OnceLock<RwLock<VoiceWakeState>> = OnceLock::new();

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct VoiceWakeState {
    enabled: bool,
    keyword: String,
    sensitivity: f32,
    auto_reply: bool,
}

impl Default for VoiceWakeState {
    fn default() -> Self {
        Self {
            enabled: false,
            keyword: "hey assistant".to_string(),
            sensitivity: 0.5,
            auto_reply: true,
        }
    }
}

fn voicewake_state() -> &'static RwLock<VoiceWakeState> {
    VOICEWAKE_STATE.get_or_init(|| RwLock::new(VoiceWakeState::default()))
}

#[handler]
pub async fn voicewake_status_handler(res: &mut Response) {
    let state = voicewake_state().read().await.clone();
    res.render(Text::Json(json!({ "voicewake": state }).to_string()));
}

#[handler]
pub async fn voicewake_set_handler(req: &mut Request, res: &mut Response) {
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

    let mut state = voicewake_state().write().await;

    if let Some(enabled) = body.get("enabled").and_then(|v| v.as_bool()) {
        state.enabled = enabled;
    }
    if let Some(keyword) = body.get("keyword").and_then(|v| v.as_str()) {
        state.keyword = keyword.to_string();
    }
    if let Some(sensitivity) = body.get("sensitivity").and_then(|v| v.as_f64()) {
        state.sensitivity = sensitivity as f32;
    }
    if let Some(auto_reply) = body.get("auto_reply").and_then(|v| v.as_bool()) {
        state.auto_reply = auto_reply;
    }

    res.render(Text::Json(
        json!({
            "status": "ok",
            "voicewake": *state,
            "message": "voicewake settings updated",
        })
        .to_string(),
    ));
}

#[handler]
pub async fn voicewake_enable_handler(res: &mut Response) {
    let mut state = voicewake_state().write().await;
    state.enabled = true;
    res.render(Text::Json(
        json!({ "status": "ok", "enabled": true, "message": "voicewake enabled" }).to_string(),
    ));
}

#[handler]
pub async fn voicewake_disable_handler(res: &mut Response) {
    let mut state = voicewake_state().write().await;
    state.enabled = false;
    res.render(Text::Json(
        json!({ "status": "ok", "enabled": false, "message": "voicewake disabled" }).to_string(),
    ));
}
