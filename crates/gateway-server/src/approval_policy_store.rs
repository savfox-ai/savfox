use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExecApprovalPolicyStore {
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default)]
    rules: Vec<Value>,
    #[serde(default)]
    node_modes: HashMap<String, String>,
    #[serde(default)]
    node_rules: HashMap<String, Vec<Value>>,
}

fn default_mode() -> String {
    "auto".to_string()
}

impl Default for ExecApprovalPolicyStore {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            rules: Vec::new(),
            node_modes: HashMap::new(),
            node_rules: HashMap::new(),
        }
    }
}

fn store_path(savfox_home: &Path) -> PathBuf {
    savfox_home
        .join("gateway")
        .join("exec-approval-policy.json")
}

async fn load_store(savfox_home: &Path) -> Result<ExecApprovalPolicyStore, String> {
    let path = store_path(savfox_home);
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(_) => return Ok(ExecApprovalPolicyStore::default()),
    };
    serde_json::from_str(&content)
        .map_err(|err| format!("failed to parse exec approval policy: {err}"))
}

async fn save_store(savfox_home: &Path, store: &ExecApprovalPolicyStore) -> Result<(), String> {
    let path = store_path(savfox_home);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("failed to create exec approval policy dir: {err}"))?;
    }
    let content = serde_json::to_string_pretty(store)
        .map_err(|err| format!("failed to serialize exec approval policy: {err}"))?;
    tokio::fs::write(path, content)
        .await
        .map_err(|err| format!("failed to write exec approval policy: {err}"))
}

pub(crate) async fn get_global(savfox_home: &Path) -> Result<Value, String> {
    let store = load_store(savfox_home).await?;
    Ok(json!({
        "mode": store.mode,
        "rules": store.rules,
    }))
}

pub(crate) async fn set_global(
    savfox_home: &Path,
    mode: &str,
    rules: Option<&Value>,
) -> Result<Value, String> {
    if !matches!(mode, "auto" | "manual" | "deny") {
        return Err(format!("invalid mode: {mode}"));
    }
    let mut store = load_store(savfox_home).await?;
    store.mode = mode.to_owned();
    if let Some(Value::Array(items)) = rules {
        store.rules = items.clone();
    }
    save_store(savfox_home, &store).await?;
    Ok(json!({
        "mode": store.mode,
        "rules": store.rules,
        "status": "set",
    }))
}

pub(crate) async fn get_node(savfox_home: &Path, node_id: &str) -> Result<Value, String> {
    let store = load_store(savfox_home).await?;
    let mode = store
        .node_modes
        .get(node_id)
        .cloned()
        .unwrap_or_else(|| store.mode.clone());
    let rules = store
        .node_rules
        .get(node_id)
        .cloned()
        .unwrap_or_else(|| store.rules.clone());
    Ok(json!({
        "node_id": node_id,
        "mode": mode,
        "rules": rules,
    }))
}

pub(crate) async fn set_node(
    savfox_home: &Path,
    node_id: &str,
    mode: &str,
    rules: Option<&Value>,
) -> Result<Value, String> {
    if node_id.is_empty() {
        return Err("missing 'node_id' parameter".to_string());
    }
    if !matches!(mode, "auto" | "manual" | "deny") {
        return Err(format!("invalid mode: {mode}"));
    }
    let mut store = load_store(savfox_home).await?;
    store.node_modes.insert(node_id.to_owned(), mode.to_owned());
    if let Some(Value::Array(items)) = rules {
        store.node_rules.insert(node_id.to_owned(), items.clone());
    }
    save_store(savfox_home, &store).await?;
    let result = get_node(savfox_home, node_id).await?;
    Ok(json!({
        "node_id": result.get("node_id").cloned().unwrap_or(Value::Null),
        "mode": result.get("mode").cloned().unwrap_or(Value::String(mode.to_owned())),
        "rules": result.get("rules").cloned().unwrap_or(Value::Array(Vec::new())),
        "status": "set",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approval_policy_roundtrip() {
        let home = std::env::temp_dir().join(format!(
            "savfox-approval-policy-test-{}",
            uuid::Uuid::now_v7()
        ));
        let set_global_ret = set_global(&home, "manual", Some(&json!([{"allow": ["git status"]}])))
            .await
            .expect("set global");
        assert_eq!(
            set_global_ret.get("mode").and_then(|v| v.as_str()),
            Some("manual")
        );

        let set_node_ret = set_node(
            &home,
            "node-a",
            "deny",
            Some(&json!([{"deny": ["rm -rf"]}])),
        )
        .await
        .expect("set node");
        assert_eq!(
            set_node_ret.get("mode").and_then(|v| v.as_str()),
            Some("deny")
        );

        let node_ret = get_node(&home, "node-a").await.expect("get node");
        assert_eq!(
            node_ret.get("node_id").and_then(|v| v.as_str()),
            Some("node-a")
        );

        let _ = tokio::fs::remove_dir_all(home).await;
    }
}
