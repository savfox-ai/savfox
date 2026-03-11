use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dm_policy::{DmPolicyMode, DmPolicyStore};

#[derive(Debug, Clone)]
pub(crate) struct ConfigFile {
    pub format: String,
    pub path: PathBuf,
    pub value: Value,
}

/// Load config from `{savfox_home}/config.{toml,json,yaml,yml}`.
pub(crate) async fn load_config_document(savfox_home: &Path) -> Result<ConfigFile, String> {
    let candidates = [
        ("toml", savfox_home.join("config.toml")),
        ("json", savfox_home.join("config.json")),
        ("yaml", savfox_home.join("config.yaml")),
        ("yaml", savfox_home.join("config.yml")),
    ];

    for (format, path) in candidates {
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            continue;
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;

        let value = match format {
            "json" => serde_json::from_str::<Value>(&content)
                .map_err(|e| format!("invalid JSON in {}: {e}", path.display()))?,
            "yaml" => serde_yaml::from_str::<Value>(&content)
                .map_err(|e| format!("invalid YAML in {}: {e}", path.display()))?,
            "toml" => {
                let toml_value: toml::Value = content
                    .parse()
                    .map_err(|e| format!("invalid TOML in {}: {e}", path.display()))?;
                serde_json::to_value(toml_value)
                    .map_err(|e| format!("TOML->JSON conversion failed: {e}"))?
            }
            _ => Value::Object(serde_json::Map::new()),
        };

        return Ok(ConfigFile {
            format: format.to_string(),
            path,
            value,
        });
    }

    Ok(ConfigFile {
        format: "toml".to_string(),
        path: savfox_home.join("config.toml"),
        value: Value::Object(serde_json::Map::new()),
    })
}

