use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use savfox_exec_policy::{Decision, blocking_append_prefix_rule};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::home_paths::exec_approval_policy_path;
use crate::json_store;

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
    #[serde(default)]
    migration: Option<LegacyPolicyMigration>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LegacyPolicyMigration {
    version: u32,
    migrated_at_ms: u64,
    migrated_rules: usize,
    rejected_rules: Vec<String>,
}

fn default_mode() -> String {
    "auto".to_owned()
}

impl Default for ExecApprovalPolicyStore {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            rules: Vec::new(),
            node_modes: HashMap::new(),
            node_rules: HashMap::new(),
            migration: None,
        }
    }
}

async fn load_store(savfox_home: &Path) -> Result<ExecApprovalPolicyStore, String> {
    json_store::load_json(
        &exec_approval_policy_path(savfox_home),
        "exec approval policy",
    )
    .await
}

async fn save_store(savfox_home: &Path, store: &ExecApprovalPolicyStore) -> Result<(), String> {
    json_store::save_json(
        &exec_approval_policy_path(savfox_home),
        store,
        "exec approval policy",
    )
    .await
}

pub(crate) async fn get_global(savfox_home: &Path) -> Result<Value, String> {
    let store = migrate_legacy_store(savfox_home).await?;
    let mapped_execution_mode = match store.mode.as_str() {
        "manual" => "interactive",
        // Legacy "auto" was ambiguous and could imply implicit approval.
        // It intentionally migrates to the non-interactive fail-closed mode.
        "auto" | "deny" => "unattended",
        _ => "unattended",
    };
    Ok(json!({
        "mode": store.mode,
        "rules": store.rules,
        "execution_mode": mapped_execution_mode,
        "deprecated": true,
        "read_only": true,
        "canonical_rule_source": "rules/default.rules",
        "migration": store.migration,
    }))
}

pub(crate) async fn set_global(
    _savfox_home: &Path,
    _mode: &str,
    _rules: Option<&Value>,
) -> Result<Value, String> {
    Err(
        "legacy Gateway approval policy is read-only; set the Agent execution policy and manage Core execpolicy rules instead"
            .to_owned(),
    )
}

pub(crate) async fn get_node(savfox_home: &Path, node_id: &str) -> Result<Value, String> {
    let store = migrate_legacy_store(savfox_home).await?;
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
        "deprecated": true,
        "read_only": true,
        "migration": store.migration,
    }))
}

pub(crate) async fn set_node(
    _savfox_home: &Path,
    _node_id: &str,
    _mode: &str,
    _rules: Option<&Value>,
) -> Result<Value, String> {
    Err(
        "legacy per-node approval policy is read-only; bind an Agent execution policy to the node and manage Core execpolicy rules instead"
            .to_owned(),
    )
}

async fn migrate_legacy_store(savfox_home: &Path) -> Result<ExecApprovalPolicyStore, String> {
    let mut store = load_store(savfox_home).await?;
    if store
        .migration
        .as_ref()
        .is_some_and(|migration| migration.version >= 1)
    {
        return Ok(store);
    }

    let mut candidates = Vec::new();
    let mut rejected_rules = Vec::new();
    for (index, rule) in store.rules.iter().enumerate() {
        match parse_legacy_rule(rule) {
            Ok(mut parsed) => candidates.append(&mut parsed),
            Err(error) => rejected_rules.push(format!("global rule {index}: {error}")),
        }
    }
    if !store.node_rules.is_empty() {
        rejected_rules.push(
            "per-node rules were not widened into global Core rules; migrate them after binding the node to an Agent"
                .to_owned(),
        );
    }

    let policy_path = savfox_home.join("rules").join("default.rules");
    let migrated_rules = candidates.len();
    tokio::task::spawn_blocking(move || {
        for (prefix, decision) in candidates {
            blocking_append_prefix_rule(&policy_path, &prefix, decision)
                .map_err(|error| error.to_string())?;
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|error| format!("legacy approval migration task failed: {error}"))??;

    store.migration = Some(LegacyPolicyMigration {
        version: 1,
        migrated_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        migrated_rules,
        rejected_rules,
    });
    save_store(savfox_home, &store).await?;
    Ok(store)
}

fn parse_legacy_rule(rule: &Value) -> Result<Vec<(Vec<String>, Decision)>, String> {
    let object = rule
        .as_object()
        .ok_or_else(|| "expected an object with exactly one allow/ask/deny key".to_owned())?;
    if object.len() != 1 {
        return Err("expected exactly one allow/ask/deny key".to_owned());
    }
    let (kind, commands) = object.iter().next().expect("non-empty object");
    let decision = match kind.as_str() {
        "allow" => Decision::Allow,
        "ask" | "prompt" => Decision::Prompt,
        "deny" | "forbidden" => Decision::Forbidden,
        _ => return Err(format!("unsupported decision '{kind}'")),
    };
    let commands = commands
        .as_array()
        .ok_or_else(|| format!("'{kind}' must be an array of command strings"))?;
    if commands.is_empty() {
        return Err(format!("'{kind}' command list is empty"));
    }
    commands
        .iter()
        .map(|command| {
            let command = command
                .as_str()
                .ok_or_else(|| format!("'{kind}' entries must be strings"))?;
            let prefix =
                shlex::split(command).ok_or_else(|| format!("cannot parse command '{command}'"))?;
            if prefix.is_empty() {
                return Err("empty command prefix".to_owned());
            }
            Ok((prefix, decision))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn legacy_policy_is_migrated_once_and_becomes_read_only() {
        let home = std::env::temp_dir().join(format!(
            "savfox-approval-policy-test-{}",
            uuid::Uuid::now_v7()
        ));
        let store = ExecApprovalPolicyStore {
            mode: "manual".to_owned(),
            rules: vec![
                json!({"allow": ["git status"]}),
                json!({"deny": ["rm -rf"]}),
            ],
            ..Default::default()
        };
        save_store(&home, &store).await.expect("write legacy store");
        let set_global_ret = get_global(&home).await.expect("migrate global");
        assert_eq!(
            set_global_ret
                .get("execution_mode")
                .and_then(|v| v.as_str()),
            Some("interactive")
        );
        assert_eq!(
            set_global_ret
                .pointer("/migration/migrated_rules")
                .and_then(Value::as_u64),
            Some(2)
        );
        let policy = tokio::fs::read_to_string(home.join("rules").join("default.rules"))
            .await
            .expect("read migrated Core policy");
        assert!(policy.contains(r#"pattern=["git", "status"], decision="allow""#));
        assert!(policy.contains(r#"pattern=["rm", "-rf"], decision="forbidden""#));
        assert!(set_global(&home, "auto", None).await.is_err());
        assert!(set_node(&home, "node-a", "deny", None).await.is_err());

        let _ = tokio::fs::remove_dir_all(home).await;
    }
}
