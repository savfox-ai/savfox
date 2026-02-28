use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PairingStatus {
    Pending,
    Approved,
    Rejected,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PairingRecord {
    pub(crate) request_id: String,
    pub(crate) node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) device_name: Option<String>,
    pub(crate) verification_code: String,
    pub(crate) status: PairingStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token: Option<String>,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
}

fn savfox_home() -> PathBuf {
    std::env::var("SAVFOX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".savfox")
        })
}

fn store_path(home: &PathBuf) -> PathBuf {
    home.join("gateway").join("node-pairings.json")
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn make_verification_code() -> String {
    uuid::Uuid::now_v7()
        .simple()
        .to_string()
        .chars()
        .take(6)
        .collect::<String>()
        .to_uppercase()
}

async fn load_records_for_home(home: &PathBuf) -> Result<Vec<PairingRecord>, String> {
    let path = store_path(home);
    let data = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    if data.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<Vec<PairingRecord>>(&data)
        .map_err(|err| format!("failed to parse pairing store: {err}"))
}

async fn save_records_for_home(home: &PathBuf, records: &[PairingRecord]) -> Result<(), String> {
    let path = store_path(home);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("failed to create pairing store dir: {err}"))?;
    }
    let data = serde_json::to_string_pretty(records)
        .map_err(|err| format!("failed to serialize pairing store: {err}"))?;
    tokio::fs::write(&path, data)
        .await
        .map_err(|err| format!("failed to write pairing store: {err}"))
}

pub(crate) async fn create_request(
    node_id: &str,
    device_id: Option<&str>,
    device_name: Option<&str>,
) -> Result<PairingRecord, String> {
    let home = savfox_home();
    create_request_for_home(&home, node_id, device_id, device_name).await
}

pub(crate) async fn create_request_for_home(
    home: &PathBuf,
    node_id: &str,
    device_id: Option<&str>,
    device_name: Option<&str>,
) -> Result<PairingRecord, String> {
    let now = now_epoch_ms();
    let record = PairingRecord {
        request_id: uuid::Uuid::now_v7().to_string(),
        node_id: node_id.to_owned(),
        device_id: device_id.map(ToOwned::to_owned),
        device_name: device_name.map(ToOwned::to_owned),
        verification_code: make_verification_code(),
        status: PairingStatus::Pending,
        token: None,
        created_at: now,
        updated_at: now,
    };
    let mut records = load_records_for_home(home).await?;
    records.push(record.clone());
    save_records_for_home(home, &records).await?;
    Ok(record)
}

pub(crate) async fn list_requests() -> Result<Vec<PairingRecord>, String> {
    let home = savfox_home();
    let mut records = load_records_for_home(&home).await?;
    records.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(records)
}

pub(crate) async fn approve_request(request_id: &str) -> Result<PairingRecord, String> {
    let home = savfox_home();
    let mut records = load_records_for_home(&home).await?;
    let now = now_epoch_ms();
    let Some(record) = records.iter_mut().find(|r| r.request_id == request_id) else {
        return Err(format!("pairing request '{request_id}' not found"));
    };
    record.status = PairingStatus::Approved;
    record.token = Some(uuid::Uuid::now_v7().to_string());
    record.updated_at = now;
    let cloned = record.clone();
    save_records_for_home(&home, &records).await?;
    Ok(cloned)
}

pub(crate) async fn reject_request(request_id: &str) -> Result<PairingRecord, String> {
    let home = savfox_home();
    let mut records = load_records_for_home(&home).await?;
    let now = now_epoch_ms();
    let Some(record) = records.iter_mut().find(|r| r.request_id == request_id) else {
        return Err(format!("pairing request '{request_id}' not found"));
    };
    record.status = PairingStatus::Rejected;
    record.updated_at = now;
    let cloned = record.clone();
    save_records_for_home(&home, &records).await?;
    Ok(cloned)
}

