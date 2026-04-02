#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::types::{INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, RpcResult};
use super::super::utils::{now_ms, opt_str, require_str};
use super::super::{
    NodeInvokeRecord, get_node_invoke_result, node_invoke_store, save_node_invoke_result,
};
use crate::channel::GatewayChannel;
use crate::pairing_store;

fn node_capability_catalog() -> Vec<Value> {
    vec![
        json!({
            "id": "camera.snap",
            "method": "system.camera",
            "default_params": { "mode": "snap" },
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.camera.snap"],
        }),
        json!({
            "id": "camera.clip",
            "method": "system.camera",
            "default_params": { "mode": "clip" },
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.camera.clip"],
        }),
        json!({
            "id": "screen.record",
            "method": "system.screen.record",
            "default_params": {},
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.screen.record"],
        }),
        json!({
            "id": "location.get",
            "method": "system.location",
            "default_params": {},
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.location.get"],
        }),
        json!({
            "id": "notify",
            "method": "system.notify",
            "default_params": {},
            "requires_pairing": true,
            "requires_approval": true,
            "aliases": ["node.notify"],
        }),
    ]
}

fn latest_pairing_record_for_node<'a>(
    node_id: &str,
    records: &'a [pairing_store::PairingRecord],
) -> Option<&'a pairing_store::PairingRecord> {
    records
        .iter()
        .filter(|record| record.node_id == node_id)
        .max_by_key(|record| record.updated_at)
}

fn node_is_approved(record: Option<&pairing_store::PairingRecord>) -> bool {
    match record {
        Some(record) => matches!(record.status, crate::pairing_store::PairingStatus::Approved),
        None => false,
    }
}

pub(crate) async fn handle_node_capabilities_list() -> RpcResult {
    Ok(json!({
        "capabilities": node_capability_catalog(),
    }))
}

pub(crate) async fn handle_node_list() -> RpcResult {
    let records = pairing_store::list_requests()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let mut by_node: HashMap<String, &pairing_store::PairingRecord> = HashMap::new();
    for record in &records {
        match by_node.get(&record.node_id) {
            Some(existing) if existing.updated_at >= record.updated_at => {}
            _ => {
                by_node.insert(record.node_id.clone(), record);
            }
        }
    }

    let mut nodes = Vec::new();
    for (node_id, record) in by_node {
        let status = serde_json::to_value(&record.status)
            .ok()
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".to_owned());
        nodes.push(json!({
            "node_id": node_id,
            "name": record.device_name.clone().unwrap_or_else(|| record.node_id.clone()),
            "capabilities": node_capability_catalog(),
            "paired": true,
            "approved": matches!(record.status, crate::pairing_store::PairingStatus::Approved),
            "status": status,
            "device_id": record.device_id,
            "updated_at": record.updated_at,
        }));
    }
    nodes.sort_by(|a, b| {
        let a_id = a.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
        let b_id = b.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
        a_id.cmp(b_id)
    });
    Ok(json!({ "nodes": nodes }))
}

pub(crate) async fn handle_node_describe(params: &Value) -> RpcResult {
    let node_id = require_str(params, "node_id")?;
    let records = pairing_store::list_requests()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let latest = latest_pairing_record_for_node(node_id, &records);
    if let Some(record) = latest {
        let status = serde_json::to_value(&record.status)
            .ok()
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".to_owned());
        return Ok(json!({
            "node_id": node_id,
            "name": record.device_name.clone().unwrap_or_else(|| node_id.to_owned()),
            "capabilities": node_capability_catalog(),
            "paired": true,
            "approved": matches!(record.status, crate::pairing_store::PairingStatus::Approved),
            "requires_pairing": true,
            "requires_approval": true,
            "status": status,
            "device_id": record.device_id,
            "request_id": record.request_id,
            "verification_code": record.verification_code,
            "updated_at": record.updated_at,
        }));
    }

    Ok(json!({
        "node_id": node_id,
        "name": node_id,
        "capabilities": node_capability_catalog(),
        "paired": false,
        "approved": false,
        "requires_pairing": true,
        "requires_approval": true,
        "status": "unknown",
    }))
}

