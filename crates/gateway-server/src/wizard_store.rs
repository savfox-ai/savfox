use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WizardStatus {
    Active,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WizardState {
    pub(crate) wizard_id: String,
    pub(crate) wizard_type: String,
    pub(crate) step: usize,
    pub(crate) total_steps: usize,
    pub(crate) status: WizardStatus,
    pub(crate) started_at_ms: u64,
    pub(crate) updated_at_ms: u64,
    #[serde(default)]
    pub(crate) notes: Vec<String>,
}

fn store_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("wizard-state.json")
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn steps_for_type(wizard_type: &str) -> Vec<&'static str> {
    match wizard_type {
        "setup" => vec!["check_config", "check_model", "send_test_message", "finish"],
        "doctor" => vec!["collect_env", "check_bridges", "check_tts", "finish"],
        _ => vec!["prepare", "execute", "finish"],
    }
}

async fn load_state(savfox_home: &Path) -> Result<Option<WizardState>, String> {
    let path = store_path(savfox_home);
    let data = tokio::fs::read_to_string(path).await.unwrap_or_default();
    if data.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str::<WizardState>(&data)
        .map(Some)
        .map_err(|err| format!("failed to parse wizard state: {err}"))
}

async fn save_state(savfox_home: &Path, state: &WizardState) -> Result<(), String> {
    let path = store_path(savfox_home);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("failed to create wizard state dir: {err}"))?;
    }
    let data = serde_json::to_string_pretty(state)
        .map_err(|err| format!("failed to serialize wizard state: {err}"))?;
    tokio::fs::write(path, data)
        .await
        .map_err(|err| format!("failed to write wizard state: {err}"))
}

pub(crate) async fn start(savfox_home: &Path, wizard_type: &str) -> Result<Value, String> {
    let steps = steps_for_type(wizard_type);
    let now = now_epoch_ms();
    let state = WizardState {
        wizard_id: uuid::Uuid::now_v7().to_string(),
        wizard_type: wizard_type.to_owned(),
        step: 0,
        total_steps: steps.len(),
        status: WizardStatus::Active,
        started_at_ms: now,
        updated_at_ms: now,
        notes: Vec::new(),
    };
    save_state(savfox_home, &state).await?;
    Ok(json!({
        "wizard_id": state.wizard_id,
        "type": state.wizard_type,
        "step": state.step,
        "step_name": steps[state.step],
        "total_steps": state.total_steps,
        "status": "started",
    }))
}

pub(crate) async fn next(
    savfox_home: &Path,
    wizard_id: &str,
    note: Option<&str>,
) -> Result<Value, String> {
    let mut state = load_state(savfox_home)
        .await?
        .ok_or_else(|| "wizard is not active".to_string())?;
    if state.wizard_id != wizard_id {
        return Err(format!("wizard_id mismatch: expected {}", state.wizard_id));
    }
    if state.status != WizardStatus::Active {
        return Err(format!("wizard is not active: {:?}", state.status));
    }
    if let Some(v) = note
        && !v.is_empty()
    {
        state.notes.push(v.to_owned());
    }
    state.updated_at_ms = now_epoch_ms();
    let steps = steps_for_type(&state.wizard_type);
    if state.step + 1 >= state.total_steps {
        state.step = state.total_steps.saturating_sub(1);
        state.status = WizardStatus::Completed;
    } else {
        state.step += 1;
    }
    save_state(savfox_home, &state).await?;
    Ok(json!({
        "wizard_id": state.wizard_id,
        "type": state.wizard_type,
        "step": state.step,
        "step_name": steps.get(state.step).copied().unwrap_or("finish"),
        "total_steps": state.total_steps,
        "status": match state.status {
            WizardStatus::Active => "next",
            WizardStatus::Completed => "completed",
            WizardStatus::Cancelled => "cancelled",
        },
    }))
}

pub(crate) async fn cancel(savfox_home: &Path, wizard_id: Option<&str>) -> Result<Value, String> {
    let mut state = load_state(savfox_home)
        .await?
        .ok_or_else(|| "wizard is not active".to_string())?;
    if let Some(id) = wizard_id
        && !id.is_empty()
        && id != state.wizard_id
    {
        return Err(format!("wizard_id mismatch: expected {}", state.wizard_id));
    }
    state.status = WizardStatus::Cancelled;
    state.updated_at_ms = now_epoch_ms();
    save_state(savfox_home, &state).await?;
    Ok(json!({
        "wizard_id": state.wizard_id,
        "status": "cancelled",
    }))
}

pub(crate) async fn status(savfox_home: &Path) -> Result<Value, String> {
    let Some(state) = load_state(savfox_home).await? else {
        return Ok(json!({ "active": false, "wizard_id": null }));
    };
    let steps = steps_for_type(&state.wizard_type);
    Ok(json!({
        "active": state.status == WizardStatus::Active,
        "wizard_id": state.wizard_id,
        "type": state.wizard_type,
        "step": state.step,
        "step_name": steps.get(state.step).copied().unwrap_or("finish"),
        "total_steps": state.total_steps,
        "status": match state.status {
            WizardStatus::Active => "active",
            WizardStatus::Completed => "completed",
            WizardStatus::Cancelled => "cancelled",
        },
        "updated_at_ms": state.updated_at_ms,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wizard_flow_roundtrip() {
        let home =
            std::env::temp_dir().join(format!("savfox-wizard-test-{}", uuid::Uuid::now_v7()));
        let started = start(&home, "setup").await.expect("start");
        let wizard_id = started
            .get("wizard_id")
            .and_then(|v| v.as_str())
            .expect("wizard_id")
            .to_string();
        let _ = next(&home, &wizard_id, Some("ok")).await.expect("next");
        let status_val = status(&home).await.expect("status");
        assert!(status_val.get("wizard_id").is_some());
        let cancelled = cancel(&home, Some(&wizard_id)).await.expect("cancel");
        assert_eq!(
            cancelled.get("status").and_then(|v| v.as_str()),
            Some("cancelled")
        );
        let _ = tokio::fs::remove_dir_all(home).await;
    }
}