pub(crate) fn serialize_config_value(value: &Value, format: &str) -> Result<String, String> {
    match format {
        "json" => serde_json::to_string_pretty(value)
            .map_err(|e| format!("JSON serialization failed: {e}")),
        "yaml" => {
            serde_yaml::to_string(value).map_err(|e| format!("YAML serialization failed: {e}"))
        }
        "toml" => {
            let toml_value = savfox_utils::json_to_toml::json_to_toml(value.clone());
            toml::to_string(&toml_value).map_err(|e| format!("TOML serialization failed: {e}"))
        }
        other => Err(format!("unsupported format: {other}")),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecurityCheckStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAuditCheck {
    pub name: String,
    pub status: SecurityCheckStatus,
    pub details: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct SecurityAuditSummary {
    pub critical: u32,
    pub warn: u32,
    pub info: u32,
    pub score: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAuditReport {
    pub generated_at: u64,
    pub summary: SecurityAuditSummary,
    pub checks: Vec<SecurityAuditCheck>,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone)]
struct GatewaySurfaceConfig {
    host: IpAddr,
    port: u16,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    webhook_enabled: bool,
    webhook_secret: Option<String>,
}

impl Default for GatewaySurfaceConfig {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 18881,
            tls_cert: None,
            tls_key: None,
            webhook_enabled: false,
            webhook_secret: None,
        }
    }
}

pub async fn run_audit(savfox_home: &Path) -> SecurityAuditReport {
    let mut checks = Vec::new();

    checks.push(check_dm_policy(savfox_home).await);

    match load_config_document(savfox_home).await {
        Ok(doc) => {
            let gateway = parse_gateway_surface(&doc.value);
            checks.push(check_hook_exposure(&gateway));
            checks.push(check_credential_exposure(&doc.value));
            checks.push(check_tls_enforcement(&gateway));
            checks.push(check_open_port_exposure(&gateway));
        }
        Err(err) => {
            checks.push(SecurityAuditCheck {
                name: "config_integrity".to_string(),
                status: SecurityCheckStatus::Fail,
                details: err,
                suggestion: Some(
                    "Run `savfox config validate` and fix config parse errors.".into(),
                ),
            });
        }
    }

    let summary = summarize(&checks);
    let suggestions = checks
        .iter()
        .filter_map(|c| c.suggestion.clone())
        .collect::<Vec<_>>();

    SecurityAuditReport {
        generated_at: now_secs(),
        summary,
        checks,
        suggestions,
    }
}

fn parse_gateway_surface(config: &Value) -> GatewaySurfaceConfig {
    let mut parsed = GatewaySurfaceConfig::default();
    let Some(gateway) = config.get("gateway").and_then(|v| v.as_object()) else {
        return parsed;
    };

    if let Some(host) = gateway.get("host").and_then(|v| v.as_str()) {
        if let Ok(ip) = host.parse::<IpAddr>() {
            parsed.host = ip;
        }
    }

    if let Some(port) = gateway.get("port").and_then(|v| v.as_u64()) {
        if (1..=u16::MAX as u64).contains(&port) {
            parsed.port = port as u16;
        }
    }

    parsed.tls_cert = gateway
        .get("tls_cert")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(ToOwned::to_owned);
    parsed.tls_key = gateway
        .get("tls_key")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(ToOwned::to_owned);

    if let Some(webhook) = gateway
        .get("bridges")
        .and_then(|v| v.get("webhook"))
        .and_then(|v| v.as_object())
    {
        parsed.webhook_enabled = webhook
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        parsed.webhook_secret = webhook
            .get("secret")
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .map(ToOwned::to_owned);
    }

    parsed
}

async fn check_dm_policy(savfox_home: &Path) -> SecurityAuditCheck {
    let store = DmPolicyStore::new(savfox_home);
    store.load().await;
    let policies = store.list_policies().await;

    let mut risky_channels = Vec::new();
    for (channel, policy) in policies {
        if policy.mode == DmPolicyMode::Open && policy.allowlist.is_empty() {
            risky_channels.push(channel);
        }
    }

    if risky_channels.is_empty() {
        SecurityAuditCheck {
            name: "dm_policy".to_string(),
            status: SecurityCheckStatus::Pass,
            details: "No open DM policy without allowlist was detected.".to_string(),
            suggestion: None,
        }
    } else {
        let channels = risky_channels.join(", ");
        SecurityAuditCheck {
            name: "dm_policy".to_string(),
            status: SecurityCheckStatus::Warn,
            details: format!("Open DM policy without allowlist on channel(s): {channels}"),
            suggestion: Some(
                "Use `dm.policy.set` to switch to `pairing`/`closed` or configure an allowlist."
                    .to_string(),
            ),
        }
    }
}

fn check_hook_exposure(gateway: &GatewaySurfaceConfig) -> SecurityAuditCheck {
    if gateway.webhook_enabled && gateway.webhook_secret.is_none() {
        return SecurityAuditCheck {
            name: "hooks_authentication".to_string(),
            status: SecurityCheckStatus::Warn,
            details: "Generic webhook bridge is enabled without a signing secret.".to_string(),
            suggestion: Some(
                "Set `gateway.bridges.webhook.secret` (or WEBHOOK_SIGNING_SECRET).".to_string(),
            ),
        };
    }

    SecurityAuditCheck {
        name: "hooks_authentication".to_string(),
        status: SecurityCheckStatus::Pass,
        details: "Hook endpoints require bearer auth; webhook signing secret is configured or webhook bridge is disabled."
            .to_string(),
        suggestion: None,
    }
}

fn check_credential_exposure(config: &Value) -> SecurityAuditCheck {
    let mut exposed = Vec::new();
    collect_inline_secrets(config, "", None, &mut exposed);

    if exposed.is_empty() {
        return SecurityAuditCheck {
            name: "credentials".to_string(),
            status: SecurityCheckStatus::Pass,
            details: "No obvious inline credentials were detected in config.".to_string(),
            suggestion: None,
        };
    }

    let preview = exposed
        .iter()
        .take(6)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let more = exposed.len().saturating_sub(6);
    let suffix = if more > 0 {
        format!(" (+{more} more)")
    } else {
        String::new()
    };

    SecurityAuditCheck {
        name: "credentials".to_string(),
        status: SecurityCheckStatus::Fail,
        details: format!("Inline secret-like values found at: {preview}{suffix}"),
        suggestion: Some(
            "Move secrets to env vars and keep config values as env references. Use `savfox config export --redacted` before sharing."
                .to_string(),
        ),
    }
}

fn check_tls_enforcement(gateway: &GatewaySurfaceConfig) -> SecurityAuditCheck {
    let tls_enabled = gateway.tls_cert.is_some() && gateway.tls_key.is_some();
    let partial_tls = gateway.tls_cert.is_some() ^ gateway.tls_key.is_some();
    let external = !gateway.host.is_loopback();

    if partial_tls {
        return SecurityAuditCheck {
            name: "tls".to_string(),
            status: SecurityCheckStatus::Fail,
            details: "TLS is partially configured (both tls_cert and tls_key are required)."
                .to_string(),
            suggestion: Some("Set both `gateway.tls_cert` and `gateway.tls_key`.".to_string()),
        };
    }

    if external && !tls_enabled {
        return SecurityAuditCheck {
            name: "tls".to_string(),
            status: SecurityCheckStatus::Fail,
            details: format!(
                "Gateway is bound to external host {}:{} without TLS.",
                gateway.host, gateway.port
            ),
            suggestion: Some(
                "Enable TLS or bind gateway.host to loopback (127.0.0.1 / ::1).".to_string(),
            ),
        };
    }

    SecurityAuditCheck {
        name: "tls".to_string(),
        status: SecurityCheckStatus::Pass,
        details: if tls_enabled {
            "TLS is configured.".to_string()
        } else {
            "Gateway is loopback-only; plaintext transport is local-only.".to_string()
        },
        suggestion: None,
    }
}

fn check_open_port_exposure(gateway: &GatewaySurfaceConfig) -> SecurityAuditCheck {
    let tls_enabled = gateway.tls_cert.is_some() && gateway.tls_key.is_some();

    if gateway.host.is_unspecified() {
        return SecurityAuditCheck {
            name: "open_ports".to_string(),
            status: if tls_enabled {
                SecurityCheckStatus::Warn
            } else {
                SecurityCheckStatus::Fail
            },
            details: format!(
                "Gateway listens on all interfaces ({}:{}).",
                gateway.host, gateway.port
            ),
            suggestion: Some(
                "Restrict host to loopback when possible; otherwise enforce TLS and firewall rules."
                    .to_string(),
            ),
        };
    }

    if gateway.host.is_loopback() {
        return SecurityAuditCheck {
            name: "open_ports".to_string(),
            status: SecurityCheckStatus::Pass,
            details: format!(
                "Gateway is bound to loopback only ({}:{}).",
                gateway.host, gateway.port
            ),
            suggestion: None,
        };
    }

    SecurityAuditCheck {
        name: "open_ports".to_string(),
        status: SecurityCheckStatus::Warn,
        details: format!(
            "Gateway is externally reachable on {}:{}.",
            gateway.host, gateway.port
        ),
        suggestion: Some("Review network ACLs and expose only required ports.".to_string()),
    }
}

fn summarize(checks: &[SecurityAuditCheck]) -> SecurityAuditSummary {
    let critical = checks
        .iter()
        .filter(|c| c.status == SecurityCheckStatus::Fail)
        .count() as u32;
    let warn = checks
        .iter()
        .filter(|c| c.status == SecurityCheckStatus::Warn)
        .count() as u32;
    let info = checks
        .iter()
        .filter(|c| c.status == SecurityCheckStatus::Pass)
        .count() as u32;

    let score = (100_i32 - (critical as i32 * 30) - (warn as i32 * 10)).max(0) as u32;

    SecurityAuditSummary {
        critical,
        warn,
        info,
        score,
    }
}

fn collect_inline_secrets(
    value: &Value,
    path: &str,
    key_hint: Option<&str>,
    exposed: &mut Vec<String>,
) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let next_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                collect_inline_secrets(child, &next_path, Some(key), exposed);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let next_path = if path.is_empty() {
                    format!("[{index}]")
                } else {
                    format!("{path}[{index}]")
                };
                collect_inline_secrets(child, &next_path, key_hint, exposed);
            }
        }
        Value::String(raw) => {
            let Some(key) = key_hint else {
                return;
            };
            if is_sensitive_key(key)
                && !is_env_reference_key(key)
                && is_secret_literal(raw)
                && !path.to_ascii_lowercase().contains(".headers.")
            {
                exposed.push(path.to_string());
            }
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    let patterns = [
        "api_key",
        "token",
        "secret",
        "password",
        "private_key",
        "access_key",
        "bearer",
    ];
    patterns.iter().any(|p| key.contains(p))
}