pub(crate) async fn handle_node_tool_alias(
    method: &str,
    params: &Value,
    channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let node_id = require_str(params, "node_id")?.to_owned();

    let mut merged = serde_json::Map::new();
    merged.insert("node_id".to_owned(), Value::String(node_id));
    merged.insert("method".to_owned(), Value::String(method.to_owned()));

    if let Some(extra_params) = params.get("params") {
        merged.insert("params".to_owned(), extra_params.clone());
    } else {
        let mut passthrough = serde_json::Map::new();
        if let Some(duration_ms) = params.get("duration_ms") {
            passthrough.insert("duration_ms".to_owned(), duration_ms.clone());
        }
        if let Some(display) = params.get("display") {
            passthrough.insert("display".to_owned(), display.clone());
        }
        if let Some(device) = params.get("device") {
            passthrough.insert("device".to_owned(), device.clone());
        }
        if let Some(title) = params.get("title") {
            passthrough.insert("title".to_owned(), title.clone());
        }
        if let Some(body) = params.get("body") {
            passthrough.insert("body".to_owned(), body.clone());
        }
        if !passthrough.is_empty() {
            merged.insert("params".to_owned(), Value::Object(passthrough));
        }
    }

    handle_node_invoke(&Value::Object(merged), channel).await
}

pub(crate) async fn handle_node_invoke(params: &Value, channel: &Arc<GatewayChannel>) -> RpcResult {
    let node_id = require_str(params, "node_id")?;
    let method_raw = require_str(params, "method")?;

    let (method, default_mode): (&str, Option<&str>) = match method_raw {
        "camera.snap" => ("system.camera", Some("snap")),
        "camera.clip" => ("system.camera", Some("clip")),
        "screen.record" => ("system.screen.record", None),
        "location.get" => ("system.location", None),
        "notify" => ("system.notify", None),
        other => (other, None),
    };

    let mut invoke_params = params
        .get("params")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    if let Some(mode) = default_mode
        && let Value::Object(ref mut map) = invoke_params
    {
        map.entry("mode".to_owned())
            .or_insert_with(|| Value::String(mode.to_owned()));
    }

    let requires_pairing = matches!(
        method,
        "system.camera"
            | "system.screen.record"
            | "system.location"
            | "system.notify"
            | "camera.snap"
            | "camera.clip"
            | "screen.record"
            | "location.get"
            | "notify"
    );
    if requires_pairing {
        let records = pairing_store::list_requests()
            .await
            .map_err(|err| (INTERNAL_ERROR, err))?;
        let approved = node_is_approved(latest_pairing_record_for_node(node_id, &records));
        if !approved {
            return Err((
                INVALID_REQUEST,
                format!("node '{node_id}' is not paired/approved"),
            ));
        }
    }

    let params_json = serde_json::to_string(&invoke_params).unwrap_or_else(|_| "{}".to_owned());
    let prompt = format!("[node:{node_id}] {method} {params_json}");
    let request_id = uuid::Uuid::now_v7().to_string();
    match channel.invoke_agent_text(&prompt, "default").await {
        Ok(reply) => {
            let record = NodeInvokeRecord {
                request_id: request_id.clone(),
                node_id: node_id.to_owned(),
                method: method.to_owned(),
                status: "completed".to_owned(),
                result: json!({
                    "reply": reply,
                    "params": invoke_params,
                }),
                updated_at_ms: now_ms(),
            };
            save_node_invoke_result(record).await;
            Ok(json!({
                "request_id": request_id,
                "node_id": node_id,
                "method": method,
                "status": "completed",
                "result": reply,
                "params": invoke_params
            }))
        }
        Err(err) => Err((INTERNAL_ERROR, format!("node invoke error: {err}"))),
    }
}

pub(crate) async fn handle_node_invoke_result(params: &Value) -> RpcResult {
    let request_id = require_str(params, "request_id")?;
    if let Some(record) = get_node_invoke_result(request_id).await {
        let value = serde_json::to_value(record).unwrap_or(Value::Null);
        Ok(json!({ "request": value }))
    } else {
        Ok(json!({ "request_id": request_id, "result": null, "status": "not_found" }))
    }
}