pub(crate) async fn verify_code(code: &str) -> Result<Option<PairingRecord>, String> {
    let home = savfox_home();
    let records = load_records_for_home(&home).await?;
    let found = records.into_iter().find(|r| {
        r.verification_code.eq_ignore_ascii_case(code)
            && matches!(r.status, PairingStatus::Pending | PairingStatus::Approved)
    });
    Ok(found)
}

pub(crate) async fn list_devices() -> Result<Vec<PairingRecord>, String> {
    let home = savfox_home();
    let records = load_records_for_home(&home).await?;
    let devices = records
        .into_iter()
        .filter(|r| matches!(r.status, PairingStatus::Approved | PairingStatus::Revoked))
        .collect();
    Ok(devices)
}

pub(crate) async fn approve_device(device_id: &str) -> Result<PairingRecord, String> {
    let home = savfox_home();
    let mut records = load_records_for_home(&home).await?;
    let now = now_epoch_ms();
    let Some(record) = records
        .iter_mut()
        .find(|r| r.device_id.as_deref() == Some(device_id))
    else {
        return Err(format!("device '{device_id}' not found"));
    };
    record.status = PairingStatus::Approved;
    if record.token.is_none() {
        record.token = Some(uuid::Uuid::now_v7().to_string());
    }
    record.updated_at = now;
    let cloned = record.clone();
    save_records_for_home(&home, &records).await?;
    Ok(cloned)
}

pub(crate) async fn reject_device(device_id: &str) -> Result<PairingRecord, String> {
    let home = savfox_home();
    let mut records = load_records_for_home(&home).await?;
    let now = now_epoch_ms();
    let Some(record) = records
        .iter_mut()
        .find(|r| r.device_id.as_deref() == Some(device_id))
    else {
        return Err(format!("device '{device_id}' not found"));
    };
    record.status = PairingStatus::Rejected;
    record.updated_at = now;
    let cloned = record.clone();
    save_records_for_home(&home, &records).await?;
    Ok(cloned)
}

pub(crate) async fn rotate_device_token(device_id: &str) -> Result<PairingRecord, String> {
    let home = savfox_home();
    let mut records = load_records_for_home(&home).await?;
    let now = now_epoch_ms();
    let Some(record) = records
        .iter_mut()
        .find(|r| r.device_id.as_deref() == Some(device_id))
    else {
        return Err(format!("device '{device_id}' not found"));
    };
    record.token = Some(uuid::Uuid::now_v7().to_string());
    record.updated_at = now;
    let cloned = record.clone();
    save_records_for_home(&home, &records).await?;
    Ok(cloned)
}

pub(crate) async fn revoke_device_token(device_id: &str) -> Result<PairingRecord, String> {
    let home = savfox_home();
    let mut records = load_records_for_home(&home).await?;
    let now = now_epoch_ms();
    let Some(record) = records
        .iter_mut()
        .find(|r| r.device_id.as_deref() == Some(device_id))
    else {
        return Err(format!("device '{device_id}' not found"));
    };
    record.status = PairingStatus::Revoked;
    record.token = None;
    record.updated_at = now;
    let cloned = record.clone();
    save_records_for_home(&home, &records).await?;
    Ok(cloned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pairing_roundtrip_for_home() {
        let home =
            std::env::temp_dir().join(format!("savfox-pairing-test-{}", uuid::Uuid::now_v7()));
        let created = create_request_for_home(&home, "node-a", Some("dev-a"), Some("phone"))
            .await
            .expect("create");
        assert_eq!(created.node_id, "node-a");

        let approved = {
            let request_id = created.request_id.clone();
            let mut records = load_records_for_home(&home).await.expect("load");
            let rec = records
                .iter_mut()
                .find(|r| r.request_id == request_id)
                .expect("record");
            rec.status = PairingStatus::Approved;
            rec.token = Some("tok".to_string());
            let out = rec.clone();
            save_records_for_home(&home, &records).await.expect("save");
            out
        };
        assert!(matches!(approved.status, PairingStatus::Approved));
        let listed = load_records_for_home(&home).await.expect("reload");
        assert_eq!(listed.len(), 1);

        let _ = tokio::fs::remove_dir_all(home).await;
    }
}