fn is_env_reference_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("env")
        || key.ends_with("_path")
        || key.contains("template")
        || key.contains("pattern")
}

fn is_secret_literal(raw: &str) -> bool {
    let value = raw.trim();
    if value.is_empty() {
        return false;
    }

    if value.starts_with("${") && value.ends_with('}') {
        return false;
    }

    if value.contains("***")
        || value.contains("<redacted>")
        || value.contains("REDACTED")
        || value == "xxxxx"
    {
        return false;
    }

    if looks_like_env_name(value) {
        return false;
    }

    true
}

fn looks_like_env_name(value: &str) -> bool {
    if value.len() < 4 {
        return false;
    }
    value
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_inline_secret_literals() {
        let config = serde_json::json!({
            "gateway": {
                "token": "super-secret-token"
            },
            "models": {
                "gpt4": {
                    "api_key": "sk-live-123456"
                }
            }
        });

        let check = check_credential_exposure(&config);
        assert_eq!(check.status, SecurityCheckStatus::Fail);
        assert!(check.details.contains("gateway.token"));
        assert!(check.details.contains("models.gpt4.api_key"));
    }

    #[test]
    fn ignores_env_reference_values() {
        let config = serde_json::json!({
            "models": {
                "gpt4": {
                    "env_key": "OPENAI_API_KEY",
                    "api_key": "${OPENAI_API_KEY}"
                }
            }
        });

        let check = check_credential_exposure(&config);
        assert_eq!(check.status, SecurityCheckStatus::Pass);
    }

    #[test]
    fn summary_score_counts_fail_and_warn() {
        let checks = vec![
            SecurityAuditCheck {
                name: "a".to_string(),
                status: SecurityCheckStatus::Pass,
                details: String::new(),
                suggestion: None,
            },
            SecurityAuditCheck {
                name: "b".to_string(),
                status: SecurityCheckStatus::Warn,
                details: String::new(),
                suggestion: None,
            },
            SecurityAuditCheck {
                name: "c".to_string(),
                status: SecurityCheckStatus::Fail,
                details: String::new(),
                suggestion: None,
            },
        ];

        let summary = summarize(&checks);
        assert_eq!(summary.critical, 1);
        assert_eq!(summary.warn, 1);
        assert_eq!(summary.info, 1);
        assert_eq!(summary.score, 60);
    }
}