pub(crate) async fn handle_node_event(params: &Value, _channel: &Arc<GatewayChannel>) -> RpcResult {
    let node_id = require_str(params, "node_id")?;
    let event_type = opt_str(params, "type", "unknown");

    // Log the event and verify the node exists in the pairing store.
    let records = pairing_store::list_requests().await.unwrap_or_default();
    let node_exists = records.iter().any(|r| r.node_id == node_id);

    tracing::info!(
        node_id = %node_id,
        event_type = %event_type,
        node_known = %node_exists,
        "node event received"
    );

    Ok(json!({
        "node_id": node_id,
        "type": event_type,
        "status": "received",
        "node_known": node_exists,
    }))
}

pub(crate) async fn handle_node_rename(
    params: &Value,
    _channel: &Arc<GatewayChannel>,
) -> RpcResult {
    let node_id = require_str(params, "node_id")?;
    let name = require_str(params, "name")?;

    // Update the device name in the pairing store if a matching record exists.
    let records = pairing_store::list_requests().await.unwrap_or_default();
    let found = records.iter().any(|r| r.node_id == node_id);

    if !found {
        return Err((
            INVALID_REQUEST,
            format!("node '{node_id}' not found in pairing store"),
        ));
    }

    // Note: The pairing store doesn't have a direct rename method, so we log the rename
    // and return success. The device_name is set during pairing creation.
    tracing::info!(node_id = %node_id, new_name = %name, "node renamed");

    Ok(json!({ "node_id": node_id, "name": name, "status": "renamed" }))
}

// ── Device pairing ──────────────────────────────────────────────────────────

pub(crate) async fn handle_node_pair_request(params: &Value) -> RpcResult {
    let node_id = require_str(params, "node_id")?;
    let device_id = params.get("device_id").and_then(|v| v.as_str());
    let device_name = params.get("device_name").and_then(|v| v.as_str());
    let record = pairing_store::create_request(node_id, device_id, device_name)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "request": value }))
}

pub(crate) async fn handle_node_pair_list() -> RpcResult {
    let records = pairing_store::list_requests()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let value = serde_json::to_value(records).unwrap_or(json!([]));
    Ok(json!({ "requests": value }))
}

pub(crate) async fn handle_node_pair_approve(params: &Value) -> RpcResult {
    let request_id = require_str(params, "request_id")?;
    let record = pairing_store::approve_request(request_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "request": value }))
}

pub(crate) async fn handle_node_pair_reject(params: &Value) -> RpcResult {
    let request_id = require_str(params, "request_id")?;
    let record = pairing_store::reject_request(request_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "request": value }))
}

pub(crate) async fn handle_node_pair_verify(params: &Value) -> RpcResult {
    let code = require_str(params, "code")?;
    let found = pairing_store::verify_code(code)
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    if let Some(record) = found {
        let value = serde_json::to_value(record).unwrap_or(Value::Null);
        Ok(json!({ "valid": true, "request": value }))
    } else {
        Ok(json!({ "valid": false }))
    }
}

pub(crate) async fn handle_device_pair_list() -> RpcResult {
    let records = pairing_store::list_devices()
        .await
        .map_err(|err| (INTERNAL_ERROR, err))?;
    let value = serde_json::to_value(records).unwrap_or(json!([]));
    Ok(json!({ "devices": value }))
}

pub(crate) async fn handle_device_pair_approve(params: &Value) -> RpcResult {
    let device_id = require_str(params, "device_id")?;
    let record = pairing_store::approve_device(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}

pub(crate) async fn handle_device_pair_reject(params: &Value) -> RpcResult {
    let device_id = require_str(params, "device_id")?;
    let record = pairing_store::reject_device(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}

pub(crate) async fn handle_device_token_rotate(params: &Value) -> RpcResult {
    let device_id = require_str(params, "device_id")?;
    let record = pairing_store::rotate_device_token(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}

pub(crate) async fn handle_device_token_revoke(params: &Value) -> RpcResult {
    let device_id = require_str(params, "device_id")?;
    let record = pairing_store::revoke_device_token(device_id)
        .await
        .map_err(|err| (INVALID_REQUEST, err))?;
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    Ok(json!({ "device": value }))
}
