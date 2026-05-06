use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::json_store;

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

/// Length of the verification code presented to the operator. 8 chars
/// of Crockford-style base32 = 40 bits of randomness, which is plenty
/// for short-lived codes (we still rely on rate limiting upstream of
/// the verifier to make brute-force impractical).
const VERIFICATION_CODE_LEN: usize = 8;

/// Crockford-style base32 alphabet (`0–9 A–H J K M N P–T V–Z`,
/// no `I L O U`). These characters are unambiguous when typed by a
/// human under low light.
const CROCKFORD_BASE32_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Generate a fresh verification code from cryptographic randomness.
///
/// Replaces the previous implementation that used the first 6 chars of
/// a UUIDv7 — UUIDv7 begins with a millisecond timestamp, so an
/// attacker who knows roughly when a pairing was initiated can guess
/// the prefix with near-zero entropy (S4 in the security review).
fn make_verification_code() -> String {
    let mut out = String::with_capacity(VERIFICATION_CODE_LEN);
    for _ in 0..VERIFICATION_CODE_LEN {
        // Free-function form avoids needing the `RngExt` trait in scope;
        // we just need uniform [0, 32) which `random_range` from
        // `rand::random_range` handles directly.
        let idx: usize = rand::random_range(0..CROCKFORD_BASE32_ALPHABET.len());
        out.push(char::from(CROCKFORD_BASE32_ALPHABET[idx]));
    }
    out
}

/// Constant-time, case-insensitive comparison of two verification codes.
///
/// Both inputs are normalised (trim + uppercase) before comparison so the
/// operator can type the code with any case / surrounding whitespace; the
/// comparison itself uses [`subtle::ConstantTimeEq`] to prevent leaking
/// byte-by-byte match information through response-time differences.
fn constant_time_code_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    let a = a.trim().to_ascii_uppercase();
    let b = b.trim().to_ascii_uppercase();
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

async fn load_records_for_home(home: &PathBuf) -> Result<Vec<PairingRecord>, String> {
    json_store::load_json(&store_path(home), "pairing store").await
}

async fn save_records_for_home(home: &PathBuf, records: &[PairingRecord]) -> Result<(), String> {
    json_store::save_json(&store_path(home), records, "pairing store").await
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
    let now = json_store::now_ms();
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
    records.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
    Ok(records)
}

pub(crate) async fn approve_request(request_id: &str) -> Result<PairingRecord, String> {
    let home = savfox_home();
    let mut records = load_records_for_home(&home).await?;
    let now = json_store::now_ms();
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
    let now = json_store::now_ms();
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
    // Walk every record so the work is independent of which (if any)
    // matched — together with `constant_time_code_eq` this prevents the
    // verifier from leaking the position of the first matching prefix.
    let mut matched: Option<PairingRecord> = None;
    for record in records {
        if matches!(
            record.status,
            PairingStatus::Pending | PairingStatus::Approved
        ) && constant_time_code_eq(&record.verification_code, code)
            && matched.is_none()
        {
            matched = Some(record);
        }
    }
    Ok(matched)
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
    let now = json_store::now_ms();
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
    let now = json_store::now_ms();
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
    let now = json_store::now_ms();
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
    let now = json_store::now_ms();
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
            rec.token = Some("tok".to_owned());
            let out = rec.clone();
            save_records_for_home(&home, &records).await.expect("save");
            out
        };
        assert!(matches!(approved.status, PairingStatus::Approved));
        let listed = load_records_for_home(&home).await.expect("reload");
        assert_eq!(listed.len(), 1);

        let _ = tokio::fs::remove_dir_all(home).await;
    }

    #[test]
    fn verification_code_has_expected_shape() {
        let code = make_verification_code();
        assert_eq!(code.len(), VERIFICATION_CODE_LEN);
        for ch in code.chars() {
            assert!(
                CROCKFORD_BASE32_ALPHABET.contains(&(ch as u8)),
                "char {ch:?} not in alphabet"
            );
            // Forbid the easily-confused glyphs the alphabet excludes.
            assert!(!matches!(ch, 'I' | 'L' | 'O' | 'U'));
        }
    }

    #[test]
    fn verification_codes_are_distinct() {
        // 40 bits of randomness → collisions in 100 samples are negligible.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            assert!(seen.insert(make_verification_code()));
        }
    }

    #[test]
    fn constant_time_eq_accepts_case_and_whitespace_variants() {
        assert!(constant_time_code_eq("ABC123", "abc123"));
        assert!(constant_time_code_eq("  ABC123  ", "ABC123"));
        assert!(!constant_time_code_eq("ABC123", "ABC124"));
        assert!(!constant_time_code_eq("ABC123", "ABC1234"));
        assert!(!constant_time_code_eq("ABC123", ""));
    }
}
